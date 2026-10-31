use crate::time::now_epoch_secs;
use crate::tmux;
use crate::tool_name::CanonicalTool;

use super::super::label::extract_tool_label;
use super::super::{sanitize_tmux_value, set_status};
use super::context::pane_writes_allowed;

/// Write a single activity entry to the log file and trim if needed.
///
/// Line format is `epoch-seconds|tool|label`. The epoch (not a bare
/// `HH:MM`) is what lets the feed order entries correctly across
/// midnight; the display time is derived on the reader side. Lines
/// written before the epoch format (`HH:MM|tool|label`) still parse,
/// with their date treated as unknown (they sort oldest).
///
/// Hook events are independent processes appending to the same file, so
/// the append and the trim run under an exclusive `flock`: the old
/// trim (read whole file → truncate-write) raced concurrent appends and
/// silently dropped entries — this log also feeds task-progress parsing,
/// so lost lines corrupt the task counters.
pub(super) fn write_activity_entry(pane: &str, tool_name: &str, label: &str) {
    let log_path = crate::activity::log_file_path(pane);
    // Both fields are agent-controlled. The record is `ts|tool|label`, so
    // a newline or pipe in either one would split the record or forge a
    // fake TaskCreate/TaskUpdate line that the task parser consumes.
    let tool_name = sanitize_tmux_value(tool_name);
    let label = sanitize_tmux_value(label);
    let timestamp = now_epoch_secs().to_string();
    let line = format!("{}|{}|{}\n", timestamp, tool_name, label);

    use std::io::{Seek, SeekFrom, Write};
    // Read+write (no O_APPEND): the trim rewrites in place through the
    // same fd. Holding the lock makes the manual end-seek equivalent to
    // O_APPEND. O_NOFOLLOW + 0600 creation — see `crate::paths`.
    let Ok(mut f) = crate::paths::open_read_write_private(&log_path) else {
        crate::debug::log(&format!(
            "activity: failed to open {} for append",
            log_path.display()
        ));
        return;
    };
    if lock_exclusive(&f) {
        let _ = f.seek(SeekFrom::End(0));
        let _ = f.write_all(line.as_bytes());
        trim_locked(&mut f, 200, 210);
    }
    // The lock is released when the fd closes.
}

/// Exclusive flock on an open file; false if the lock could not be
/// taken. Shared by the activity-log append/trim and the subagent-list
/// read-modify-write, which serialize independent hook processes.
pub(in crate::cli::hook) fn lock_exclusive(file: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd;
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) == 0 }
}

/// Rewrite the file in place with only its last `keep` lines when it
/// exceeds `threshold` lines. Caller must hold the flock; all reads and
/// writes go through the same fd so no second open can deadlock on our
/// own lock.
fn trim_locked(file: &mut std::fs::File, keep: usize, threshold: usize) {
    use std::io::{Read, Seek, SeekFrom, Write};
    let _ = file.seek(SeekFrom::Start(0));
    let mut content = String::new();
    if file.read_to_string(&mut content).is_err() {
        return;
    }
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= threshold {
        return;
    }
    let kept = lines[lines.len() - keep..].join("\n") + "\n";
    let _ = file.seek(SeekFrom::Start(0));
    if file.write_all(kept.as_bytes()).is_ok() {
        let _ = file.set_len(kept.len() as u64);
    }
}

/// Trim a log file to `keep` lines when it exceeds `threshold` lines.
/// Path-based seam for tests; production writes trim through
/// `write_activity_entry` under the same lock.
#[cfg(test)]
pub(super) fn trim_log_file(path: &std::path::Path, keep: usize, threshold: usize) {
    let Ok(mut f) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
    else {
        return;
    };
    if lock_exclusive(&f) {
        trim_locked(&mut f, keep, threshold);
    }
}

