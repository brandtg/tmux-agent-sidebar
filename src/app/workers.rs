use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use crate::group::{PaneGitInfo, resolve_pane_git_info};
use crate::port::{self, PaneProcessSnapshot, PaneScanTarget};
use crate::session;
use crate::tmux;
use crate::version::{self, UpdateNotice};

/// How long a cached path→git-info entry is reused before the resolver
/// re-runs `git rev-parse` for it. Bounds both the subprocess rate (one
/// spawn per path per TTL instead of per tick) and the staleness of the
/// branch label shown for panes without hook-provided worktree metadata.
const GIT_INFO_TTL: Duration = Duration::from_secs(10);

/// How long the capture-pane subprocess may take before the approval scan
/// gives up on that pane. Well under the 1s tick so a hung capture cannot
/// stack up scans.
const APPROVAL_CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);

/// How much of the pane tail is inspected for an approval prompt. agy
/// renders the prompt block at the bottom of the screen; restricting the
/// match to the tail keeps conversation text that merely quotes a prompt
/// line ("Run this command?") from flagging the pane.
const APPROVAL_TAIL_LINES: usize = 15;

/// Text agy renders while a tool call waits for a permission decision.
/// Verified live against agy 1.4.2 (command and URL prompts); the
/// changelog documents the same family for tool calls ("Allow calling
/// this tool?"). Matched against line starts in the pane tail — the
/// prompt block is left-aligned, and start-anchoring keeps quoted
/// prompt text mid-conversation from matching.
const PERMISSION_PROMPT_PATTERNS: [&str; 4] = [
    "Requesting permission for",
    "Run this command?",
    "Allow access to this URL?",
    "Allow calling this tool?",
];

/// Channels and shared flags produced by [`spawn`] that the main event loop
/// drains every tick.
pub(super) struct Workers {
    pub session_rx: Receiver<HashMap<String, String>>,
    pub version_rx: Receiver<UpdateNotice>,
    /// Requests the git-info resolver worker: the distinct pane working
    /// directories of the current snapshot. Results arrive on `git_info_rx`.
    pub git_info_tx: mpsc::Sender<Vec<String>>,
    pub git_info_rx: Receiver<HashMap<String, PaneGitInfo>>,
    /// Requests the port-scan worker: the panes to examine. Results arrive
    /// on `port_scan_rx`.
    pub port_scan_tx: mpsc::Sender<Vec<PaneScanTarget>>,
    pub port_scan_rx: Receiver<Option<PaneProcessSnapshot>>,
    /// Requests the approval-scan worker: the Antigravity pane ids whose
    /// status is `running` (its own flagged set is merged in worker-side).
    /// Fire-and-forget — the worker writes pane options directly.
    pub approval_tx: mpsc::Sender<Vec<String>>,
}

/// Spawn the background threads (session-name polling, version notice
/// fetch, git-info resolution, port scanning) that feed the event loop.
pub(super) fn spawn(_state: &crate::state::AppState) -> Workers {
    let (session_tx, session_rx) = mpsc::channel::<HashMap<String, String>>();
    let (version_tx, version_rx) = mpsc::channel::<UpdateNotice>();
    let (git_info_tx, git_info_req_rx) = mpsc::channel::<Vec<String>>();
    let (git_info_res_tx, git_info_rx) = mpsc::channel::<HashMap<String, PaneGitInfo>>();
    let (port_scan_tx, port_scan_req_rx) = mpsc::channel::<Vec<PaneScanTarget>>();
    let (port_scan_res_tx, port_scan_rx) = mpsc::channel::<Option<PaneProcessSnapshot>>();
    let (approval_tx, approval_req_rx) = mpsc::channel::<Vec<String>>();
    std::thread::spawn(move || {
        session_poll_loop(&session_tx);
    });
    std::thread::spawn(move || {
        if let Some(notice) = version::fetch_update_notice() {
            let _ = version_tx.send(notice);
        }
    });
    std::thread::spawn(move || {
        git_info_poll_loop(&git_info_req_rx, &git_info_res_tx);
    });
    std::thread::spawn(move || {
        port_scan_loop(&port_scan_req_rx, &port_scan_res_tx);
    });
    std::thread::spawn(move || {
        approval_scan_loop(&approval_req_rx);
    });

    Workers {
        session_rx,
        version_rx,
        git_info_tx,
        git_info_rx,
        port_scan_tx,
        port_scan_rx,
        approval_tx,
    }
}

