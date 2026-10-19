use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::subprocess;
use crate::tmux::{AGY_BIN, ANTIGRAVITY_AGENT, AgentType, HERMES_AGENT};

/// Deadline for the full-process `ps` scan; it runs on the TUI event loop
/// every refresh tick and must never hang there.
const PS_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub(crate) struct ProcessInfo {
    pub(crate) comm: String,
    pub(crate) args: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ProcessSnapshot {
    pub(crate) children_of: HashMap<u32, Vec<u32>>,
    pub(crate) info_by_pid: HashMap<u32, ProcessInfo>,
}

impl ProcessSnapshot {
    pub(crate) fn scan() -> Option<Self> {
        let mut command = Command::new("ps");
        command.args(["-eo", "pid=,ppid=,comm=,args="]);
        let output = subprocess::run_with_timeout(&mut command, PS_TIMEOUT).ok()?;
        if !output.status.success() {
            return None;
        }
        Some(Self::from_ps_output(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    pub(crate) fn from_ps_output(ps_output: &str) -> Self {
        let mut children_of: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut info_by_pid: HashMap<u32, ProcessInfo> = HashMap::new();

        for line in ps_output.lines() {
            let mut parts = line.split_whitespace();
            let Some(pid_str) = parts.next() else {
                continue;
            };
            let Some(ppid_str) = parts.next() else {
                continue;
            };
            let Ok(pid) = pid_str.parse::<u32>() else {
                continue;
            };
            let Ok(ppid) = ppid_str.parse::<u32>() else {
                continue;
            };
            let Some(comm) = parts.next() else {
                continue;
            };

            children_of.entry(ppid).or_default().push(pid);
            info_by_pid.insert(
                pid,
                ProcessInfo {
                    comm: comm.to_string(),
                    args: parts.collect::<Vec<_>>().join(" "),
                },
            );
        }

        Self {
            children_of,
            info_by_pid,
        }
    }

    pub(crate) fn descendants(&self, seed_pids: &[u32]) -> HashSet<u32> {
        let mut seen = HashSet::new();
        let mut queue: VecDeque<u32> = seed_pids.iter().copied().collect();

        while let Some(pid) = queue.pop_front() {
            if !seen.insert(pid) {
                continue;
            }
            if let Some(children) = self.children_of.get(&pid) {
                for &child in children {
                    if !seen.contains(&child) {
                        queue.push_back(child);
                    }
                }
            }
        }

        seen
    }

    pub(crate) fn tree_has_agent(&self, seed_pids: &[u32], agent: &AgentType) -> bool {
        let agent_name = agent.as_str();
        self.descendants(seed_pids).into_iter().any(|pid| {
            self.info_by_pid
                .get(&pid)
                .map(|info| process_matches_agent(info, agent_name))
                .unwrap_or(false)
        })
    }

    /// Whether any process under the seed pids runs a program that is
    /// not a shell or a meta-invocation (`tmux`, `sudo`, `login`).
    ///
    /// A shell whose foreground command is another shell (`bash
    /// deploy.sh`) is indistinguishable from an idle prompt by its tmux
    /// command alone — both read as `bash`. A script doing work spawns
    /// non-shell children (`sleep`, `rsync`, `gcc`), which this detects.
    /// Trees that contain only shells (an idle prompt, or a script
    /// spending its whole life in builtins) stay shell-classified.
    pub(crate) fn tree_has_non_shell(&self, seed_pids: &[u32]) -> bool {
        self.descendants(seed_pids).into_iter().any(|pid| {
            self.info_by_pid
                .get(&pid)
                .map(|info| !is_meta_or_shell_comm(&info.comm))
                .unwrap_or(false)
        })
    }

    pub(crate) fn command_lines_for_tree(&self, seed_pids: &[u32]) -> Vec<String> {
        self.descendants(seed_pids)
            .into_iter()
            .filter_map(|pid| self.info_by_pid.get(&pid))
            .map(|info| {
                if info.args.is_empty() {
                    info.comm.clone()
                } else {
                    info.args.trim().to_string()
                }
            })
            .collect()
    }
}

pub(crate) fn command_basename(command: &str) -> &str {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
}

/// Bun-compiled agent binaries keep an `.exe` suffix on every platform —
/// opencode ships `@opencode/cli/bin/opencode.exe` even on macOS — and that
/// suffix surfaces in both process `comm` values and tmux's
/// `pane_current_command`. Strip it before comparing against agent labels,
/// or macOS panes read as a different program than their Linux namesake.
pub(crate) fn strip_exe_suffix(name: &str) -> &str {
    name.strip_suffix(".exe").unwrap_or(name)
}

/// Shell and meta-invocation names that never count as "work" when
/// scanning a pane's process tree. The shell set is the same list
/// [`crate::tmux::classify_window_status`] classifies with; `tmux`,
/// `sudo`, and `login` are meta-invocations — the shell-integration
/// snippet forks a transient `tmux` at every prompt, which must not
/// read as a running task.
fn is_meta_or_shell_comm(comm: &str) -> bool {
    if crate::tmux::is_shell_basename(comm) {
        return true;
    }
    matches!(command_basename(comm), "tmux" | "sudo" | "login")
}

pub(crate) fn process_matches_agent(info: &ProcessInfo, agent_name: &str) -> bool {
    // Hermes' launcher is `python3 -I -c <bootstrap>`. On Linux the
    // bootstrap renames the process (prctl), so `comm` reads `hermes`; macOS
    // has no rename and keeps `comm` as `python3`, leaving the install root
    // visible only in argv. Match that launcher shape explicitly or macOS
    // hermes panes are invisible to every sweep that uses this predicate.
    if agent_name == HERMES_AGENT && process_matches_hermes_launcher(info) {
        return true;
    }
    // Antigravity's executable is `agy` (see `AGY_BIN`), so its process
    // `comm` never equals the "antigravity" agent label. Match the binary
    // name explicitly or the stale-state sweep reads live panes as dead.
    if agent_name == ANTIGRAVITY_AGENT && strip_exe_suffix(command_basename(&info.comm)) == AGY_BIN
    {
        return true;
    }
    if strip_exe_suffix(command_basename(&info.comm)) == agent_name {
        return true;
    }

    // Wrapper installs name the runtime at args0, not the agent:
    // `npx claude` runs under node, `node .../claude` and bun shims put
    // the agent binary deeper in argv. Match argv tokens, but strictly —
    // see `argv_tokens_match_agent` — so these live panes are not misread
    // as "agent dead" by the port-scan sweep (which would wipe their
    // metadata mid-session).
    argv_tokens_match_agent(&info.args, agent_name)
}

/// Package runners whose argv carries the agent as a bare package name
/// rather than a path (`npx claude`, or npx's internal
/// `node .../npx-cli.js claude` entry).
const PACKAGE_RUNNERS: [&str; 5] = ["npx", "bunx", "uvx", "dlx", "pnpx"];

/// Whether `args` invokes `agent_name` strictly enough to trust.
///
/// A token whose basename equals the agent name matches only when it is a
/// path into the agent binary (`node .../bin/claude --model opus`) or sits
/// directly after a package runner (`npx claude`). A bare agent name
/// anywhere else is data, not a program: `ansible-playbook --tags hermes`
/// once turned an ordinary shell pane into a Hermes agent row.
fn argv_tokens_match_agent(args: &str, agent_name: &str) -> bool {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    tokens.iter().enumerate().any(|(index, raw_token)| {
        let token = raw_token.trim_matches('"');
        if command_basename(token) != agent_name {
            return false;
        }
        if token.contains('/') {
            return true;
        }
        index > 0
            && (PACKAGE_RUNNERS.contains(&command_basename(tokens[index - 1]))
                || command_basename(tokens[index - 1]).starts_with("npx"))
    })
}

/// The hermes launcher shape: a python interpreter running `-I -c`
/// bootstrap code that references the `~/.hermes` install root.
///
/// The interpreter is read from argv[0], not `comm`: macOS truncates
/// `comm` to the first 16 bytes of argv[0], which for the launcher is a
/// fragment of the interpreter's path (`/Users/greg-bran`), not a basename.
fn process_matches_hermes_launcher(info: &ProcessInfo) -> bool {
    let interpreter = info
        .args
        .split_whitespace()
        .next()
        .map(command_basename)
        .unwrap_or("");
    if !interpreter.starts_with("python") {
        return false;
    }
    let mut isolated = false;
    let mut command = false;
    let mut install_root = false;
    for token in info.args.split_whitespace() {
        match token {
            "-I" => isolated = true,
            "-c" => command = true,
            _ => {}
        }
        if token.contains("/.hermes/") {
            install_root = true;
        }
    }
    isolated && command && install_root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descendants_walks_process_tree() {
        let snapshot = ProcessSnapshot {
            children_of: HashMap::from([(1, vec![2, 3]), (2, vec![4]), (4, vec![5])]),
            info_by_pid: HashMap::new(),
        };
        let seen = snapshot.descendants(&[1]);
        assert!(seen.contains(&1));
        assert!(seen.contains(&2));
        assert!(seen.contains(&3));
        assert!(seen.contains(&4));
        assert!(seen.contains(&5));
    }

    #[test]
    fn parse_ps_processes_preserves_spaced_args() {
        let snapshot = ProcessSnapshot::from_ps_output(
            "100 1 codex /Applications/Codex App/bin/codex --full-auto\n101 100 sh sh -c wrapper\n",
        );

        assert_eq!(snapshot.children_of.get(&1).cloned(), Some(vec![100]));
        let info = snapshot.info_by_pid.get(&100).expect("process info");
        assert_eq!(info.comm, "codex");
        assert_eq!(info.args, "/Applications/Codex App/bin/codex --full-auto");
    }

    #[test]
    fn process_matches_agent_matches_agy_binary_name() {
        // Antigravity runs as the `agy` executable; the agent label is
        // "antigravity". The comm must match through the alias or the
        // stale-state sweep would wipe a live pane's metadata.
        assert!(process_matches_agent(
            &ProcessInfo {
                comm: "agy".to_string(),
                args: "agy".to_string(),
            },
            "antigravity",
        ));
        // The canonical label still matches (hooks rename nothing here,
        // but future-proof the predicate).
        assert!(process_matches_agent(
            &ProcessInfo {
                comm: "antigravity".to_string(),
                args: "antigravity".to_string(),
            },
            "antigravity",
        ));
        // Other agents must not match through the agy alias.
        assert!(!process_matches_agent(
            &ProcessInfo {
                comm: "agy".to_string(),
                args: "agy".to_string(),
            },
            "claude",
        ));
    }

    #[test]
    fn tree_has_agent_matches_descendant_process_name() {
        let snapshot = ProcessSnapshot::from_ps_output(
            "100 1 fish fish -c opencode\n101 100 opencode opencode\n",
        );

        assert!(snapshot.tree_has_agent(&[100], &AgentType::OpenCode));
        assert!(!snapshot.tree_has_agent(&[100], &AgentType::Codex));
    }

    #[test]
    fn process_matches_agent_matches_macos_exe_suffix() {
        // opencode's bun binary is `opencode.exe` on every platform; on
        // macOS that name is what shows up in both comm and tmux's
        // pane_current_command. It must still match the "opencode" label.
        assert!(process_matches_agent(
            &ProcessInfo {
                comm: "opencode.exe".to_string(),
                args: "/Users/u/.local/share/fnm/node-versions/v22.21.0/installation/lib/node_modules/@opencode/cli/bin/opencode.exe"
                    .to_string(),
            },
            "opencode",
        ));
        // The suffix strip must not loosen matching for lookalikes.
        assert!(!process_matches_agent(
            &ProcessInfo {
                comm: "not-opencode.exe".to_string(),
                args: "/usr/local/bin/not-opencode.exe".to_string(),
            },
            "opencode",
        ));
    }

    #[test]
    fn process_matches_agent_requires_command_name_match() {
        assert!(process_matches_agent(
            &ProcessInfo {
                comm: "claude".to_string(),
                args: "/opt/homebrew/bin/claude --flag".to_string(),
            },
            "claude",
        ));
        assert!(process_matches_agent(
            &ProcessInfo {
                comm: "node".to_string(),
                args: "/usr/local/bin/opencode".to_string(),
            },
            "opencode",
        ));
        assert!(!process_matches_agent(
            &ProcessInfo {
                comm: "not-opencode".to_string(),
                args: "/usr/local/bin/not-opencode".to_string(),
            },
            "opencode",
        ));
    }

    #[test]
    fn process_matches_agent_matches_runner_and_path_tokens() {
        // npx shim: the runtime owns comm/args0, the agent name only
        // appears as a later argv token.
        assert!(process_matches_agent(
            &ProcessInfo {
                comm: "node".to_string(),
                args: "npx claude --continue".to_string(),
            },
            "claude",
        ));
        // npx's cli entry: node running npx-cli.js with the package name.
        assert!(process_matches_agent(
            &ProcessInfo {
                comm: "node".to_string(),
                args: "/usr/local/bin/node /path/to/npx-cli.js claude".to_string(),
            },
            "claude",
        ));
        // Direct node invocation of the installed binary.
        assert!(process_matches_agent(
            &ProcessInfo {
                comm: "node".to_string(),
                args: "node /home/u/.npm/_npx/abc/bin/claude --model opus".to_string(),
            },
            "claude",
        ));
        // Bun shim.
        assert!(process_matches_agent(
            &ProcessInfo {
                comm: "bun".to_string(),
                args: "bun /usr/local/bin/claude".to_string(),
            },
            "claude",
        ));
        // A basename glued to other characters must not match.
        assert!(!process_matches_agent(
            &ProcessInfo {
                comm: "node".to_string(),
                args: "node /path/to/not-claude".to_string(),
            },
            "claude",
        ));
    }

    #[test]
    fn process_matches_agent_rejects_bare_agent_name_outside_runner_position() {
        // An agent name appearing as argument data is not the agent running:
        // `ansible-playbook site.yml --tags hermes` once registered an
        // ordinary shell pane as a Hermes agent row.
        assert!(!process_matches_agent(
            &ProcessInfo {
                comm: "ansible-playbook".to_string(),
                args: "ansible-playbook site.yml --tags hermes".to_string(),
            },
            "hermes",
        ));
        // Same for a bare name in a non-runner position right after the
        // program (`grep hermes notes.txt`).
        assert!(!process_matches_agent(
            &ProcessInfo {
                comm: "grep".to_string(),
                args: "grep hermes notes.txt".to_string(),
            },
            "hermes",
        ));
        // And an agent name glued into a larger path segment.
        assert!(!process_matches_agent(
            &ProcessInfo {
                comm: "python3".to_string(),
                args: "python3 /home/u/.hermes-tools/render.py".to_string(),
            },
            "hermes",
        ));
    }

    #[test]
    fn process_matches_agent_matches_hermes_launcher_without_rename() {
        // macOS: the hermes launcher cannot rename its process, and `comm`
        // is argv[0] truncated to 16 bytes — for the launcher that is a
        // fragment of the interpreter's path, not a basename. The launcher
        // shape (python argv[0], `-I -c`, `/.hermes/` install root) must
        // still match Hermes.
        assert!(process_matches_agent(
            &ProcessInfo {
                comm: "/Users/greg-bran".to_string(),
                args: "/Users/u/.hermes/tools/python-3.14.7/bin/python3 -I -c import os, sys\012sys.path.insert(0, '/Users/u/.hermes/hermes-agent')\012import hermes_cli".to_string(),
            },
            "hermes",
        ));
        // An interpreter name in comm alone (no launcher argv) does not match.
        assert!(!process_matches_agent(
            &ProcessInfo {
                comm: "python3.14".to_string(),
                args: "python3 /tmp/tool.py".to_string(),
            },
            "hermes",
        ));
        // A hermes-managed python running an ordinary script (no -c
        // bootstrap) is not the launcher.
        assert!(!process_matches_agent(
            &ProcessInfo {
                comm: "python3".to_string(),
                args: "/Users/u/.hermes/tools/python-3.14.7/bin/python3 /tmp/tool.py".to_string(),
            },
            "hermes",
        ));
        // The renamed Linux process keeps matching through comm.
        assert!(process_matches_agent(
            &ProcessInfo {
                comm: "hermes".to_string(),
                args: "python3 -I -c bootstrap".to_string(),
            },
            "hermes",
        ));
    }

    #[test]
    fn tree_has_agent_matches_wrapper_installed_agent() {
        let snapshot = ProcessSnapshot::from_ps_output(
            "100 1 zsh zsh -c npx claude\n101 100 node node /path/to/npx-cli.js claude\n",
        );

        assert!(snapshot.tree_has_agent(&[100], &AgentType::Claude));
    }

    #[test]
    fn tree_has_non_shell_detects_script_work() {
        // A script run: the interpreter shells out to real programs.
        let snapshot = ProcessSnapshot::from_ps_output(
            "100 1 zsh zsh\n101 100 bash bash deploy.sh\n102 101 rsync rsync -a src/ dst/\n",
        );
        assert!(snapshot.tree_has_non_shell(&[100]));
    }

    #[test]
    fn tree_has_non_shell_ignores_shell_only_trees() {
        // An idle prompt: nothing but shells under the pane pid.
        let snapshot = ProcessSnapshot::from_ps_output("100 1 zsh zsh\n");
        assert!(!snapshot.tree_has_non_shell(&[100]));

        // A script currently in a shell builtin loop: still shell-only.
        let snapshot =
            ProcessSnapshot::from_ps_output("100 1 zsh zsh\n101 100 bash bash loop.sh\n");
        assert!(!snapshot.tree_has_non_shell(&[100]));
    }

    #[test]
    fn tree_has_non_shell_ignores_meta_invocations() {
        // The shell-integration snippet forks a transient `tmux` at every
        // prompt; that must not read as a running task.
        let snapshot =
            ProcessSnapshot::from_ps_output("100 1 zsh zsh\n101 100 tmux tmux set-option -p x y\n");
        assert!(!snapshot.tree_has_non_shell(&[100]));
    }
}
