use crate::debug;
use crate::event::{AgentEvent, resolve_adapter};

use super::{read_stdin_json, tmux_pane};

mod activity;
mod context;
mod handlers;
mod notifications;

use context::sync_pane_location;
use notifications::notification_settings;

// ─── hook subcommand ────────────────────────────────────────────────────────

pub(crate) fn cmd_hook(args: &[String]) -> i32 {
    let agent_name = args.first().map(|s| s.as_str()).unwrap_or("");
    let event_name = args.get(1).map(|s| s.as_str()).unwrap_or("");

    if agent_name.is_empty() || event_name.is_empty() {
        debug::log(&format!(
            "hook: ignored: missing agent or event (args {args:?})"
        ));
        return 0;
    }

    let Some(adapter) = resolve_adapter(agent_name) else {
        debug::log(&format!(
            "hook: ignored: unknown agent '{agent_name}' (event '{event_name}')"
        ));
        return 0;
    };

    let pane = tmux_pane();
    if pane.is_empty() {
        debug::log(&format!(
            "hook: ignored: TMUX_PANE not set (agent '{agent_name}' event '{event_name}')"
        ));
        return 0;
    }

    let Some((input, input_bytes, input_valid)) = read_stdin_json() else {
        debug::log(&format!(
            "hook: {agent_name} {event_name} pane {pane}: stdin could not be read; dropping event"
        ));
        return 0;
    };
    if !input_valid {
        // Malformed or truncated payload: adapters must never see it —
        // an all-empty event parsed from garbage stdin would fire
        // spurious teardowns and notifications.
        debug::log(&format!(
            "hook: {agent_name} {event_name} pane {pane}: payload is not valid JSON ({input_bytes} bytes); dropping event"
        ));
        return 0;
    }
    let Some(event) = adapter.parse(event_name, &input) else {
        debug::log(&format!(
            "hook: {agent_name} {event_name} pane {pane}: adapter produced no event for {} bytes: {}",
            input_bytes,
            payload_preview(&input)
        ));
        return 0;
    };

    debug::log(&format!(
        "hook: {agent_name} {event_name} pane {pane} session {:?} -> {:?}",
        event.session_id(),
        event.kind()
    ));
    handle_event(&pane, agent_name, event)
}

/// Single-line, size-capped rendering of a hook payload for the debug
/// trace. Debug logs live in the user's private runtime dir, but the
/// payload can embed prompts and file contents, so only a prefix is kept.
fn payload_preview(input: &serde_json::Value) -> String {
    const MAX_PREVIEW_BYTES: usize = 300;
    let rendered = input.to_string();
    if rendered.len() <= MAX_PREVIEW_BYTES {
        return rendered;
    }
    let mut end = MAX_PREVIEW_BYTES;
    while !rendered.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &rendered[..end])
}

// ─── event handler ──────────────────────────────────────────────────────────

fn handle_event(pane: &str, agent_name: &str, event: AgentEvent) -> i32 {
    match event {
        AgentEvent::SessionStart {
            agent,
            cwd,
            permission_mode,
            source,
            worktree,
            session_id,
            ..
        } => handlers::on_session_start(
            pane,
            &context::make_ctx(&agent, &cwd, &permission_mode, &worktree, &session_id),
            &source,
        ),
        AgentEvent::SessionTitle {
            session_id, title, ..
        } => handlers::on_session_title(pane, &title, session_id.as_deref()),
        AgentEvent::SessionEnd {
            end_reason,
            session_id,
        } => {
            let notifications = notification_settings();
            handlers::on_session_end(
                pane,
                agent_name,
                &end_reason,
                session_id.as_deref(),
                &notifications,
            )
        }
        AgentEvent::UserPromptSubmit {
            agent,
            cwd,
            permission_mode,
            prompt,
            worktree,
            session_id,
            ..
        } => handlers::on_user_prompt_submit(
            pane,
            &context::make_ctx(&agent, &cwd, &permission_mode, &worktree, &session_id),
            &prompt,
        ),
        AgentEvent::Notification {
            agent,
            cwd,
            permission_mode,
            wait_reason,
            meta_only,
            worktree,
            session_id,
            ..
        } => {
            let notifications = notification_settings();
            handlers::on_notification(
                pane,
                &context::make_ctx(&agent, &cwd, &permission_mode, &worktree, &session_id),
                &wait_reason,
                meta_only,
                &notifications,
            )
        }
        AgentEvent::Stop {
            agent,
            cwd,
            permission_mode,
            last_message,
            response,
            worktree,
            session_id,
            ..
        } => {
            let notifications = notification_settings();
            handlers::on_stop(
                pane,
                &context::make_ctx(&agent, &cwd, &permission_mode, &worktree, &session_id),
                &last_message,
                response.as_deref(),
                &notifications,
            )
        }
        AgentEvent::StopFailure {
            agent,
            cwd,
            permission_mode,
            error,
            worktree,
            session_id,
            ..
        } => {
            let notifications = notification_settings();
            handlers::on_stop_failure(
                pane,
                &context::make_ctx(&agent, &cwd, &permission_mode, &worktree, &session_id),
                &error,
                &notifications,
            )
        }
        AgentEvent::SubagentStart {
            agent_type,
            agent_id,
        } => handlers::on_subagent_start(pane, &agent_type, agent_id.as_deref()),
        AgentEvent::SubagentStop { agent_id, .. } => {
            handlers::on_subagent_stop(pane, agent_id.as_deref())
        }
        AgentEvent::ActivityLog {
            tool_name,
            tool_input,
            tool_response,
        } => activity::handle_activity_log(pane, &tool_name, &tool_input, &tool_response),
        AgentEvent::PermissionDenied {
            agent,
            cwd,
            permission_mode,
            worktree,
            session_id,
            ..
        } => {
            let notifications = notification_settings();
            handlers::on_permission_denied(
                pane,
                &context::make_ctx(&agent, &cwd, &permission_mode, &worktree, &session_id),
                &notifications,
            )
        }
        AgentEvent::CwdChanged {
            cwd,
            worktree,
            session_id,
            ..
        } => {
            sync_pane_location(pane, &cwd, &worktree, &session_id);
            0
        }
        AgentEvent::TaskCreated { .. } => 0,
        AgentEvent::TaskCompleted {
            task_id,
            task_subject,
        } => {
            super::set_attention(pane, "notification");
            let notifications = notification_settings();
            handlers::on_task_completed(pane, agent_name, &task_id, &task_subject, &notifications)
        }
        AgentEvent::TeammateIdle {
            teammate_name,
            idle_reason,
            ..
        } => handlers::on_teammate_idle(pane, &teammate_name, &idle_reason),
    }
}
