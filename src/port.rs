use std::collections::{BTreeSet, HashMap, HashSet};
use std::process::Command;
use std::time::Duration;

use crate::process::{ProcessSnapshot, command_basename};
use crate::subprocess;

/// Deadline for the `lsof` listening-port probe; it runs on the port-scan
/// worker thread (every 10s) and must never hang there.
const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Default, Clone)]
pub struct PaneProcessSnapshot {
    /// Listening ports found in each scanned pane's process tree. Empty
    /// for a pane whose tree held no listener — but also for *every*
    /// pane when the lsof probe failed, which consumers must hold over
    /// one sweep instead of reading as "the listener died" (see
    /// `AppState::apply_process_snapshot`).
    pub ports_by_pane: HashMap<String, Vec<u16>>,
    pub command_by_pane: HashMap<String, String>,
    pub live_agent_panes: HashSet<String>,
    /// Every pane the scan examined, including panes with no ports and no
    /// detected command. Consumers use this to refresh (or clear) runtime
    /// state exactly for the panes this scan saw — never for panes that
    /// appeared after the scan request was queued.
    pub scanned_panes: HashSet<String>,
    /// The subset of [`Self::scanned_panes`] that were agent targets.
    /// Dead-scan teardown only advances for these; non-agent window panes
    /// are never "live agents" and must not be torn down by the sweep.
    pub scanned_agent_panes: HashSet<String>,
}

/// Whether a scan target is an agent pane or a non-agent window pane.
/// Only agent targets participate in dead-scan teardown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneKind {
    Agent,
    Window,
}

/// One pane's process-scan input. A `pane_pid` of `None` (degenerate pid
/// parse) still participates in the scan as an always-missed pane so its
/// dead-scan streak can advance, matching the old inline scanner.
#[derive(Debug, Clone)]
pub struct PaneScanTarget {
    pub pane_id: String,
    pub pane_pid: Option<u32>,
    pub agent: crate::tmux::AgentType,
    pub kind: PaneKind,
}

/// Whether an lsof exit status carries usable listening-port output.
///
/// lsof encodes the result in its exit code: 0 = listeners found and
/// printed, 1 = none found — a valid empty scan, and the common case on
/// a quiet box. Anything else (2+, or `None` from a signal kill on the
/// timeout path) means the probe itself failed and must not read as
/// "no listeners anywhere".
fn lsof_status_usable(status: &std::process::ExitStatus) -> bool {
    matches!(status.code(), Some(0) | Some(1))
}

/// Run the `lsof` listening-port probe and return its raw output.
/// `None` is reserved for probe failure (spawn error, timeout, error
/// exit) so the caller can tell "no listeners" apart from "could not
/// look" — see [`lsof_status_usable`].
fn run_lsof() -> Option<String> {
    let mut command = Command::new("lsof");
    command.args(["-iTCP", "-sTCP:LISTEN", "-nP", "-F", "pn"]);
    let output = subprocess::run_with_timeout(&mut command, SUBPROCESS_TIMEOUT).ok()?;
    if lsof_status_usable(&output.status) {
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        None
    }
}

fn is_shell_command(basename: &str) -> bool {
    matches!(
        basename,
        "bash" | "sh" | "zsh" | "fish" | "tmux" | "login" | "sudo"
    )
}

fn best_command_for_pane(pane_pid: u32, process_snapshot: &ProcessSnapshot) -> Option<String> {
    let descendants = process_snapshot.descendants(&[pane_pid]);
    let mut leaf_candidates: Vec<(usize, String)> = Vec::new();
    let mut fallback_candidates: Vec<(usize, String)> = Vec::new();

    for pid in descendants {
        let Some(info) = process_snapshot.info_by_pid.get(&pid) else {
            continue;
        };
        let first_comm = info.comm.split_whitespace().next().unwrap_or(&info.comm);
        let basename = command_basename(first_comm);
        if basename.is_empty() || is_shell_command(basename) {
            continue;
        }
        let candidate = if info.args.is_empty() {
            info.comm.clone()
        } else {
            info.args.trim().to_string()
        };
        let len = candidate.len();
        let is_leaf = process_snapshot
            .children_of
            .get(&pid)
            .is_none_or(|children| children.is_empty());
        if is_leaf {
            leaf_candidates.push((len, candidate));
        } else {
            fallback_candidates.push((len, candidate));
        }
    }

    leaf_candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    if let Some((_, command)) = leaf_candidates.into_iter().next() {
        return Some(command);
    }

    fallback_candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    fallback_candidates
        .into_iter()
        .next()
        .map(|(_, command)| command)
}

fn extract_port(name: &str) -> Option<u16> {
    let trimmed = name.trim();
    let (_, tail) = trimmed.rsplit_once(':')?;
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u16>().ok()
}

fn parse_lsof_listening_ports(lsof_output: &str) -> Vec<(u32, u16)> {
    let mut current_pid: Option<u32> = None;
    let mut out = Vec::new();

    for line in lsof_output.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            current_pid = rest.parse::<u32>().ok();
            continue;
        }
        if let Some(rest) = line.strip_prefix('n')
            && let (Some(pid), Some(port)) = (current_pid, extract_port(rest))
        {
            out.push((pid, port));
        }
    }

    out
}

