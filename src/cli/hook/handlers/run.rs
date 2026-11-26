use crate::cli::{sanitize_tmux_value, set_attention, set_status};
use crate::desktop_notification;
use crate::desktop_notification::DesktopNotificationKind;
use crate::tmux;

use crate::time::{now_epoch_millis, now_epoch_secs};

use super::super::activity::write_activity_entry;
use super::super::context::{
    AgentContext, clear_run_state, is_system_message, mark_task_reset, set_agent_meta,
};
use super::super::notifications::{
    NotifyLabels, NotifyPayload, notification_run_id, notify_lifecycle, stop_body,
    stop_failure_body, stop_failure_fingerprint, task_completed_body, task_completed_fingerprint,
};
use super::status_priority::resolve_stop_status;
use super::subagent::with_subagent_lock;

pub(in crate::cli::hook) fn on_user_prompt_submit(
    pane: &str,
    ctx: &AgentContext<'_>,
    prompt: &str,
) -> i32 {
    set_agent_meta(pane, ctx);
    // One batched write for the notification stamp and the prompt
    // preview; the run clock and wait-reason clear below join it when
    // the state read succeeds (four separate spawns before, audit
    // 2026-11-19 C2).
    let run_id = now_epoch_millis().to_string();
    let prompt_value = if !prompt.is_empty() && !is_system_message(prompt) {
        Some(sanitize_tmux_value(prompt))
    } else {
        None
    };
    let mut writes = vec![tmux::PaneOptionWrite::Set {
        key: tmux::PANE_NOTIFICATION_RUN_ID,
        value: &run_id,
    }];
    if let Some(p) = &prompt_value {
        writes.push(tmux::PaneOptionWrite::Set {
            key: tmux::PANE_PROMPT,
            value: p,
        });
        writes.push(tmux::PaneOptionWrite::Set {
            key: tmux::PANE_PROMPT_SOURCE,
            value: "user",
        });
    }
    // One batched read for the three options the promotion logic inspects
    // (audit 2026-11-19 C2): status, wait reason, and the run clock used
    // to be three separate spawns on the hottest hook event.
    let Some(reads) = tmux::get_pane_options(
        pane,
        &[
            tmux::PANE_STATUS,
            tmux::PANE_WAIT_REASON,
            tmux::PANE_STARTED_AT,
        ],
    ) else {
        // A failed read must not read as an unclaimed pane (audit
        // 2026-11-26 L-hook-3): skip the status promotion, the run-clock
        // stamp, and the wait-reason clear — all three decide from prior
        // pane state — and let the next event re-decide. The identity
        // writes above are unconditional overwrites and still land.
        tmux::apply_pane_options(pane, &writes);
        return 0;
    };
    let current_status = reads.first().map(String::as_str).unwrap_or_default();
    let wait_reason = reads.get(1).map(String::as_str).unwrap_or_default();
    let existing_started = reads.get(2).map(String::as_str).unwrap_or_default();
    // agy's `PreInvocation` maps here but fires before *every* model call
    // in a turn, not just the first — including the boot title-generation
    // call (empty recovered prompt) and calls that race an
    // approval-prompt scan flag (parallel sibling work). Neither is a
    // resume signal: a boot call must not light up an idle pane sitting
    // at agy's input box, and a pending prompt stays `waiting` until the
    // scan worker sees it disappear. OpenCode's submission is a genuine
    // user submit and Claude's fires once per turn, so neither needs the
    // guard. A real agy turn still promotes: its recovered prompt is
    // non-empty, and its tool completions resume via the activity log.
    let agy_background_call = ctx.agent == crate::tmux::ANTIGRAVITY_AGENT
        && ((current_status == "idle" && prompt.is_empty())
            || (current_status == "waiting"
                && super::status_priority::is_pending_action_wait_reason(wait_reason)));
    if !agy_background_call {
        // set_status("running") unsets @pane_attention inside its batch —
        // the attention clear that used to precede it was the same write
        // twice (audit 2026-11-19 C2).
        set_status(pane, "running");
    }
    // Only a real user turn restarts the run clock. Synthetic empty-prompt
    // submissions — OpenCode fires one per assistant response via
    // `session.status busy` — must keep an existing clock running, or the
    // elapsed timer resets on every response instead of spanning the turn.
    // An empty prompt still stamps a missing clock so recovery paths
    // (retry → busy with no chat.message) start timing.
    // agy: a mid-turn PreInvocation carries a transcript-recovered prompt
    // that is never empty, so `!prompt.is_empty()` would restamp the clock
    // on *every* model call — the row would show elapsed-since-last-model-
    // call instead of turn length. While the turn is already running, only
    // a missing clock (recovery) stamps; a present clock keeps running.
    let agy_running = ctx.agent == crate::tmux::ANTIGRAVITY_AGENT && current_status == "running";
    let started_stamp = if agy_running && !existing_started.is_empty() {
        None
    } else if !prompt.is_empty() || existing_started.is_empty() {
        Some(now_epoch_secs().to_string())
    } else {
        None
    };
    if let Some(stamp) = &started_stamp {
        writes.push(tmux::PaneOptionWrite::Set {
            key: tmux::PANE_STARTED_AT,
            value: stamp,
        });
    }
    if !agy_background_call {
        writes.push(tmux::PaneOptionWrite::Unset {
            key: tmux::PANE_WAIT_REASON,
        });
    }
    tmux::apply_pane_options(pane, &writes);
    0
}

