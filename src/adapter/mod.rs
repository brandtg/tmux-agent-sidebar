pub mod claude;
pub mod codex;
pub mod opencode;

use crate::event::{AgentEvent, AgentEventKind, WorktreeInfo};

pub(crate) fn json_str<'a>(val: &'a serde_json::Value, key: &str) -> &'a str {
    val.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

pub(crate) fn optional_str(val: &serde_json::Value, key: &str) -> Option<String> {
    let s = json_str(val, key);
    if s.is_empty() { None } else { Some(s.into()) }
}

pub(crate) fn json_value_or_null(val: &serde_json::Value, key: &str) -> serde_json::Value {
    val.get(key).cloned().unwrap_or(serde_json::Value::Null)
}

/// The identity fields every session-scoped [`AgentEvent`] variant carries:
/// agent label, `cwd`, `permission_mode`, worktree metadata, and the
/// agent/session ids. Each adapter builds one `EventBase` per payload via its
/// own `base()` helper — making the per-agent divergences (which fields an
/// agent actually provides) explicit in exactly one place — then hands it to
/// the per-variant constructors below so the variant field wiring itself is
/// written once instead of copy-pasted per parse arm.
pub(crate) struct EventBase {
    agent: &'static str,
    cwd: String,
    permission_mode: String,
    worktree: Option<WorktreeInfo>,
    agent_id: Option<String>,
    session_id: Option<String>,
}

impl EventBase {
    /// Identity fields every payload carries: the agent label, `cwd`, and
    /// `session_id`. `permission_mode` starts empty; adapters whose upstream
    /// payloads carry one chain [`EventBase::with_permission_mode`].
    pub(crate) fn new(input: &serde_json::Value, agent: &'static str) -> Self {
        Self {
            agent,
            cwd: json_str(input, "cwd").into(),
            permission_mode: String::new(),
            worktree: None,
            agent_id: None,
            session_id: optional_str(input, "session_id"),
        }
    }

    /// Read `permission_mode` from the payload (Claude, Codex).
    pub(crate) fn with_permission_mode(mut self, input: &serde_json::Value) -> Self {
        self.permission_mode = json_str(input, "permission_mode").into();
        self
    }

    /// Attach worktree metadata parsed by the adapter (only Claude payloads
    /// carry a `worktree` object today).
    pub(crate) fn with_worktree(mut self, worktree: Option<WorktreeInfo>) -> Self {
        self.worktree = worktree;
        self
    }

    /// Attach the subagent/teammate id (only Claude payloads carry one).
    pub(crate) fn with_agent_id(mut self, agent_id: Option<String>) -> Self {
        self.agent_id = agent_id;
        self
    }

    pub(crate) fn session_start(self, source: String) -> AgentEvent {
        AgentEvent::SessionStart {
            agent: self.agent.into(),
            cwd: self.cwd,
            permission_mode: self.permission_mode,
            source,
            worktree: self.worktree,
            agent_id: self.agent_id,
            session_id: self.session_id,
        }
    }

    pub(crate) fn user_prompt_submit(self, prompt: String) -> AgentEvent {
        AgentEvent::UserPromptSubmit {
            agent: self.agent.into(),
            cwd: self.cwd,
            permission_mode: self.permission_mode,
            prompt,
            worktree: self.worktree,
            agent_id: self.agent_id,
            session_id: self.session_id,
        }
    }

    pub(crate) fn notification(self, wait_reason: String, meta_only: bool) -> AgentEvent {
        AgentEvent::Notification {
            agent: self.agent.into(),
            cwd: self.cwd,
            permission_mode: self.permission_mode,
            wait_reason,
            meta_only,
            worktree: self.worktree,
            agent_id: self.agent_id,
            session_id: self.session_id,
        }
    }

    pub(crate) fn stop(self, last_message: String, response: Option<String>) -> AgentEvent {
        AgentEvent::Stop {
            agent: self.agent.into(),
            cwd: self.cwd,
            permission_mode: self.permission_mode,
            last_message,
            response,
            worktree: self.worktree,
            agent_id: self.agent_id,
            session_id: self.session_id,
        }
    }

