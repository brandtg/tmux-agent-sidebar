pub mod capture;
mod hook;
mod label;
pub(crate) mod layouts;
pub(crate) mod notify_focus;
mod overview;
pub mod plugin_state;
pub(crate) mod setup;
pub(crate) mod shared_html;
mod spawn;
mod switch;
mod todo;
pub(crate) mod toggle;

pub(crate) use overview::spawn_overview_popup_detached;

use std::io::Read;

use crate::tmux;

/// Run a CLI subcommand. Returns Some(exit_code) if a subcommand was matched
/// (including the usage error for an unknown one — a typo must not launch
/// the TUI, audit 2026-11-25 L-cli-5), None if no subcommand was given
/// (caller should launch TUI).
pub fn run(args: &[String]) -> Option<i32> {
    let cmd = args.first().map(|s| s.as_str())?;
    let rest = &args[1..];
    let code = match cmd {
        "setup" => setup::cmd_setup(rest),
        "hook" => hook::cmd_hook(rest),
        "toggle" => toggle::cmd_toggle(rest),
        "toggle-all" => toggle::cmd_toggle_all(rest),
        "auto-close" => toggle::cmd_auto_close(rest),
        "maintain-width" => toggle::cmd_maintain_width(rest),
        "layouts" => layouts::cmd_layouts(rest),
        "set-status" => cmd_set_status(rest),
        "spawn" => spawn::cmd_spawn(rest),
        "todo" => todo::cmd_todo(rest),
        "capture" => capture::cmd_capture(rest),
        "overview" => overview::cmd_overview(rest),
        "switch" => switch::cmd_switch(rest),
        "notify-focus" => notify_focus::cmd_notify_focus(rest),
        "--version" | "version" => {
            println!("{}", crate::VERSION);
            0
        }
        // A typo'd subcommand must not fall through to the TUI the way a
        // bare invocation does — print usage and exit 2 instead (audit
        // 2026-11-25 L-cli-5). No args at all is the only TUI path.
        _ => {
            eprintln!("unknown subcommand '{cmd}'");
            eprintln!(
                "usage: tmux-agent-sidebar \
                 [setup|hook|toggle|toggle-all|auto-close|maintain-width|layouts|set-status|\
                 spawn|todo|capture|overview|switch|notify-focus|version]"
            );
            2
        }
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

fn set_status(pane: &str, status: &str) {
    use crate::tmux::PaneOptionWrite;
    if status == "clear" {
        tmux::apply_pane_options(
            pane,
            &[
                PaneOptionWrite::Unset {
                    key: tmux::PANE_STATUS,
                },
                PaneOptionWrite::Unset {
                    key: tmux::PANE_ATTENTION,
                },
            ],
        );
    } else {
        let mut writes = vec![PaneOptionWrite::Set {
            key: tmux::PANE_STATUS,
            value: status,
        }];
        match status {
            "running" | "idle" => {
                writes.push(PaneOptionWrite::Unset {
                    key: tmux::PANE_ATTENTION,
                });
            }
            _ => {}
        }
        tmux::apply_pane_options(pane, &writes);
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
    fn run_exits_2_with_usage_for_unknown_command() {
        // A typo must not fall through to the TUI the way a bare
        // invocation does (audit 2026-11-25 L-cli-5).
        assert_eq!(run(&["unknown-cmd".into()]), Some(2));
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
}