/// Claude `PreToolUse`: a tool call is about to execute.
///
/// This is the earliest "the agent took its next action" signal available —
/// upstream fires it before the permission decision, whereas `PostToolUse`
/// only lands after a tool call succeeds. That gap is what strands a
/// `PermissionDenied` warning: auto mode denies the call and the agent
/// continues the same turn, so the next event would only arrive when some
/// later tool happens to finish — a multi-minute build kept the stale
/// `waiting`/`permission_denied` row up for its whole run. Treating the
/// attempt itself as the resume signal clears the stale wait state the
/// moment the agent acts on a new direction.
///
/// No activity entry is written here: `PostToolUse` logs the same call when
/// it resolves, and a denied call has nothing to log.
///
/// Only Claude registers `PreToolUse` today. If another agent ever maps a
/// pre-tool trigger onto `ToolStart`, revisit the Hermes background-fork
/// caveat from `handle_activity_log` first — a fork's tool events must not
/// promote an idle pane to `running`.
pub(in crate::cli::hook) fn on_tool_start(pane: &str, ctx: &AgentContext<'_>) -> i32 {
    if ctx.agent == crate::tmux::HERMES_AGENT {
        return 0;
    }
    // One batched read for status, the run clock, and the session binding
    // the post-turn guard compares against (audit 2026-11-19 C2 pattern).
    let Some(reads) = tmux::get_pane_options(
        pane,
        &[
            tmux::PANE_STATUS,
            tmux::PANE_STARTED_AT,
            tmux::PANE_SESSION_ID,
        ],
    ) else {
        // A failed read must not read as an unclaimed pane — an empty
        // status is promotable, so one timed-out spawn used to flip the
        // pane to `running` and restamp its run clock (audit 2026-11-26
        // L-hook-3). The next event re-decides.
        return 0;
    };
    let current_status = reads.first().map(String::as_str).unwrap_or_default();
    // Mid-turn tool calls arrive with the pane already `running` — the
    // common case must stay a read-only probe, not a tmux write storm.
    // The read is batched with the run-clock probe below (audit
    // 2026-11-19 C2).
    if current_status == "running" {
        return 0;
    }
    // A Claude `PreToolUse` landing on a post-turn state is a dangling
    // call from the CLI's own background machinery, not a resume: the
    // away-summary/recap generator fires one seconds after `Stop` for a
    // call that never executes (no `PostToolUse`, no transcript row).
    // Promoting it glued a finished session solid green because no `Stop`
    // ever follows a background call (observed live 2026-11-04: a
    // PreToolUse 1s after Stop on a done branch-review pane). A real turn
    // always re-opens with `UserPromptSubmit`, which owns the
    // idle/background promotion. `waiting`/`error` stay promotable — they
    // are mid-turn states (stranded permission denial, auto-retry), and
    // an empty status is an unclaimed pane.
    //
    // Exception: a tool call from a DIFFERENT session than the pane's
    // binding is a session adoption in progress (see
    // `session_takeover`) — a continued-in handoff re-keys the pane's
    // conversation to a fresh session id and keeps tooling with no
    // `UserPromptSubmit` ever firing. Those calls are the new session's
    // real turn and must promote, or the freshly adopted session renders
    // idle while it works.
    let bound = reads.get(2).map(String::as_str).unwrap_or_default();
    if ctx.agent == crate::tmux::CLAUDE_AGENT
        && (current_status == "idle" || current_status == "background")
        && !super::status_priority::session_takeover(ctx.session_id.as_deref(), bound)
    {
        return 0;
    }
    set_agent_meta(pane, ctx);
    set_status(pane, "running");
    // Same cleanup as the `handle_activity_log` resume: the reason row
    // renders for every status, so a stale `permission_denied` (or error
    // text stored by `on_stop_failure`) must go when the agent resumes.
    // set_status's batch already unset @pane_attention; the wait-reason
    // clear is one batched write, not two spawns (audit 2026-11-19 C2).
    tmux::apply_pane_options(
        pane,
        &[tmux::PaneOptionWrite::Unset {
            key: tmux::PANE_WAIT_REASON,
        }],
    );
    let existing_started = reads.get(1).map(String::as_str).unwrap_or_default();
    if existing_started.is_empty() {
        tmux::set_pane_option(pane, tmux::PANE_STARTED_AT, &now_epoch_secs().to_string());
    }
    0
}