/// Git-metadata resolver worker. Owns the path→[`PaneGitInfo`] cache that
/// `group_panes_by_repo` consumes: each tick the event loop sends the
/// distinct pane working directories, the worker refreshes entries that are
/// missing or older than [`GIT_INFO_TTL`] (one `git rev-parse` per stale
/// path — the spawn the render thread used to pay every tick) and returns
/// the full map. Entries for paths that dropped out of the snapshot are
/// pruned.
pub(super) fn git_info_poll_loop(
    rx: &mpsc::Receiver<Vec<String>>,
    tx: &mpsc::Sender<HashMap<String, PaneGitInfo>>,
) {
    let mut cache: HashMap<String, (PaneGitInfo, std::time::Instant)> = HashMap::new();
    while let Ok(paths) = rx.recv() {
        let now = std::time::Instant::now();
        let wanted: HashSet<&str> = paths.iter().map(|path| path.as_str()).collect();
        cache.retain(|path, _| wanted.contains(path.as_str()));
        for path in &paths {
            let fresh = cache
                .get(path)
                .is_some_and(|(_, resolved_at)| now.duration_since(*resolved_at) < GIT_INFO_TTL);
            if !fresh {
                let info = resolve_pane_git_info(path);
                cache.insert(path.clone(), (info, now));
            }
        }
        let snapshot: HashMap<String, PaneGitInfo> = cache
            .iter()
            .map(|(path, (info, _))| (path.clone(), info.clone()))
            .collect();
        if tx.send(snapshot).is_err() {
            return;
        }
    }
}

/// Port-scan worker. Runs the `ps` snapshot plus the `lsof` listening-port
/// probe off the render thread (the inline scan used to stall the TUI for
/// up to the subprocess timeout every 10 seconds). A failed scan sends
/// `None`; the event loop ignores it and the next due scan retries.
pub(super) fn port_scan_loop(
    rx: &mpsc::Receiver<Vec<PaneScanTarget>>,
    tx: &mpsc::Sender<Option<PaneProcessSnapshot>>,
) {
    while let Ok(targets) = rx.recv() {
        let scanned = port::scan_pane_processes(&targets);
        if tx.send(scanned).is_err() {
            return;
        }
    }
}

