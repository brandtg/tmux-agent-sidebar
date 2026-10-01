pub mod capture;
mod hook;
mod label;
mod notify_focus;
pub mod plugin_state;
pub(crate) mod setup;
pub(crate) mod shared_html;
mod spawn;
pub(crate) mod toggle;

use std::io::Read;

use crate::tmux;

/// Run a CLI subcommand. Returns Some(exit_code) if a subcommand was matched,
/// None if no subcommand was given (caller should launch TUI).
pub fn run(args: &[String]) -> Option<i32> {
    let cmd = args.first().map(|s| s.as_str())?;
    let rest = &args[1..];
    let code = match cmd {
        "setup" => setup::cmd_setup(rest),
        "hook" => hook::cmd_hook(rest),
        "toggle" => toggle::cmd_toggle(rest),
        "toggle-all" => toggle::cmd_toggle_all(rest),
        "auto-close" => toggle::cmd_auto_close(rest),
        "set-status" => cmd_set_status(rest),
        "spawn" => spawn::cmd_spawn(rest),
        "capture" => capture::cmd_capture(rest),
        "notify-focus" => notify_focus::cmd_notify_focus(rest),
        "--version" | "version" => {
            println!("{}", crate::VERSION);
            0
        }
        _ => return None,
    };
    Some(code)
}

// ─── Shared helpers ──────────────────────────────────────────────────────────

/// Read the hook payload from stdin. `None` means stdin could not be
/// read at all; a body that is not valid JSON yields `parsed == false`
/// so the caller can drop the event instead of degrading it to an
/// all-empty one — an empty `SessionEnd` or `TaskCompleted` parsed from
/// garbage stdin used to fire spurious teardowns and notifications. The
/// raw byte count and parse flag feed the opt-in debug trace, where
/// "invalid JSON" and "valid JSON the adapter doesn't map" are different
/// diagnoses. Manual invocation from a TTY (no piped payload) still
/// yields Null so subcommands behave as before.
fn read_stdin_json() -> Option<(serde_json::Value, usize, bool)> {
    let is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) != 0 };
    if is_tty {
        return Some((serde_json::Value::Null, 0, true));
    }
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return None;
    }
    let bytes = buf.len();
    match serde_json::from_str(&buf) {
        Ok(value) => Some((value, bytes, true)),
        Err(_) => Some((serde_json::Value::Null, bytes, false)),
    }
}

fn tmux_pane() -> String {
    std::env::var("TMUX_PANE").unwrap_or_default()
}

fn local_time_hhmm() -> String {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

fn set_status(pane: &str, status: &str) {
    if status == "clear" {
        tmux::unset_pane_option(pane, tmux::PANE_STATUS);
        tmux::unset_pane_option(pane, tmux::PANE_ATTENTION);
    } else {
        tmux::set_pane_option(pane, tmux::PANE_STATUS, status);
        match status {
            "running" | "idle" => {
                tmux::unset_pane_option(pane, tmux::PANE_ATTENTION);
            }
            _ => {}
        }
    }
}

fn set_attention(pane: &str, state: &str) {
    if state == "clear" {
        tmux::unset_pane_option(pane, tmux::PANE_ATTENTION);
    } else {
        tmux::set_pane_option(pane, tmux::PANE_ATTENTION, state);
    }
}

/// Canonicalize a string before storing it in a tmux pane option. Thin
/// wrapper over [`crate::tmux::sanitize_option_value`] (which
/// `set_pane_option` now also applies as defense in depth); kept as a
/// separate entry point because activity-log lines use the same `|`
/// field-separator rules as tmux options.
pub(crate) fn sanitize_tmux_value(s: &str) -> String {
    tmux::sanitize_option_value(s)
}

// ─── set-status subcommand ──────────────────────────────────────────────────

fn cmd_set_status(args: &[String]) -> i32 {
    let status = match args.first() {
        Some(s) => s.as_str(),
        None => {
            crate::debug::log("set-status: ignored: no status argument");
            return 0;
        }
    };
    let pane = tmux_pane();
    if pane.is_empty() {
        crate::debug::log(&format!(
            "set-status: ignored: TMUX_PANE not set (status '{status}')"
        ));
        return 0;
    }
    set_status(&pane, status);
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::json_str;
    use serde_json::json;

    // ─── run() dispatch tests ─────────────────────────────────────────

    #[test]
    fn run_returns_none_for_empty_args() {
        assert_eq!(run(&[]), None);
    }

    #[test]
    fn run_returns_none_for_unknown_command() {
        assert_eq!(run(&["unknown-cmd".into()]), None);
    }

    #[test]
    fn run_returns_none_for_tui_mode_no_args() {
        assert_eq!(run(&[]), None);
    }

    // ─── json_str tests ──────────────────────────────────────────────

    #[test]
    fn json_str_extracts_string() {
        let val = json!({"name": "claude", "count": 42});
        assert_eq!(json_str(&val, "name"), "claude");
    }

    #[test]
    fn json_str_returns_empty_for_missing_key() {
        assert_eq!(json_str(&json!({"name": "claude"}), "missing"), "");
    }

    #[test]
    fn json_str_returns_empty_for_non_string() {
        assert_eq!(json_str(&json!({"count": 42}), "count"), "");
    }

    #[test]
    fn json_str_returns_empty_for_null() {
        assert_eq!(json_str(&serde_json::Value::Null, "anything"), "");
    }

    #[test]
    fn json_str_returns_empty_for_array() {
        assert_eq!(json_str(&json!({"arr": [1, 2]}), "arr"), "");
    }

    #[test]
    fn json_str_returns_empty_for_bool() {
        assert_eq!(json_str(&json!({"flag": true}), "flag"), "");
    }

    // ─── sanitize_tmux_value tests ───────────────────────────────────

    #[test]
    fn sanitize_replaces_newlines() {
        assert_eq!(
            sanitize_tmux_value("line1\nline2\nline3"),
            "line1 line2 line3"
        );
    }

    #[test]
    fn sanitize_replaces_pipes() {
        assert_eq!(sanitize_tmux_value("a|b|c"), "a b c");
    }

    #[test]
    fn sanitize_replaces_both() {
        assert_eq!(sanitize_tmux_value("a|b\nc"), "a b c");
    }

    #[test]
    fn sanitize_leaves_clean_text() {
        assert_eq!(sanitize_tmux_value("hello world"), "hello world");
    }

    #[test]
    fn sanitize_empty() {
        assert_eq!(sanitize_tmux_value(""), "");
    }

    #[test]
    fn sanitize_consecutive_pipes_and_newlines() {
        assert_eq!(sanitize_tmux_value("a||\n\nb"), "a    b");
    }

    // ─── local_time_hhmm tests ──────────────────────────────────────

    #[test]
    fn local_time_hhmm_format() {
        let t = local_time_hhmm();
        assert_eq!(t.len(), 5);
        assert_eq!(t.as_bytes()[2], b':');
        let h: u32 = t[..2].parse().unwrap();
        let m: u32 = t[3..].parse().unwrap();
        assert!(h < 24);
        assert!(m < 60);
    }
}