/// Claude `PostToolUseFailure`: a tool call failed mid-turn.
///
/// Feed-only. Failed calls are invisible to `PostToolUse` (success-only),
/// so without this the feed shows a tool run that silently went wrong.
/// The turn continues after a failed tool — the agent sees the error and
/// adapts — so no status, attention, or wait-reason change: the next
/// `PreToolUse` / `PostToolUse` / `Stop` owns those transitions. No
/// `set_agent_meta` either — identity was already freshened by this
/// turn's opener, and a per-tool rewrite is churn.
///
/// Only Claude registers `PostToolUseFailure` today; the same
/// background-fork caveat as `handle_activity_log` applies if another
/// agent ever maps a trigger here.
pub(in crate::cli::hook) fn on_tool_failure(pane: &str, tool_name: &str, error: &str) -> i32 {
    // First line only: Bash errors are `Exit code N` plus interleaved
    // output, and the feed row label stays scannable.
    let first_line = error.lines().next().unwrap_or("").trim();
    let label = if first_line.is_empty() {
        tool_name
    } else {
        first_line
    };
    write_activity_entry(pane, tool_name, label);
    0
}

pub(in crate::cli::hook) fn on_stop(
    pane: &str,
    ctx: &AgentContext<'_>,
    last_message: &str,
    response: Option<&str>,
    notifications: Option<desktop_notification::DesktopNotificationSettings>,
) -> i32 {
    set_agent_meta(pane, ctx);
    if !last_message.is_empty() {
        let msg = sanitize_tmux_value(last_message);
        tmux::apply_pane_options(
            pane,
            &[
                tmux::PaneOptionWrite::Set {
                    key: tmux::PANE_PROMPT,
                    value: &msg,
                },
                tmux::PaneOptionWrite::Set {
                    key: tmux::PANE_PROMPT_SOURCE,
                    value: "response",
                },
            ],
        );
    }
    let bg_shell_live = !tmux::get_pane_option_value(pane, tmux::PANE_BG_CMD).is_empty();
    // Claude Code `Task` subagents are synchronous: once the parent
    // reaches Stop, no child should still be running, so any leftover
    // list is stale state from a missed or mismatched SubagentStop and
    // is cleared before `mark_task_reset`, whose guard intentionally
    // skips writes while subagents are active.
    //
    // Only the liveness list is cleared here. The routing copy
    // (`@pane_subagent_routes`) deliberately survives: async subagent
    // hosts (`--agent claude --inherit-permission-mode` under bg-pty-host)
    // keep firing env-scrubbed hook events after the parent's Stop, and
    // without their route entries those events drop on the floor (see
    // `cli::hook::resolve`).
    //
    // Hermes `delegate_task` has an async/background mode: the parent's
    // turn can end while dispatched children still run, and each child
    // removes itself via `subagent_stop` when it finishes. For Hermes a
    // non-empty list is therefore live work, not stale state — keep it
    // and let the final `subagent_stop` drain it (dropping the pane back
    // to idle), so the pane doesn't flash idle while children run.
    //
    // The read+unset holds the same per-pane flock as the subagent start/
    // stop paths: a concurrent SubagentStop does a read-modify-write on
    // this list, and an unlocked unset landing between its read and its
    // rewrite resurrects the removed children — stranding
    // `pane_writes_allowed` and tripping the SessionEnd subagent guard
    // until the next parent Stop re-clears.
    let children_live = with_subagent_lock(pane, || {
        let subagents = tmux::get_pane_option_value(pane, tmux::PANE_SUBAGENTS);
        let children_live = ctx.agent == crate::tmux::HERMES_AGENT && !subagents.is_empty();
        if !children_live {
            tmux::unset_pane_option(pane, tmux::PANE_SUBAGENTS);
        }
        children_live
    });
    let bg_work_live = bg_shell_live || children_live;
    if bg_work_live {
        tmux::unset_pane_option(pane, tmux::PANE_WAIT_REASON);
    } else {
        clear_run_state(pane);
    }
    mark_task_reset(pane);
    set_status(pane, resolve_stop_status(bg_work_live));
    // Flag the finished turn for attention AFTER set_status, which
    // clears attention for `idle`. The sidebar clears the flag when the
    // user focuses the pane; a new prompt / activity clears it on the
    // hook side.
    set_attention(pane, "done");

    if !bg_work_live {
        let run_id = notification_run_id(pane);
        // Skip the generic Stop notification if an explicit TaskCompleted
        // stamp from the current run has already fired — otherwise Claude
        // Code's `TaskCompleted` → `Stop` sequence produces two desktop
        // notifications for the same logical completion.
        let already_notified = desktop_notification::has_run_scoped_stamp(
            pane,
            DesktopNotificationKind::TaskCompleted,
            run_id,
        );
        if !already_notified {
            let _ = notify_lifecycle(
                pane,
                NotifyLabels::FromCtx(ctx),
                notifications,
                run_id,
                NotifyPayload {
                    kind: DesktopNotificationKind::TaskCompleted,
                    event: desktop_notification::DesktopNotificationEvent::Stop,
                    fingerprint_suffix: "stop",
                    body: &stop_body(last_message),
                },
            );
        }
    }
    if let Some(resp) = response {
        println!("{resp}");
    }
    0
}