/// Activity-log handler, called from `hook <agent> activity-log` event.
pub(super) fn handle_activity_log(
    pane: &str,
    agent_name: &str,
    tool_name: &str,
    tool_input: &serde_json::Value,
    tool_response: &serde_json::Value,
) -> i32 {
    let label = extract_tool_label(tool_name, tool_input, tool_response);
    if is_background_bash(tool_name, tool_input) {
        let stored = if label.is_empty() {
            tmux::BG_CMD_PLACEHOLDER
        } else {
            label.as_str()
        };
        tmux::set_pane_option(pane, tmux::PANE_BG_CMD, &sanitize_tmux_value(stored));
        // Track the spawned process by PID when the result reports one
        // (Hermes' `terminal(background=true)` returns `pid`). The ps
        // sweep compares the stored raw command against shell-expanded
        // ps args, which never match once the command uses quotes or
        // `$(…)` substitution — a live PID is immune to that rewrite.
        // Overwrite-or-clear every spawn: a stale PID from a previous
        // shell must not keep the next command's marker alive.
        match bg_pid_from_response(tool_response) {
            Some(pid) => tmux::set_pane_option(pane, tmux::PANE_BG_PID, &pid.to_string()),
            None => tmux::unset_pane_option(pane, tmux::PANE_BG_PID),
        }
    }

    let current_status = tmux::get_pane_option_value(pane, tmux::PANE_STATUS);
    // Permission/question prompts are cleared by something other than a
    // tool result, so a tool event landing while the prompt is open must
    // not flip the pane back to `running` and hide it. OpenCode runs a
    // step's tool calls in parallel and clears its prompts with an
    // explicit reply event. Antigravity has no waiting hook at all — its
    // waiting state is maintained by the sidebar's approval-prompt scan
    // worker (see `approval_scan_loop`) — and its own PostToolUse churn
    // (sibling completions in a parallel step, internal pseudo-tools like
    // `__task_reset__`) lands while a prompt is open and would stomp the
    // flag; the worker itself clears the state once the prompt disappears.
    // Claude has no prompt pending at PostToolUse time — its `PostToolUse`
    // IS the resume signal — so it stays unguarded.
    let pending_action = (agent_name == crate::tmux::OPENCODE_AGENT
        || agent_name == crate::tmux::ANTIGRAVITY_AGENT)
        && current_status == "waiting"
        && super::handlers::is_pending_action_wait_reason(&tmux::get_pane_option_value(
            pane,
            tmux::PANE_WAIT_REASON,
        ));
    // Hermes is excluded from the promotion below. Its turns are bracketed
    // by `pre_llm_call` (→ running) and `post_llm_call` (→ idle), but its
    // background forks (skill review, side questions) emit `post_tool_call`
    // while the turn-end hook is deliberately suppressed upstream
    // (`_apply_output_hooks` gates `post_llm_call` on `_persist_disabled`,
    // which the fork sets) — so a fork's tool events would promote the idle
    // pane to `running` with no stop ever coming, glowing forever until the
    // next real turn ends. For every other agent a tool call really is
    // unambiguous activity, so the inference stays.
    // agy's internal pseudo-tools (`__task_reset__` and friends, matched by
    // their `__` prefix) are state churn, not user-visible work: agy fires
    // one at boot and after tool batches, so promoting off them would show
    // "working" while agy sits at its input box between turns.
    // An empty status is promoted too: a tool just ran in this pane, so the
    // agent is unambiguously active. The old empty-status guard left
    // bootstrapped OpenCode panes (claimed without a run-state event, e.g.
    // when two same-directory sessions raced for bindings) blue forever.
    let agy_internal_churn =
        agent_name == crate::tmux::ANTIGRAVITY_AGENT && tool_name.starts_with("__");
    if agent_name != crate::tmux::HERMES_AGENT
        && !agy_internal_churn
        && current_status != "running"
        && !pending_action
    {
        set_status(pane, "running");
        // Any resume into `running` clears a stale wait reason, not just
        // resumes from `waiting`: `on_stop_failure` stores error text in
        // @pane_wait_reason under status `error`, and the renderer shows
        // the reason row for every status. The pending-action guard above
        // is what keeps a live prompt intact, not this gate.
        tmux::unset_pane_option(pane, tmux::PANE_ATTENTION);
        tmux::unset_pane_option(pane, tmux::PANE_WAIT_REASON);
        let existing_started = tmux::get_pane_option_value(pane, tmux::PANE_STARTED_AT);
        if existing_started.is_empty() {
            tmux::set_pane_option(pane, tmux::PANE_STARTED_AT, &now_epoch_secs().to_string());
        }
    }

    // Update permission mode when plan mode tools are used.
    // Same parent-protection rule as `set_agent_meta`: a subagent that
    // enters/exits plan mode must not flip the parent pane's badge.
    if pane_writes_allowed(pane) {
        match tool_name {
            "EnterPlanMode" => {
                tmux::set_pane_option(pane, tmux::PANE_PERMISSION_MODE, "plan");
            }
            "ExitPlanMode" => {
                tmux::set_pane_option(pane, tmux::PANE_PERMISSION_MODE, "default");
            }
            _ => {}
        }
    }

    write_activity_entry(pane, tool_name, &label);
    0
}