    pub(crate) fn stop_failure(self, error: String) -> AgentEvent {
        AgentEvent::StopFailure {
            agent: self.agent.into(),
            cwd: self.cwd,
            permission_mode: self.permission_mode,
            error,
            worktree: self.worktree,
            agent_id: self.agent_id,
            session_id: self.session_id,
        }
    }

    pub(crate) fn permission_denied(self) -> AgentEvent {
        AgentEvent::PermissionDenied {
            agent: self.agent.into(),
            cwd: self.cwd,
            permission_mode: self.permission_mode,
            worktree: self.worktree,
            agent_id: self.agent_id,
            session_id: self.session_id,
        }
    }

    pub(crate) fn cwd_changed(self) -> AgentEvent {
        AgentEvent::CwdChanged {
            cwd: self.cwd,
            worktree: self.worktree,
            agent_id: self.agent_id,
            session_id: self.session_id,
        }
    }

    pub(crate) fn session_title(self, title: String) -> AgentEvent {
        AgentEvent::SessionTitle {
            agent: self.agent.into(),
            cwd: self.cwd,
            session_id: self.session_id,
            title,
        }
    }
}

/// Binding between an upstream agent-side hook trigger (as it appears in the
/// agent's `settings.json`) and the internal `AgentEventKind` the sidebar
/// produces once the hook fires.
///
/// Each adapter exposes its full `HOOK_REGISTRATIONS` table so install
/// wizards, README snippets, setup commands, and docs can all be generated
/// from a single source of truth. The `kind` field is a compile-time enum,
/// not a string, so typos cannot creep in. Drift between the table and the
/// adapter's `parse()` match arms is caught by the tests in `claude.rs` /
/// `codex.rs` via [`assert_table_drift_free`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookRegistration {
    /// Trigger name in the agent's settings.json (e.g. `"SessionStart"`,
    /// `"PostToolUse"`).
    pub trigger: &'static str,
    /// Optional matcher value. `None` means "register with an empty matcher"
    /// (catches all). `Some("startup|resume")` etc. captures a specific filter.
    pub matcher: Option<&'static str>,
    /// Internal event this registration produces.
    pub kind: AgentEventKind,
}

#[cfg(test)]
pub(crate) fn minimal_payload(kind: AgentEventKind) -> serde_json::Value {
    use serde_json::json;
    match kind {
        AgentEventKind::ActivityLog => json!({"tool_name": "Read"}),
        AgentEventKind::SubagentStart | AgentEventKind::SubagentStop => {
            json!({"agent_type": "Explore"})
        }
        _ => json!({}),
    }
}

#[cfg(test)]
pub(crate) fn assert_table_drift_free(agent: &str, table: &[HookRegistration]) {
    use crate::event::resolve_adapter;
    let adapter = resolve_adapter(agent).expect("adapter should exist");

    // Table → parse: every registration must be accepted by `parse()` and
    // produce an `AgentEvent` whose kind matches the registration.
    for reg in table {
        let event_name = reg.kind.external_name();
        let payload = minimal_payload(reg.kind);
        let produced = adapter.parse(event_name, &payload).unwrap_or_else(|| {
            panic!(
                "{agent}: HOOK_REGISTRATIONS lists {:?} but parse() returned None — parse arm missing",
                reg.kind
            )
        });
        assert_eq!(
            produced.kind(),
            reg.kind,
            "{agent}: table declares {:?} but parse() produced {:?}",
            reg.kind,
            produced.kind()
        );
    }

    // Parse → table: every kind `parse()` accepts must appear in the table.
    // Catches "added parse arm, forgot to update HOOK_REGISTRATIONS".
    for kind in AgentEventKind::ALL {
        let accepted = adapter
            .parse(kind.external_name(), &minimal_payload(*kind))
            .is_some();
        let in_table = table.iter().any(|r| r.kind == *kind);
        assert!(
            !accepted || in_table,
            "{agent}: parse() accepts {:?} but HOOK_REGISTRATIONS does not list it — add it to the table",
            kind
        );
    }
}
