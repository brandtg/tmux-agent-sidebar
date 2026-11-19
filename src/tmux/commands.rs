use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::subprocess;

/// Deadline for tmux IPC; every render-path query (list-panes,
/// display-message, ...) goes through here and must never hang the loop.
const TMUX_TIMEOUT: Duration = Duration::from_secs(3);

/// Whether tmux IPC may spawn at all in this process. The binary entry
/// point opts in via [`enable_tmux_ipc`]; a test harness never runs
/// `main()`, so under `cargo test` every tmux invocation is refused at
/// this choke point. Without the gate, tests exercise real lib paths
/// against whatever tmux server they can find — harmless on CI (no
/// server, every call fails exactly like the tests assume) but live
/// against the developer's session locally: snapshot tests would pull
/// in the user's real panes, hook-dispatch tests would write options,
/// and focus paths could steal the client.
static TMUX_IPC_ENABLED: AtomicBool = AtomicBool::new(false);

/// Allow tmux IPC for this process. Called once, first thing, from the
/// binary entry point. Tests that genuinely need a live tmux (the
/// `#[ignore]`d capture integration tests, which run on their own
/// throwaway server) may call this too — scoped to the test.
pub fn enable_tmux_ipc() {
    TMUX_IPC_ENABLED.store(true, Ordering::Relaxed);
}

pub fn run_tmux(args: &[&str]) -> Option<String> {
    #[cfg(test)]
    if test_fail_tmux::should_fail() {
        return None;
    }
    exec_tmux(args, Some(TMUX_TIMEOUT)).ok()
}

/// Run a tmux command that intentionally blocks for as long as the user
/// keeps something on screen — `display-popup` being the only case today.
/// These must not be subject to the render-path deadline: a popup that
/// stays open for minutes is the expected outcome, not a hang. Killing
/// the `display-popup` client process mid-popup does not even close the
/// popup (the server keeps it); it just makes the invoking `toggle`
/// report failure, which tmux surfaces as a spurious
/// `"...toggle ..." returned 1` status-line error when the popup closes.
pub fn run_tmux_blocking(args: &[&str]) -> Option<String> {
    #[cfg(test)]
    if test_fail_tmux::should_fail() {
        return None;
    }
    exec_tmux(args, None).ok()
}

/// Run a tmux command, returning trimmed stdout on success and stderr on failure.
/// Used by the spawn/remove flow so the UI can surface a meaningful error message
/// instead of a silent fallthrough.
pub fn run_tmux_capture(args: &[&str]) -> Result<String, String> {
    exec_tmux(args, Some(TMUX_TIMEOUT)).map(|out| out.trim().to_string())
}

/// Shared execution path for every tmux invocation. Failures are recorded
/// in the opt-in debug trace (`TMUX_AGENT_SIDEBAR_DEBUG=1`, see
/// [`crate::debug`]) before the caller's usual silent fallback. This is the
/// single choke point: every pane-option write, query, and window
/// operation goes through here.
///
/// `timeout` bounds the wait; `None` waits for the child to exit on its
/// own (see [`run_tmux_blocking`]).
///
/// Read commands (`show`, `display-message`, `list-panes`, …) fail
/// routinely in normal operation — an unset option or a pane that just
/// closed — so only their spawn/timeout failures are traced. Non-zero
/// exits are traced for writes (`set …`), where a failure silently loses
/// state and is exactly what the trace exists for.
fn exec_tmux(args: &[&str], timeout: Option<Duration>) -> Result<String, String> {
    if !TMUX_IPC_ENABLED.load(Ordering::Relaxed) {
        return Err("tmux IPC disabled (not enabled via enable_tmux_ipc)".to_string());
    }
    let mut command = Command::new("tmux");
    command.args(args);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let output = match timeout {
        Some(timeout) => subprocess::run_with_timeout(&mut command, timeout),
        None => Ok(command.output().map_err(|err| err.to_string())?),
    }
    .map_err(|err| {
        crate::debug::log(&format!("tmux {args:?} failed: {err}"));
        err
    })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if args.first() == Some(&"set") {
        crate::debug::log(&format!(
            "tmux {args:?} exited with {}: {}",
            output.status,
            if stderr.is_empty() {
                "<no stderr>".to_string()
            } else {
                stderr.clone()
            }
        ));
    }
    Err(if stderr.is_empty() {
        format!("tmux exited with status {}", output.status)
    } else {
        stderr
    })
}