fn is_background_bash(tool_name: &str, tool_input: &serde_json::Value) -> bool {
    tool_name == CanonicalTool::Bash.as_str()
        && ["run_in_background", "runInBackground"]
            .iter()
            .any(|key| tool_input.get(key).and_then(|v| v.as_bool()) == Some(true))
}

/// Extract the spawned process's PID from a background-shell tool
/// result. Hermes' `terminal(background=true)` returns
/// `{"output": …, "pid": <int>, …}`; Claude's `run_in_background`
/// payload carries only a string shell id, so callers fall back to the
/// command-string sweep for those. Numeric strings are accepted in case
/// an upstream serializes the id as text.
fn bg_pid_from_response(response: &serde_json::Value) -> Option<u32> {
    let pid = response.get("pid")?;
    pid.as_u64()
        .or_else(|| pid.as_str().and_then(|s| s.parse().ok()))
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use serde_json::json;
    use std::fs;

    // ─── trim_log_file tests ────────────────────────────────────────

    #[test]
    fn trim_log_file_under_threshold_no_change() {
        let dir = std::env::temp_dir();
        let path = dir.join("trim_test_under.log");
        fs::write(&path, "line1\nline2\nline3\n").unwrap();

        trim_log_file(&path, 2, 5);

        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 3);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn trim_log_file_over_threshold_trims() {
        let dir = std::env::temp_dir();
        let path = dir.join("trim_test_over.log");
        let lines: Vec<String> = (1..=15).map(|i| format!("line{}", i)).collect();
        fs::write(&path, lines.join("\n") + "\n").unwrap();

        trim_log_file(&path, 5, 10);

        let content = fs::read_to_string(&path).unwrap();
        let remaining: Vec<&str> = content.lines().collect();
        assert_eq!(remaining.len(), 5);
        assert_eq!(remaining[0], "line11");
        assert_eq!(remaining[4], "line15");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn trim_log_file_exactly_at_threshold_no_change() {
        let dir = std::env::temp_dir();
        let path = dir.join("trim_test_exact.log");
        let lines: Vec<String> = (1..=10).map(|i| format!("line{}", i)).collect();
        fs::write(&path, lines.join("\n") + "\n").unwrap();

        trim_log_file(&path, 5, 10);

        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 10);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn trim_log_file_nonexistent_file_no_panic() {
        let dir = std::env::temp_dir();
        let path = dir.join("trim_test_nonexistent.log");
        let _ = fs::remove_file(&path);
        trim_log_file(&path, 5, 10);
    }

    // ─── write_activity_entry tests ─────────────────────────────────

    #[test]
    fn write_activity_entry_creates_and_appends() {
        let pane_id = "%CLI_WRITE_TEST";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        write_activity_entry(pane_id, "Read", "main.rs");
        write_activity_entry(pane_id, "Edit", "lib.rs");

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("|Read|main.rs"));
        assert!(lines[1].ends_with("|Edit|lib.rs"));
        // The timestamp field is epoch seconds on the wire (digits, not
        // the old `HH:MM`); both lines must carry a plausible recent value.
        for line in &lines {
            let ts: u64 = line.split('|').next().unwrap().parse().unwrap();
            assert!(ts >= 1_000_000_000, "expected a recent epoch, got {ts}");
        }
        fs::remove_file(&path).ok();
    }

    #[test]
    fn write_activity_entry_sanitizes_label() {
        let pane_id = "%CLI_SANITIZE_TEST";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        write_activity_entry(pane_id, "Bash", "cat file | grep foo\nbar");

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "newlines in label should not create extra lines"
        );
        let label = lines[0].splitn(3, '|').nth(2).unwrap();
        assert!(!label.contains('|'));
        assert!(!label.contains('\n'));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn write_activity_entry_sanitizes_tool_name() {
        let pane_id = "%CLI_SANITIZE_TOOL_TEST";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        // A malicious/odd tool name must not split the `ts|tool|label`
        // record or forge an extra TaskCreate entry.
        write_activity_entry(pane_id, "TaskCreate|injected\nline", "ok");

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1, "tool_name must not create extra lines");
        let mut fields = lines[0].split('|');
        assert!(fields.next().unwrap().parse::<u64>().is_ok());
        assert_eq!(fields.next().unwrap(), "TaskCreate injected line");
        assert_eq!(fields.next().unwrap(), "ok");
        assert_eq!(fields.next(), None, "record must have exactly 3 fields");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn write_activity_entry_trims_at_threshold() {
        let pane_id = "%CLI_TRIM_TEST";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        for i in 1..=215 {
            write_activity_entry(pane_id, "Read", &format!("file{}.rs", i));
        }

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert!(lines.len() <= 210, "should be trimmed, got {}", lines.len());
        assert!(lines.last().unwrap().ends_with("|Read|file215.rs"));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn concurrent_appends_survive_trim() {
        // Regression: hook events are independent processes. The old
        // read-then-truncate-write trim dropped entries appended while a
        // trim was in flight; under the flock every concurrent append
        // must land and survive the trim.
        let pane_id = "%CLI_CONCURRENT";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        let old: String = (1..=205)
            .map(|i| format!("00:00|Read|old{}.rs\n", i))
            .collect();
        fs::write(&path, old).unwrap();

        let handles: Vec<_> = (0..20)
            .map(|i| {
                let pane = pane_id.to_string();
                std::thread::spawn(move || {
                    write_activity_entry(&pane, "Bash", &format!("cmd{}", i));
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert!(
            (200..=210).contains(&lines.len()),
            "trim must keep the list within bounds, got {}",
            lines.len()
        );
        for i in 0..20 {
            let needle = format!("|cmd{}", i);
            assert!(
                lines.iter().any(|l| l.ends_with(&needle)),
                "concurrent entry {i} must not be dropped by the trim"
            );
        }
        fs::remove_file(&path).ok();
    }

    // ─── handle_activity_log tests ──────────────────────────────────

    #[test]
    fn handle_activity_log_writes_entry() {
        let pane_id = "%CLI_HANDLE_TEST";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        handle_activity_log(
            pane_id,
            "claude",
            "Read",
            &json!({"file_path": "/home/user/src/main.rs"}),
            &Value::Null,
        );

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("|Read|main.rs"));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_marks_background_bash() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_BG_BASH";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        handle_activity_log(
            pane_id,
            "claude",
            "Bash",
            &json!({"command": "npm run dev", "run_in_background": true}),
            &Value::Null,
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_BG_CMD).as_deref(),
            Some("npm run dev"),
            "command string must be stored so the row body can show what is running",
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_background_bash_without_command_falls_back_to_placeholder() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_BG_BASH_NO_CMD";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        handle_activity_log(
            pane_id,
            "claude",
            "Bash",
            &json!({"run_in_background": true}),
            &Value::Null,
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_BG_CMD).as_deref(),
            Some(tmux::BG_CMD_PLACEHOLDER)
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_sanitizes_command_before_writing() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_BG_BASH_PIPE";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        handle_activity_log(
            pane_id,
            "claude",
            "Bash",
            &json!({"command": "cat a.log | grep foo\nbar", "run_in_background": true}),
            &Value::Null,
        );

        let stored = tmux::test_mock::get(pane_id, tmux::PANE_BG_CMD).unwrap_or_default();
        assert!(!stored.contains('|'));
        assert!(!stored.contains('\n'));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_stores_pid_from_spawn_result() {
        // Hermes' `terminal(background=true)` result carries the spawned
        // process's PID; storing it lets the sweep verify liveness by PID
        // instead of matching the raw command against shell-expanded ps
        // args (which quoting / `$(…)` substitution always break).
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_BG_PID";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        handle_activity_log(
            pane_id,
            "hermes",
            "Bash",
            // Post-normalization shape (the Hermes adapter maps
            // `terminal(background=true)` onto `run_in_background`).
            &json!({"command": "opencode run --title \"m13\" \"$(cat prompt.md)\"", "run_in_background": true}),
            &json!({"output": "Background process started", "pid": 1126098}),
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_BG_PID).as_deref(),
            Some("1126098")
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_accepts_string_pid_and_drops_stale_one() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_BG_PID_STALE";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        handle_activity_log(
            pane_id,
            "hermes",
            "Bash",
            &json!({"command": "npm run dev", "run_in_background": true}),
            &json!({"pid": "4242"}),
        );
        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_BG_PID).as_deref(),
            Some("4242")
        );

        // The next spawn without a PID must not inherit the old one: the
        // sweep would otherwise keep the new command alive on a dead PID.
        handle_activity_log(
            pane_id,
            "claude",
            "Bash",
            &json!({"command": "cargo watch", "run_in_background": true}),
            &Value::Null,
        );
        assert!(
            !tmux::test_mock::contains(pane_id, tmux::PANE_BG_PID),
            "stale PID must be cleared when the new spawn reports none"
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_ignores_non_positive_pid() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_BG_PID_INVALID";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        handle_activity_log(
            pane_id,
            "hermes",
            "Bash",
            &json!({"command": "npm run dev", "run_in_background": true}),
            &json!({"pid": 0}),
        );
        assert!(!tmux::test_mock::contains(pane_id, tmux::PANE_BG_PID));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_preserves_bg_cmd_when_background_resumes_running() {
        // Regression: a tool-use burst must not clear @pane_bg_cmd, or
        // the next Stop would land in `idle` instead of `background`.
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_BG_PRESERVE";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "background");
        tmux::test_mock::set(pane_id, tmux::PANE_BG_CMD, "npm run dev");

        handle_activity_log(
            pane_id,
            "claude",
            "Read",
            &json!({"file_path": "/home/user/src/main.rs"}),
            &Value::Null,
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("running")
        );
        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_BG_CMD).as_deref(),
            Some("npm run dev")
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_waiting_transitions_to_running_on_tool_use() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_WAIT_TO_RUN";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "waiting");
        tmux::test_mock::set(pane_id, tmux::PANE_ATTENTION, "notification");
        tmux::test_mock::set(pane_id, tmux::PANE_WAIT_REASON, "permission_prompt");

        handle_activity_log(
            pane_id,
            "claude",
            "Read",
            &json!({"file_path": "/home/user/src/main.rs"}),
            &Value::Null,
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("running")
        );
        assert!(!tmux::test_mock::contains(pane_id, tmux::PANE_ATTENTION));
        assert!(!tmux::test_mock::contains(pane_id, tmux::PANE_WAIT_REASON));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_empty_status_transitions_to_running() {
        // Regression: an empty @pane_status stayed blue forever — the
        // empty-status guard refused the promotion even though a tool just
        // ran, so a bootstrapped OpenCode pane (claimed without a
        // run-state event) never went green.
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_EMPTY_TO_RUN";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        handle_activity_log(
            pane_id,
            "opencode",
            "Read",
            &json!({"file_path": "/home/user/src/main.rs"}),
            &Value::Null,
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("running")
        );
        assert!(tmux::test_mock::contains(pane_id, tmux::PANE_STARTED_AT));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_error_transitions_to_running_clears_wait_reason() {
        // Regression: `on_stop_failure` stores the error text in
        // @pane_wait_reason while status is `error`, and the renderer
        // shows the reason row for any status. The next tool use must
        // clear it, not only when resuming from `waiting`.
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_ERR_TO_RUN";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "error");
        tmux::test_mock::set(pane_id, tmux::PANE_ATTENTION, "notification");
        tmux::test_mock::set(pane_id, tmux::PANE_WAIT_REASON, "rate_limit");

        handle_activity_log(
            pane_id,
            "claude",
            "Read",
            &json!({"file_path": "/home/user/src/main.rs"}),
            &Value::Null,
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("running")
        );
        assert!(
            !tmux::test_mock::contains(pane_id, tmux::PANE_WAIT_REASON),
            "stale error wait_reason must not survive the resume to running"
        );
        assert!(!tmux::test_mock::contains(pane_id, tmux::PANE_ATTENTION));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_keeps_opencode_permission_wait_sticky() {
        // Regression: OpenCode runs a step's tool calls in parallel, so a
        // sibling tool's result can land while a permission prompt is open.
        // It must not clear the wait — only the explicit reply does.
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_OC_PERM_STICKY";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "waiting");
        tmux::test_mock::set(pane_id, tmux::PANE_ATTENTION, "notification");
        tmux::test_mock::set(pane_id, tmux::PANE_WAIT_REASON, "permission");

        handle_activity_log(
            pane_id,
            "opencode",
            "Read",
            &json!({"file_path": "/home/user/src/main.rs"}),
            &Value::Null,
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("waiting"),
            "a concurrent tool result must not clear a pending permission"
        );
        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_WAIT_REASON).as_deref(),
            Some("permission")
        );
        assert!(tmux::test_mock::contains(pane_id, tmux::PANE_ATTENTION));
        // The activity entry is still recorded.
        assert!(fs::read_to_string(&path).unwrap().contains("|Read|main.rs"));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_keeps_opencode_question_wait_sticky() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_OC_QUESTION_STICKY";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "waiting");
        tmux::test_mock::set(pane_id, tmux::PANE_WAIT_REASON, "question");

        handle_activity_log(
            pane_id,
            "opencode",
            "Read",
            &json!({"file_path": "/home/user/src/main.rs"}),
            &Value::Null,
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("waiting")
        );
        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_WAIT_REASON).as_deref(),
            Some("question")
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_keeps_antigravity_permission_wait_sticky() {
        // Regression: Antigravity's waiting state is maintained by the
        // approval-prompt scan worker, and agy's own PostToolUse churn
        // (parallel sibling completions, internal pseudo-tools like
        // `__task_reset__`) lands while a permission prompt is open. A
        // tool result must not clear the wait — only the worker's rescan
        // (prompt gone) does.
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_AGY_PERM_STICKY";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "waiting");
        tmux::test_mock::set(pane_id, tmux::PANE_ATTENTION, "notification");
        tmux::test_mock::set(pane_id, tmux::PANE_WAIT_REASON, "permission");

        handle_activity_log(
            pane_id,
            "antigravity",
            "Read",
            &json!({"file_path": "/home/user/src/main.rs"}),
            &Value::Null,
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("waiting"),
            "a concurrent tool result must not clear a pending permission"
        );
        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_WAIT_REASON).as_deref(),
            Some("permission")
        );
        assert!(tmux::test_mock::contains(pane_id, tmux::PANE_ATTENTION));
        // The activity entry is still recorded.
        assert!(fs::read_to_string(&path).unwrap().contains("|Read|main.rs"));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_keeps_antigravity_question_wait_sticky() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_AGY_QUESTION_STICKY";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "waiting");
        tmux::test_mock::set(pane_id, tmux::PANE_WAIT_REASON, "question");

        handle_activity_log(
            pane_id,
            "antigravity",
            "__task_reset__",
            &json!({}),
            &Value::Null,
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("waiting")
        );
        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_WAIT_REASON).as_deref(),
            Some("question")
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_agy_internal_pseudo_tool_does_not_promote() {
        // agy fires `__task_reset__` at boot and after tool batches; it is
        // state churn, not user-visible work, and must not light up an
        // idle pane sitting at agy's input box between turns.
        let _guard = tmux::test_mock::install();
        let pane_id = "%AGY_CHURN_IDLE";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "idle");

        handle_activity_log(
            pane_id,
            "antigravity",
            "__task_reset__",
            &json!({}),
            &Value::Null,
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("idle"),
            "internal agy state churn must not show the pane as working"
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_agy_real_tool_still_promotes_idle() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%AGY_REAL_TOOL";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "idle");

        handle_activity_log(
            pane_id,
            "antigravity",
            "Read",
            &json!({"file_path": "/home/user/src/main.rs"}),
            &Value::Null,
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("running")
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_opencode_non_action_wait_still_resumes() {
        // Only pending-action reasons are sticky; an ordinary waiting pane
        // (e.g. a soft notification) still resumes on tool use.
        let _guard = tmux::test_mock::install();
        let pane_id = "%CLI_OC_SOFT_WAIT";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "waiting");
        tmux::test_mock::set(pane_id, tmux::PANE_WAIT_REASON, "rate_limit");

        handle_activity_log(
            pane_id,
            "opencode",
            "Read",
            &json!({"file_path": "/home/user/src/main.rs"}),
            &Value::Null,
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("running")
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_hermes_tool_event_does_not_promote_idle_pane() {
        // Hermes background forks (skill review, side questions) emit
        // `post_tool_call` while their turn-end `post_llm_call` is
        // deliberately suppressed upstream — a fork's tool events on an
        // idle pane must not flip it to `running` with no stop ever coming.
        let _guard = tmux::test_mock::install();
        let pane_id = "%HERMES_NO_PROMOTE";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "idle");

        handle_activity_log(pane_id, "hermes", "skill_view", &json!({}), &Value::Null);

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("idle"),
            "fork tool events must not promote a hermes pane it will never stop"
        );
        assert!(
            !tmux::test_mock::contains(pane_id, tmux::PANE_STARTED_AT),
            "no run clock without a pre_llm_call bracket"
        );
        // The activity entry itself must still land.
        assert!(fs::read_to_string(&path).unwrap().contains("|skill_view|"));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_hermes_tool_event_keeps_waiting_state() {
        // Same gate, waiting variant: a hermes question prompt stays put
        // when a fork's tool result lands (no promotion, no clears).
        let _guard = tmux::test_mock::install();
        let pane_id = "%HERMES_WAIT_KEEP";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "waiting");
        tmux::test_mock::set(pane_id, tmux::PANE_WAIT_REASON, "question");
        tmux::test_mock::set(pane_id, tmux::PANE_ATTENTION, "notification");

        handle_activity_log(
            pane_id,
            "hermes",
            "Read",
            &json!({"file_path": "/a.rs"}),
            &Value::Null,
        );

        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("waiting")
        );
        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_WAIT_REASON).as_deref(),
            Some("question")
        );
        assert!(tmux::test_mock::contains(pane_id, tmux::PANE_ATTENTION));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_empty_tool_name_does_nothing() {
        let pane_id = "%CLI_EMPTY_TOOL";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        // With the adapter pattern, empty tool_name is filtered by the adapter
        // before reaching handle_activity_log. We still test that handle_activity_log
        // writes an entry even with empty tool_name (label extraction handles it).
        let result = handle_activity_log(pane_id, "claude", "", &Value::Null, &Value::Null);
        assert_eq!(result, 0);
        // Empty tool_name still writes an entry now (adapter filters upstream)
    }

    #[test]
    fn handle_activity_log_tool_input_as_json_object() {
        let pane_id = "%CLI_JSON_STR";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        handle_activity_log(
            pane_id,
            "claude",
            "Edit",
            &json!({"file_path": "/a/b/test.rs"}),
            &Value::Null,
        );

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("|Edit|test.rs"));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_null_tool_input_uses_empty_label() {
        let pane_id = "%CLI_NULL_INPUT";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        handle_activity_log(pane_id, "claude", "UnknownTool", &Value::Null, &Value::Null);

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("|UnknownTool|"));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_task_create_with_response() {
        let pane_id = "%CLI_TASK_CREATE";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);

        handle_activity_log(
            pane_id,
            "claude",
            "TaskCreate",
            &json!({"subject": "Fix bug"}),
            &json!({"task": {"id": "42"}}),
        );

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("|TaskCreate|#42 Fix bug"));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn handle_activity_log_enter_plan_mode_blocked_by_subagents() {
        let _guard = tmux::test_mock::install();
        let pane = "%PARENT_PLAN";
        tmux::test_mock::set(pane, tmux::PANE_SUBAGENTS, "Explore:sub-1");
        tmux::test_mock::set(pane, tmux::PANE_PERMISSION_MODE, "default");

        // A subagent's EnterPlanMode tool use must not flip the parent
        // badge to "plan".
        handle_activity_log(pane, "claude", "EnterPlanMode", &Value::Null, &Value::Null);

        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_PERMISSION_MODE).as_deref(),
            Some("default"),
            "child EnterPlanMode must not overwrite parent's permission_mode"
        );
    }
}
