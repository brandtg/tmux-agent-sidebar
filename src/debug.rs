//! Opt-in debug tracing, gated by `TMUX_AGENT_SIDEBAR_DEBUG=1`.
//!
//! Hook dispatch and tmux writes are deliberately silent in production —
//! hook processes must never pollute the agent's stdout/stderr, and a
//! failed pane-option write is not worth crashing over. The cost is that
//! "why is my sidebar wrong" is undebuggable: an unknown agent name, a
//! missing `TMUX_PANE`, a rejected payload, and a failed `set-option` all
//! vanish with exit 0. When this env var is set, the normally-swallowed
//! decisions and failures are appended, one line each, to
//! `$XDG_RUNTIME_DIR/tmux-agent-sidebar-debug.log` (`/tmp` fallback).
//!
//! Set the variable in the environment of the process you want to trace —
//! the agent's hook invocations and/or the sidebar TUI. The file is shared
//! between all of a user's sidebar processes; every entry is tagged with
//! the tmux pane (when known) and the writer's pid.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::time::now_epoch_millis;

pub(crate) const ENV_VAR: &str = "TMUX_AGENT_SIDEBAR_DEBUG";

/// Cap on the debug log size. A failure loop on the TUI refresh path
/// (e.g. polling a dead tmux server once per second) would otherwise fill
/// the tmpfs runtime dir unbounded.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Whether tracing is on for this process. Cached: hook processes are
/// short-lived and the TUI reads it once at startup.
pub(crate) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| match std::env::var_os(ENV_VAR) {
        Some(value) => !value.is_empty() && value != "0",
        None => false,
    })
}

pub(crate) fn log_file_path() -> PathBuf {
    crate::paths::runtime_dir().join("tmux-agent-sidebar-debug.log")
}

/// Append one timestamped line. Best-effort: open and write failures are
/// swallowed — tracing must never turn a silent failure into a crash or
/// slow down the caller.
pub(crate) fn log(message: &str) {
    if !enabled() {
        return;
    }
    let Some(mut file) = open_log() else {
        return;
    };
    let pane = std::env::var("TMUX_PANE").unwrap_or_default();
    let tag = if pane.is_empty() {
        format!("pid {}", std::process::id())
    } else {
        format!("{pane} pid {}", std::process::id())
    };
    // Keep the file line-oriented whatever the message embeds.
    let flat = message.replace(['\n', '\r'], "\\n");
    let _ = file.write_all(format!("{} [{}] {}\n", timestamp(), tag, flat).as_bytes());
}

fn open_log() -> Option<std::fs::File> {
    let path = log_file_path();
    // symlink_metadata never follows a planted symlink; only our own
    // regular files get rotated.
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if meta.is_file() && meta.len() > MAX_LOG_BYTES {
            let _ = std::fs::remove_file(&path);
        }
    }
    crate::paths::open_append_private(&path).ok()
}

fn timestamp() -> String {
    let millis = now_epoch_millis();
    let secs = millis / 1000;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let epoch: libc::time_t = secs as libc::time_t;
    unsafe { libc::localtime_r(&epoch, &mut tm) };
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        millis % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_file_name_is_stable() {
        assert_eq!(
            log_file_path().file_name().and_then(|n| n.to_str()),
            Some("tmux-agent-sidebar-debug.log")
        );
    }

    #[test]
    fn disabled_by_default_is_a_no_op() {
        // Without the env var set, log() must not create the file. If the
        // whole test run was started with tracing on, the no-op property
        // doesn't apply — skip instead of failing.
        if std::env::var_os(ENV_VAR).is_some() {
            return;
        }
        let before = std::fs::symlink_metadata(log_file_path()).is_ok();
        log("test entry while disabled");
        let after = std::fs::symlink_metadata(log_file_path()).is_ok();
        assert_eq!(before, after);
    }
}