/// Antigravity blocking-prompt worker.
///
/// agy has no hook for "waiting on the user" — its `PreToolUse` fires
/// before the decision is made and carries no policy state, and nothing
/// else fires while a permission prompt or question picker is open, so
/// the pane would stay solid green (running) for as long as the user
/// takes to answer. The only observable signal is the prompt agy renders
/// in the pane itself, so this worker capture-scans Antigravity panes and
/// maintains the waiting state on the pane options directly:
///
/// - prompt visible → `@pane_status=waiting`, `@pane_wait_reason=
///   permission|question` (see [`detect_prompt_reason`]),
///   `@pane_attention=notification` (the ask is attention-worthy — the
///   same contract the hook-driven agents use)
/// - prompt gone → back to `running` with the wait reason and attention
///   cleared
///
/// The worker remembers which panes it flagged so a waiting pane keeps
/// being rescanned until the prompt is answered even though its status is
/// no longer `running` (the TUI only queues running panes). Writes are
/// fire-and-forget: the next `list-panes` parse reads the options like any
/// hook-written state, and `notify_sidebars_of_pane` pokes the sidebar for
/// an instant repaint.
pub(super) fn approval_scan_loop(rx: &mpsc::Receiver<Vec<String>>) {
    let mut flagged: HashSet<String> = HashSet::new();
    while let Ok(targets) = rx.recv() {
        // Rescan running panes plus every pane this worker flagged, so the
        // clear is observed even though a flagged pane reads as `waiting`.
        let mut panes = targets;
        for pane_id in &flagged {
            if !panes.contains(pane_id) {
                panes.push(pane_id.clone());
            }
        }

        let mut still_flagged = HashSet::new();
        for pane_id in panes {
            let capture = capture_pane_tail(&pane_id);
            let prompt_reason = detect_prompt_reason(&capture);
            crate::debug::log(&format!(
                "approval-scan: pane {pane_id} captured {} line(s), prompt={prompt_reason:?}",
                capture.len()
            ));
            match (prompt_reason, flagged.contains(&pane_id)) {
                (Some(reason), false) => {
                    flag_waiting(&pane_id, reason);
                    still_flagged.insert(pane_id.clone());
                    crate::cli::notify_focus::notify_sidebars_of_pane(&pane_id);
                }
                (None, true) => {
                    clear_waiting(&pane_id);
                    crate::cli::notify_focus::notify_sidebars_of_pane(&pane_id);
                }
                (Some(_), true) => {
                    still_flagged.insert(pane_id.clone());
                }
                (None, false) => {}
            }
        }
        flagged = still_flagged;
    }
}

/// Capture the visible pane content and reduce it to the tail lines the
/// approval matcher inspects. Empty on any failure — a failed capture must
/// never flip state (the next cycle retries).
fn capture_pane_tail(pane_id: &str) -> Vec<String> {
    let mut command = std::process::Command::new("tmux");
    command.args(["capture-pane", "-p", "-t", pane_id]);
    let Ok(output) = crate::subprocess::run_with_timeout(&mut command, APPROVAL_CAPTURE_TIMEOUT)
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // Blank rows are dropped before slicing: capture-pane pads to the full
    // pane height, so on a mostly-empty screen the prompt block would sit
    // entirely above a tail of blank padding lines.
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(APPROVAL_TAIL_LINES);
    lines[start..].iter().map(|l| l.to_string()).collect()
}

/// Classify a prompt block in the pane tail: `Some(wait_reason)` while a
/// blocking prompt is up, `None` otherwise. Permission prompts map to
/// `permission`, agent question pickers to `question` — both are in the
/// sidebar's wait-reason vocabulary (same contract as Hermes'
/// `on_human_input_request` mapping).
fn detect_prompt_reason(tail: &[String]) -> Option<&'static str> {
    for line in tail {
        let line = line.trim_start();
        if PERMISSION_PROMPT_PATTERNS
            .iter()
            .any(|pattern| line.starts_with(pattern))
        {
            return Some("permission");
        }
        if is_question_header(line) {
            return Some("question");
        }
    }
    None
}

/// `Question 1/1: …` — the numbered header of agy's question picker
/// (`ask_question`). Numbering is what distinguishes it from conversation
/// text that merely starts with "Question".
fn is_question_header(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("Question ") else {
        return false;
    };
    let Some((numbering, _)) = rest.split_once(':') else {
        return false;
    };
    let Some((current, total)) = numbering.split_once('/') else {
        return false;
    };
    !current.is_empty()
        && !total.is_empty()
        && current.chars().all(|c| c.is_ascii_digit())
        && total.chars().all(|c| c.is_ascii_digit())
}

/// Write the waiting state for a blocking prompt (`permission` or
/// `question` wait reason).
fn flag_waiting(pane_id: &str, reason: &str) {
    tmux::set_pane_option(pane_id, tmux::PANE_STATUS, "waiting");
    tmux::set_pane_option(pane_id, tmux::PANE_WAIT_REASON, reason);
    tmux::set_pane_option(pane_id, tmux::PANE_ATTENTION, "notification");
}