pub fn display_message(target: &str, format: &str) -> String {
    run_tmux(&["display-message", "-t", target, "-p", format])
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Resolve the session name containing `pane_id`. Returns `None` when tmux
/// can't find the pane (e.g. it has just been closed).
pub fn pane_session_name(pane_id: &str) -> Option<String> {
    Some(display_message(pane_id, "#{session_name}")).filter(|s| !s.is_empty())
}

/// Create a new tmux window in `session` whose initial cwd is `cwd` and whose
/// title is `name`. Returns `(pane_id, window_id)` on success — the window id
/// is used by the spawn flow to set markers at window scope so split panes
/// (e.g. Claude Code subagents) inherit them.
pub fn new_window(session: &str, cwd: &str, name: &str) -> Result<(String, String), String> {
    let out = run_tmux_capture(&[
        "new-window",
        "-t",
        session,
        "-c",
        cwd,
        "-n",
        name,
        "-P",
        "-F",
        "#{pane_id} #{window_id}",
    ])?;
    let mut parts = out.split_whitespace();
    let pane = parts
        .next()
        .ok_or_else(|| "new-window returned no pane id".to_string())?
        .to_string();
    let window = parts
        .next()
        .ok_or_else(|| "new-window returned no window id".to_string())?
        .to_string();
    Ok((pane, window))
}

/// Split `target_pane` horizontally, inserting the new pane to its left
/// at `percent` percent of the target's width, starting in `cwd`, without
/// changing the active pane (`-d`). Returns the new pane id. Used by the
/// spawn / resume flows to give the agent a one-third pane next to a
/// blank shell; the sidebar pane is added separately by the
/// after-new-window hook, which targets panes by geometry rather than
/// focus, so it lands on the far left whether it fires before or after
/// this split.
pub fn split_window_left(target_pane: &str, cwd: &str, percent: u32) -> Result<String, String> {
    let percent = percent.to_string();
    let out = run_tmux_capture(&[
        "split-window",
        "-h",
        "-b",
        "-d",
        "-p",
        &percent,
        "-t",
        target_pane,
        "-c",
        cwd,
        "-P",
        "-F",
        "#{pane_id}",
    ])?;
    let pane = out.trim();
    if pane.is_empty() {
        return Err("split-window returned no pane id".to_string());
    }
    Ok(pane.to_string())
}

/// Set a user option at window scope. Needed so markers survive through
/// split panes that inherit from the window. Returns an error so the
/// spawn flow can roll back when a marker the remove path relies on
/// cannot be written — silently dropping the failure would leave an
/// un-removable pane.
pub fn set_window_option(window: &str, key: &str, value: &str) -> Result<(), String> {
    // `--` so values starting with `-` (user-supplied task slugs, paths)
    // are never parsed as tmux flags.
    run_tmux_capture(&["set", "-w", "-t", window, "--", key, value]).map(|_| ())
}

/// Send a command line to `target` (a pane id) and press Enter so the shell
/// executes it. Used to launch the agent binary right after window creation.
/// The text is sent with `-l` (literal) so nothing in `command` can collide
/// with tmux key names (e.g. `Tab`, `BSpace`); Enter is issued as a
/// separate invocation so it's interpreted as the Return key.
pub fn send_command(target: &str, command: &str) -> Result<(), String> {
    run_tmux_capture(&["send-keys", "-t", target, "-l", command])?;
    run_tmux_capture(&["send-keys", "-t", target, "Enter"]).map(|_| ())
}

/// Kill the tmux window identified by `window_id` (e.g. `@7`).
pub fn kill_window(window_id: &str) -> Result<(), String> {
    run_tmux_capture(&["kill-window", "-t", window_id]).map(|_| ())
}

pub fn select_pane(pane_id: &str) {
    // One probe returns both targets (the audit's J2: this jump path
    // paid two `display-message` spawns here plus three more below),
    // and the three state changes chain into a single tmux client
    // process via tmux's `;` separator — one round trip instead of
    // five serial ones, in the same order.
    let probe = display_message(pane_id, "#{session_id}|#{window_id}");
    let (session_id, window_id) = match probe.split_once('|') {
        Some((session_id, window_id)) => (session_id, window_id),
        None => ("", ""),
    };
    let mut argv: Vec<&str> = Vec::new();
    if !session_id.is_empty() {
        argv.extend(["switch-client", "-t", session_id, ";"]);
    }
    if !window_id.is_empty() {
        argv.extend(["select-window", "-t", window_id, ";"]);
    }
    argv.extend(["select-pane", "-t", pane_id]);
    let _ = run_tmux(&argv);
}

/// Switch the invoking tmux client to `session`'s window `index` — the
/// click action for a window header row (`index:name`, the
/// `@sidebar_session_windows` / `@sidebar_worktree_windows` level; the
/// label is tmux's own window selector, so the click does what the
/// text says). Same chain as [`select_pane`] minus the pane:
/// `switch-client` first moves the client to the session (needed when
/// the target lives in another session, a no-op when it is already
/// there), then `select-window` picks the window inside it — chaining
/// both matters, because `switch-client` alone would land on the
/// session's last-active window, which may not be the header the user
/// clicked. The window keeps its own active pane, so no
/// `select-pane` follows.
pub fn switch_window(session: &str, index: i64) {
    let window_target = format!("{session}:{index}");
    let _ = run_tmux(&[
        "switch-client",
        "-t",
        session,
        ";",
        "select-window",
        "-t",
        &window_target,
    ]);
}

/// Test-only seam that makes every `run_tmux` call fail for the current
/// thread until the guard is dropped. Used to simulate a tmux server that
/// is unreachable (restart, socket hiccup) without spawning a real tmux.
#[cfg(test)]
pub mod test_fail_tmux {
    use std::cell::Cell;

    thread_local! {
        static FAIL: Cell<bool> = const { Cell::new(false) };
    }

    /// Make `run_tmux` return `None` for the current thread. Returns a
    /// guard that restores real tmux execution on drop.
    pub fn install() -> FailGuard {
        FAIL.with(|f| f.set(true));
        FailGuard
    }

    pub struct FailGuard;

    impl Drop for FailGuard {
        fn drop(&mut self) {
            FAIL.with(|f| f.set(false));
        }
    }

    pub(super) fn should_fail() -> bool {
        FAIL.with(|f| f.get())
    }
}