pub(in crate::cli::hook) fn on_stop_failure(
    pane: &str,
    ctx: &AgentContext<'_>,
    error: &str,
    notifications: Option<desktop_notification::DesktopNotificationSettings>,
) -> i32 {
    set_agent_meta(pane, ctx);
    set_attention(pane, "clear");
    clear_run_state(pane);
    mark_task_reset(pane);
    if !error.is_empty() {
        tmux::set_pane_option(pane, tmux::PANE_WAIT_REASON, error);
        // Feed visibility: a failed turn is an attention event. `Error` is
        // a sidebar pseudo-tool — see `FeedKind::classify`.
        write_activity_entry(pane, "Error", error);
    }
    set_status(pane, "error");
    let _ = notify_lifecycle(
        pane,
        NotifyLabels::FromCtx(ctx),
        notifications,
        None,
        NotifyPayload {
            kind: DesktopNotificationKind::TaskFailed,
            event: desktop_notification::DesktopNotificationEvent::StopFailure,
            fingerprint_suffix: stop_failure_fingerprint(error),
            body: &stop_failure_body(error),
        },
    );
    0
}

pub(in crate::cli::hook) fn on_task_completed(
    pane: &str,
    agent_name: &str,
    task_id: &str,
    task_subject: &str,
    notifications: Option<desktop_notification::DesktopNotificationSettings>,
) -> i32 {
    let _ = notify_lifecycle(
        pane,
        NotifyLabels::FromPane { agent: agent_name },
        notifications,
        None,
        NotifyPayload {
            kind: DesktopNotificationKind::TaskCompleted,
            event: desktop_notification::DesktopNotificationEvent::TaskCompleted,
            fingerprint_suffix: task_completed_fingerprint(task_id, task_subject),
            body: &task_completed_body(task_subject),
        },
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn on_user_prompt_submit_sets_running_and_stores_prompt() {
        let _guard = tmux::test_mock::install();
        let pane = "%PROMPT";
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_user_prompt_submit(pane, &ctx, "fix the bug");
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("running")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_PROMPT).as_deref(),
            Some("fix the bug")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_PROMPT_SOURCE).as_deref(),
            Some("user")
        );
        assert!(tmux::test_mock::contains(pane, tmux::PANE_STARTED_AT));
    }

    #[test]
    fn on_user_prompt_submit_failed_read_skips_state_writes_keeps_identity() {
        // L-hook-3: with the state read down, the empty-read fallback
        // used to promote the pane, restamp its clock, and clear a
        // pending wait. All three decide from prior pane state and must
        // be skipped; the notification stamp and prompt preview are
        // unconditional overwrites and stay.
        let _guard = tmux::test_mock::install();
        let pane = "%PROMPT_READ_FAIL";
        tmux::test_mock::set(pane, tmux::PANE_WAIT_REASON, "permission");
        let _fail = tmux::test_mock::fail_reads(pane);
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_user_prompt_submit(pane, &ctx, "fix the bug");
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_PROMPT).as_deref(),
            Some("fix the bug"),
            "the prompt preview is read-independent and must land"
        );
        assert!(
            tmux::test_mock::contains(pane, tmux::PANE_NOTIFICATION_RUN_ID),
            "the notification stamp is read-independent and must land"
        );
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_STATUS),
            "a failed read must not promote the pane"
        );
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_STARTED_AT),
            "a failed read must not restamp the run clock"
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_WAIT_REASON).as_deref(),
            Some("permission"),
            "a failed read must not clear a pending wait reason"
        );
    }

    #[test]
    fn on_user_prompt_submit_ignores_system_messages() {
        let _guard = tmux::test_mock::install();
        let pane = "%SYS_PROMPT";
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        on_user_prompt_submit(pane, &ctx, "<system-reminder>ignore me</system-reminder>");
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_PROMPT),
            "system messages should not be stored as user prompt"
        );
        // But status should still advance to running.
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("running")
        );
    }

    #[test]
    fn on_user_prompt_submit_keeps_antigravity_pending_wait() {
        // agy's PreInvocation fires before every model call in a turn; a
        // call racing an approval-prompt scan flag must not clear the
        // wait — the scan worker clears it when the prompt disappears.
        let _guard = tmux::test_mock::install();
        let pane = "%AGY_PROMPT_STICKY";
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "waiting");
        tmux::test_mock::set(pane, tmux::PANE_WAIT_REASON, "permission");
        let ctx = AgentContext {
            agent: "antigravity",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_user_prompt_submit(pane, &ctx, "continue");
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("waiting"),
            "a model call must not clear a pending agy permission prompt"
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_WAIT_REASON).as_deref(),
            Some("permission")
        );
    }

    #[test]
    fn on_user_prompt_submit_agy_running_turn_does_not_restamp_the_clock() {
        // M-hook-2 (audit 2026-11-25): agy's PreInvocation fires before
        // every model call with a transcript-recovered (never-empty)
        // prompt. Restamping per call would render elapsed-since-last-
        // model-call instead of turn length, so a running turn keeps its
        // clock; a missing clock (recovery) still stamps.
        let _guard = tmux::test_mock::install();
        let pane = "%AGY_RUNNING_CLOCK";
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "running");
        tmux::test_mock::set(pane, tmux::PANE_STARTED_AT, "1700000000");
        let ctx = AgentContext {
            agent: "antigravity",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_user_prompt_submit(pane, &ctx, "recovered prompt");
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STARTED_AT).as_deref(),
            Some("1700000000"),
            "a mid-turn agy model call must not restart the run clock"
        );

        // Recovery: the clock is missing (sidebar restart mid-turn) —
        // the running PreInvocation starts one.
        let pane = "%AGY_RUNNING_NO_CLOCK";
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "running");
        let exit = on_user_prompt_submit(pane, &ctx, "recovered prompt");
        assert_eq!(exit, 0);
        assert!(
            tmux::test_mock::contains(pane, tmux::PANE_STARTED_AT),
            "a missing clock must still be stamped so the turn times"
        );
    }

    #[test]
    fn on_user_prompt_submit_agy_boot_call_does_not_promote_idle() {
        // agy fires PreInvocation ~24ms after SessionStart for its boot
        // title-generation model call — empty recovered prompt, no turn
        // in flight. The pane must stay idle at agy's input box.
        let _guard = tmux::test_mock::install();
        let pane = "%AGY_BOOT_CALL";
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "idle");
        let ctx = AgentContext {
            agent: "antigravity",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_user_prompt_submit(pane, &ctx, "");
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("idle"),
            "agy's boot model call must not show the pane as working"
        );
    }

    #[test]
    fn on_user_prompt_submit_clears_stale_wait_reason_but_preserves_bg_cmd() {
        let _guard = tmux::test_mock::install();
        let pane = "%PROMPT_CLEAR_WAIT";
        tmux::test_mock::set(pane, tmux::PANE_WAIT_REASON, "permission");
        tmux::test_mock::set(pane, tmux::PANE_BG_CMD, "npm run dev");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        on_user_prompt_submit(pane, &ctx, "new prompt");
        assert!(!tmux::test_mock::contains(pane, tmux::PANE_WAIT_REASON));
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_BG_CMD).as_deref(),
            Some("npm run dev"),
            "bg command must survive a new user turn — the shell is still running",
        );
    }

    #[test]
    fn on_user_prompt_submit_empty_prompt_preserves_running_clock() {
        // OpenCode fires a synthetic empty-prompt submission on every
        // assistant response (session.status busy). The run clock must
        // span the whole turn, not restart per response.
        let _guard = tmux::test_mock::install();
        let pane = "%PROMPT_KEEP_CLOCK";
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "running");
        tmux::test_mock::set(pane, tmux::PANE_STARTED_AT, "1700000000");
        let ctx = AgentContext {
            agent: "opencode",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_user_prompt_submit(pane, &ctx, "");
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STARTED_AT).as_deref(),
            Some("1700000000"),
            "empty-prompt submission must not restart an existing run clock"
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("running")
        );
    }

    #[test]
    fn on_user_prompt_submit_empty_prompt_stamps_missing_clock() {
        // Recovery path: retry → busy with no chat.message seen, so no
        // clock exists yet — stamp one.
        let _guard = tmux::test_mock::install();
        let pane = "%PROMPT_RECOVER_CLOCK";
        let ctx = AgentContext {
            agent: "opencode",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_user_prompt_submit(pane, &ctx, "");
        assert_eq!(exit, 0);
        assert!(tmux::test_mock::contains(pane, tmux::PANE_STARTED_AT));
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("running")
        );
    }

    #[test]
    fn on_user_prompt_submit_real_prompt_restarts_running_clock() {
        // A genuine user message (chat.message / Claude UserPromptSubmit)
        // restarts the clock even while one is already running — a queued
        // prompt starts a new turn.
        let _guard = tmux::test_mock::install();
        let pane = "%PROMPT_RESTART_CLOCK";
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "running");
        tmux::test_mock::set(pane, tmux::PANE_STARTED_AT, "1700000000");
        let ctx = AgentContext {
            agent: "opencode",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_user_prompt_submit(pane, &ctx, "follow-up prompt");
        assert_eq!(exit, 0);
        let stamped = tmux::test_mock::get(pane, tmux::PANE_STARTED_AT);
        assert_ne!(
            stamped.as_deref(),
            Some("1700000000"),
            "real prompt must restart the run clock"
        );
        assert!(stamped.is_some());
    }

    #[test]
    fn on_tool_start_clears_stale_permission_denied_wait_state() {
        // Regression: an auto-mode denial (PermissionDenied) leaves the pane
        // `waiting`/`permission_denied` while the agent continues the same
        // turn. The next signal used to be the next *successful*
        // PostToolUse, so a long-running retry kept the stale warning up
        // for its whole duration. PreToolUse must clear it the moment the
        // agent attempts anything new.
        let _guard = tmux::test_mock::install();
        let pane = "%TOOL_START_CLEAR";
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "waiting");
        tmux::test_mock::set(pane, tmux::PANE_WAIT_REASON, "permission_denied");
        tmux::test_mock::set(pane, tmux::PANE_ATTENTION, "notification");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "auto",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_tool_start(pane, &ctx);
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("running")
        );
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_WAIT_REASON),
            "the stale denial reason must clear on the next tool attempt"
        );
        assert!(!tmux::test_mock::contains(pane, tmux::PANE_ATTENTION));
    }

    #[test]
    fn on_tool_start_failed_read_does_not_promote() {
        // L-hook-3: a transient tmux read failure must not read as an
        // unclaimed pane — an empty status is promotable, so one
        // timed-out spawn used to flip the pane to `running` and restamp
        // its clock. The next event re-decides.
        let _guard = tmux::test_mock::install();
        let pane = "%TOOL_START_READ_FAIL";
        let _fail = tmux::test_mock::fail_reads(pane);
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "auto",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_tool_start(pane, &ctx);
        assert_eq!(exit, 0);
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_STATUS),
            "a failed read must not promote the pane"
        );
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_STARTED_AT),
            "a failed read must not restamp the run clock"
        );
    }

    #[test]
    fn on_tool_start_is_noop_while_already_running() {
        // Mid-turn tool calls arrive with the pane already running; they
        // must not restart the run clock or touch state.
        let _guard = tmux::test_mock::install();
        let pane = "%TOOL_START_RUNNING";
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "running");
        tmux::test_mock::set(pane, tmux::PANE_STARTED_AT, "1700000000");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "auto",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_tool_start(pane, &ctx);
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("running")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STARTED_AT).as_deref(),
            Some("1700000000"),
            "a running pane must keep its run clock"
        );
    }

    #[test]
    fn on_tool_start_does_not_promote_post_turn_idle_pane() {
        // Regression: Claude's post-turn background machinery (the
        // away-summary/recap generator) fires a PreToolUse seconds after
        // the turn's Stop, for a call that never executes. Promoting the
        // idle pane glued a finished branch-review session solid green
        // (2026-11-04) because no Stop ever follows a background call. A
        // real turn re-opens with UserPromptSubmit, which owns promotion.
        let _guard = tmux::test_mock::install();
        let pane = "%TOOL_START_DANGLING_IDLE";
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "idle");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "auto",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_tool_start(pane, &ctx);
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("idle"),
            "a dangling post-turn PreToolUse must not show the pane as working"
        );
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_STARTED_AT),
            "no run clock without a real turn opener"
        );
    }

    #[test]
    fn on_tool_start_does_not_promote_post_turn_background_pane() {
        // Same dangling-call stance when a background shell outlived the
        // turn: the pane stays `background` until the next real turn opens.
        let _guard = tmux::test_mock::install();
        let pane = "%TOOL_START_DANGLING_BG";
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "background");
        tmux::test_mock::set(pane, tmux::PANE_BG_CMD, "npm run dev");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "auto",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_tool_start(pane, &ctx);
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("background"),
            "a dangling post-turn PreToolUse must not override background"
        );
    }

    #[test]
    fn on_tool_start_ignores_hermes_background_forks() {
        // Hermes' background forks emit tool events while the parent turn
        // is idle; a pre-tool trigger must not promote the pane to
        // `running` with no stop ever coming (same stance as
        // `handle_activity_log`). Defensive: only Claude registers
        // PreToolUse today.
        let _guard = tmux::test_mock::install();
        let pane = "%TOOL_START_HERMES";
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "waiting");
        tmux::test_mock::set(pane, tmux::PANE_WAIT_REASON, "permission_denied");
        let ctx = AgentContext {
            agent: "hermes",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_tool_start(pane, &ctx);
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("waiting"),
            "hermes tool events must not flip the pane to running"
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_WAIT_REASON).as_deref(),
            Some("permission_denied")
        );
    }

    #[test]
    fn on_stop_with_background_shell_sets_background_status() {
        let _guard = tmux::test_mock::install();
        let pane = "%STOP_BG";
        tmux::test_mock::set(pane, tmux::PANE_BG_CMD, "npm run dev");
        tmux::test_mock::set(pane, tmux::PANE_STARTED_AT, "123");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };

        let exit = on_stop(
            pane,
            &ctx,
            "",
            None,
            Some(desktop_notification::DesktopNotificationSettings {
                enabled: false,
                events: Default::default(),
            }),
        );

        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("background")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STARTED_AT).as_deref(),
            Some("123")
        );
    }

    #[test]
    fn on_stop_without_background_shell_sets_idle_status() {
        let _guard = tmux::test_mock::install();
        let pane = "%STOP_IDLE";
        tmux::test_mock::set(pane, tmux::PANE_STARTED_AT, "123");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };

        on_stop(
            pane,
            &ctx,
            "",
            None,
            Some(desktop_notification::DesktopNotificationSettings {
                enabled: false,
                events: Default::default(),
            }),
        );

        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("idle")
        );
        assert!(!tmux::test_mock::contains(pane, tmux::PANE_STARTED_AT));
    }

    #[test]
    fn on_stop_flags_done_attention_for_unseen_turn() {
        let _guard = tmux::test_mock::install();
        let pane = "%STOP_DONE";
        tmux::test_mock::set(pane, tmux::PANE_ATTENTION, "notification");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };

        on_stop(
            pane,
            &ctx,
            "all finished",
            None,
            Some(desktop_notification::DesktopNotificationSettings {
                enabled: false,
                events: Default::default(),
            }),
        );

        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_ATTENTION).as_deref(),
            Some("done"),
            "Stop must leave the pane flagged done-unseen; the sidebar clears it on focus",
        );
    }

    #[test]
    fn on_stop_clears_stale_subagents() {
        let _guard = tmux::test_mock::install();
        let pane = "%STOP_STALE_SUBAGENTS";
        tmux::test_mock::set(
            pane,
            tmux::PANE_SUBAGENTS,
            "general-purpose:sub-1,general-purpose:sub-2",
        );
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };

        on_stop(
            pane,
            &ctx,
            "",
            None,
            Some(desktop_notification::DesktopNotificationSettings {
                enabled: false,
                events: Default::default(),
            }),
        );

        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_SUBAGENTS),
            "parent Stop must clear stale subagent list"
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("idle")
        );
    }

    #[test]
    fn on_stop_preserves_subagent_routes() {
        // Regression (dropped-event trigger 2): the parent's Stop used to
        // be the only place the routing list could live, so clearing it
        // stranded every async subagent host still running under
        // bg-pty-host — its env-scrubbed events matched no pane and were
        // dropped. The liveness guard clears; the routing copy survives.
        let _guard = tmux::test_mock::install();
        let pane = "%STOP_KEEP_ROUTES";
        tmux::test_mock::set(pane, tmux::PANE_SUBAGENTS, ",Explore:sub-1");
        tmux::test_mock::set(pane, tmux::PANE_SUBAGENT_ROUTES, ",Explore:sub-1");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };

        on_stop(
            pane,
            &ctx,
            "",
            None,
            Some(desktop_notification::DesktopNotificationSettings {
                enabled: false,
                events: Default::default(),
            }),
        );

        assert!(!tmux::test_mock::contains(pane, tmux::PANE_SUBAGENTS));
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_SUBAGENT_ROUTES).as_deref(),
            Some(",Explore:sub-1"),
            "routes must survive the parent's Stop so live hosts keep routing"
        );
    }

    #[test]
    fn on_tool_failure_writes_feed_entry_without_status_change() {
        // A failed tool call must become visible in the feed (PostToolUse
        // is success-only) but never flip status or attention: the turn
        // continues after a failed tool.
        let _guard = tmux::test_mock::install();
        let pane = "%TOOL_FAIL";
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "running");
        let log_path = crate::activity::log_file_path(pane);
        let _ = fs::remove_file(&log_path);

        let exit = on_tool_failure(pane, "Bash", "Exit code 1\nError: tests failed");
        assert_eq!(exit, 0);

        let content = fs::read_to_string(&log_path).unwrap();
        assert!(
            content.contains("|Bash|Exit code 1"),
            "first error line only, expected it in {content:?}"
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("running"),
            "a failed tool must not change status"
        );
        assert!(!tmux::test_mock::contains(pane, tmux::PANE_ATTENTION));
        fs::remove_file(&log_path).ok();
    }

    #[test]
    fn on_tool_failure_falls_back_to_tool_name_when_error_empty() {
        let _guard = tmux::test_mock::install();
        let pane = "%TOOL_FAIL_EMPTY";
        let log_path = crate::activity::log_file_path(pane);
        let _ = fs::remove_file(&log_path);

        on_tool_failure(pane, "WebSearch", "");
        let content = fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("|WebSearch|WebSearch"), "got {content:?}");
        fs::remove_file(&log_path).ok();
    }

    #[test]
    fn on_stop_keeps_async_subagents_for_hermes_and_resolves_background() {
        // Hermes `delegate_task` background mode: children outlive the
        // parent's turn and remove themselves via SubagentStop. The list
        // is live work, not stale state — it must survive the Stop and
        // the pane must resolve to `background`, not idle.
        let _guard = tmux::test_mock::install();
        let pane = "%STOP_HERMES_CHILDREN";
        tmux::test_mock::set(pane, tmux::PANE_SUBAGENTS, "leaf:child-1");
        tmux::test_mock::set(pane, tmux::PANE_STARTED_AT, "123");
        let ctx = AgentContext {
            agent: "hermes",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };

        on_stop(
            pane,
            &ctx,
            "",
            None,
            Some(desktop_notification::DesktopNotificationSettings {
                enabled: false,
                events: Default::default(),
            }),
        );

        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_SUBAGENTS).as_deref(),
            Some("leaf:child-1"),
            "async children must survive the parent Stop so the final SubagentStop can drain them"
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("background")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STARTED_AT).as_deref(),
            Some("123"),
            "live children keep the run clock, mirroring the bg-shell path"
        );
    }

    #[test]
    fn on_stop_failure_records_error_wait_reason_and_error_status() {
        let _guard = tmux::test_mock::install();
        let pane = "%STOP_FAIL";
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_stop_failure(
            pane,
            &ctx,
            "rate_limit",
            Some(desktop_notification::DesktopNotificationSettings {
                enabled: false,
                events: Default::default(),
            }),
        );
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("error")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_WAIT_REASON).as_deref(),
            Some("rate_limit")
        );
    }
}