/// Scan per-pane process state for the provided pane targets.
/// The lookup starts from each pane's PID and walks the process tree, so it can
/// pick up child dev servers spawned by an agent shell and detect when the
/// agent process itself has exited.
///
/// Runs on the port-scan worker thread: it owns the `ps` and `lsof`
/// subprocesses that used to stall the render thread every 10 seconds.
pub(crate) fn scan_pane_processes(targets: &[PaneScanTarget]) -> Option<PaneProcessSnapshot> {
    if targets.is_empty() {
        return None;
    }

    // A failed `ps` scan surfaces as `None` (no result is sent), so a
    // transient failure can never be mistaken for "every pane missed" —
    // that would advance dead-scan streaks for live agents.
    let process_snapshot = ProcessSnapshot::scan()?;

    let mut pid_to_panes: HashMap<u32, Vec<String>> = HashMap::new();
    let mut live_agent_panes: HashSet<String> = HashSet::new();
    let mut command_by_pane: HashMap<String, String> = HashMap::new();
    let mut scanned_panes: HashSet<String> = HashSet::new();
    let mut scanned_agent_panes: HashSet<String> = HashSet::new();
    for target in targets {
        scanned_panes.insert(target.pane_id.clone());
        if target.kind == PaneKind::Agent {
            scanned_agent_panes.insert(target.pane_id.clone());
        }
        let Some(pane_pid) = target.pane_pid else {
            continue;
        };
        let descendant_set = process_snapshot.descendants(&[pane_pid]);
        if process_snapshot.tree_has_agent(&[pane_pid], &target.agent) {
            live_agent_panes.insert(target.pane_id.clone());
        }
        if let Some(command) = best_command_for_pane(pane_pid, &process_snapshot) {
            command_by_pane.insert(target.pane_id.clone(), command);
        }
        for pid in descendant_set {
            pid_to_panes
                .entry(pid)
                .or_default()
                .push(target.pane_id.clone());
        }
    }

    // lsof is optional: a failed probe only means "no listening-port
    // data this sweep", never "the whole scan failed". Returning None
    // here would freeze dead-scan streaks (the caller drops None
    // snapshots), silently disabling teardown on hosts without lsof.
    // The process-tree data above is the load-bearing half. The empty
    // port set a failed probe produces is survivable at the consumer:
    // `apply_process_snapshot` holds the previous per-pane badge for a
    // sweep rather than clearing it, so a transient lsof failure does
    // not flash every badge on screen.
    let lsof_output = run_lsof();
    if lsof_output.is_none() {
        // A failed probe is indistinguishable from "no ports found" in
        // the snapshot itself; surface the distinction for flash reports.
        crate::debug::log("port-scan: lsof probe failed; previous port state holds for one sweep");
    }
    let listening = lsof_output
        .map(|output| parse_lsof_listening_ports(&output))
        .unwrap_or_default();

    let mut ports_by_pane: HashMap<String, BTreeSet<u16>> = HashMap::new();
    for (pid, port) in listening {
        if let Some(panes) = pid_to_panes.get(&pid) {
            for pane_id in panes {
                ports_by_pane
                    .entry(pane_id.clone())
                    .or_default()
                    .insert(port);
            }
        }
    }

    Some(PaneProcessSnapshot {
        ports_by_pane: ports_by_pane
            .into_iter()
            .map(|(pane_id, ports)| (pane_id, ports.into_iter().collect()))
            .collect(),
        command_by_pane,
        live_agent_panes,
        scanned_panes,
        scanned_agent_panes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_port_handles_common_lsof_names() {
        assert_eq!(extract_port("127.0.0.1:3000"), Some(3000));
        assert_eq!(extract_port("*:5173"), Some(5173));
        assert_eq!(extract_port("localhost:http"), None);
    }

    #[test]
    fn parse_lsof_listening_ports_pairs_pid_and_port() {
        let sample = "p123\nn127.0.0.1:3000\np456\nn*:5173\n";
        assert_eq!(
            parse_lsof_listening_ports(sample),
            vec![(123, 3000), (456, 5173)]
        );
    }

    #[cfg(unix)]
    #[test]
    fn lsof_exit_one_is_a_valid_empty_scan() {
        use std::os::unix::process::ExitStatusExt;
        // Exit 0: listeners found and printed.
        assert!(lsof_status_usable(&std::process::ExitStatus::from_raw(0)));
        // Exit 1: no listeners matched — a valid empty scan, the common
        // case on a quiet box, not a probe failure.
        assert!(lsof_status_usable(&std::process::ExitStatus::from_raw(
            1 << 8
        )));
    }

    #[cfg(unix)]
    #[test]
    fn lsof_error_exit_is_a_probe_failure() {
        use std::os::unix::process::ExitStatusExt;
        // Exit 2+: lsof itself failed (bad flags, permission errors).
        assert!(!lsof_status_usable(&std::process::ExitStatus::from_raw(
            2 << 8
        )));
        // A signal kill (the timeout path SIGKILLs the probe) carries no
        // exit code and must read as failure, not as empty data.
        assert!(!lsof_status_usable(&std::process::ExitStatus::from_raw(9)));
    }

    #[test]
    fn best_command_for_pane_prefers_leaf_non_shell_command() {
        let snapshot = ProcessSnapshot::from_ps_output(
            "10 1 zsh zsh\n11 10 node /usr/bin/node /tmp/server.js --port 3000\n12 10 git /usr/bin/git status\n",
        );

        let command = best_command_for_pane(10, &snapshot).unwrap();
        assert_eq!(command, "/usr/bin/node /tmp/server.js --port 3000");
    }
}