/// Clear the waiting state once the prompt is answered.
fn clear_waiting(pane_id: &str) {
    tmux::set_pane_option(pane_id, tmux::PANE_STATUS, "running");
    tmux::unset_pane_option(pane_id, tmux::PANE_WAIT_REASON);
    tmux::set_pane_option(pane_id, tmux::PANE_ATTENTION, "clear");
}

/// Session name polling thread. Refreshes the `session_id → name` map every
/// 10 seconds — Claude `/rename` labels from `~/.claude/sessions/*.json` and
/// Hermes session titles from `~/.hermes/state.db` — so the main TUI thread
/// never performs blocking filesystem/subprocess I/O to refresh labels.
pub(super) fn session_poll_loop(tx: &mpsc::Sender<HashMap<String, String>>) {
    loop {
        std::thread::sleep(Duration::from_secs(10));
        let names = session::scan_session_names();
        if tx.send(names).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod approval_tests {
    use super::*;

    #[test]
    fn detects_the_permission_prompt_family() {
        // Real renderings captured from agy 1.4.2: the command prompt
        // carries a "Requesting permission for:" header, the URL prompt
        // leads with its own question line.
        let command_prompt: Vec<String> = vec![
            "Requesting permission for:".into(),
            "   touch permission-test.txt".into(),
            "Run this command?".into(),
            "> 1. Yes, run command".into(),
            "  ↑/↓ Navigate · tab Amend".into(),
        ];
        let url_prompt: Vec<String> = vec![
            "https://example.com".into(),
            "Allow access to this URL?".into(),
            "> 1. Yes, allow access".into(),
        ];
        assert_eq!(detect_prompt_reason(&command_prompt), Some("permission"));
        assert_eq!(detect_prompt_reason(&url_prompt), Some("permission"));
    }

    #[test]
    fn detects_the_question_picker_header() {
        // Real rendering of the ask_question picker: a "Question" title
        // line, then the numbered header with the options.
        let question: Vec<String> = vec![
            "Question".into(),
            "────────────────────────────".into(),
            "Question 1/1: Which database should we use?".into(),
            "> 1. PostgreSQL".into(),
            "  2. SQLite".into(),
            "  ↑/↓ Navigate · enter Select · esc Skip".into(),
        ];
        assert_eq!(detect_prompt_reason(&question), Some("question"));
        // Multi-question pickers number higher.
        let multi: Vec<String> = vec!["Question 2/3: Second?".into()];
        assert_eq!(detect_prompt_reason(&multi), Some("question"));
    }

    #[test]
    fn question_lookalikes_do_not_match() {
        // Conversation text that starts with "Question" without the
        // numbered header must not flag the pane.
        for line in [
            "Question: what should we do?",
            "Question 1: pick one",
            "Question 1/2 without colon",
            "The Question 1/1: is misleading",
        ] {
            assert_eq!(detect_prompt_reason(&[line.to_string()]), None, "{line}");
        }
    }

    #[test]
    fn quoted_prompt_text_is_ignored() {
        // A response that merely quotes a prompt line must not flag the
        // pane: mid-line matches don't count, and the tail restriction
        // keeps head-of-scroll quotes out of the inspected region.
        let mid_line_quote: Vec<String> = vec![
            "The agent asked: Run this command? earlier".into(),
            "done".into(),
        ];
        let indented_quote: Vec<String> = vec!["   Run this command? earlier".into()];

        // Mid-line quotes never match...
        assert_eq!(detect_prompt_reason(&mid_line_quote), None);
        // ...and a line-start quote only matches when it sits in the tail
        // region the scan actually inspects (the caller slices the tail).
        assert_eq!(detect_prompt_reason(&indented_quote), Some("permission"));
    }

    #[test]
    fn ordinary_running_panes_do_not_match() {
        let running: Vec<String> = vec![
            "❯ Read src/main.rs".into(),
            "● Explored 1 file (example.com)".into(),
            "esc to cancel            Gemini 3.8 Flash · high".into(),
        ];
        assert_eq!(detect_prompt_reason(&running), None);
    }
}
