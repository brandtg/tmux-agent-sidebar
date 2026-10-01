mod adapter;
mod kind;

pub use adapter::{EventAdapter, resolve_adapter};
pub use kind::AgentEventKind;

use serde_json::Value;

/// Worktree metadata from Claude Code hook payloads.
/// Present only when the agent is running in a worktree; `None` otherwise.
#[derive(Debug, Clone, PartialEq)]
pub struct WorktreeInfo {
    pub name: String,
    pub path: String,
    pub branch: String,
    pub original_repo_dir: String,
}

/// Internal event representation. All fields are pre-extracted by the adapter.
/// The core handler never reads raw JSON or checks agent names.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    SessionStart {
        agent: String,
        cwd: String,
        permission_mode: String,
        source: String,
        worktree: Option<WorktreeInfo>,
        agent_id: Option<String>,
        session_id: Option<String>,
    },
    /// Agent-reported session title (opencode `session.created` /
    /// `session.updated`). Written to `@pane_session_title` and rendered
    /// as the row title. Carries only identity fields — it must never
    /// mutate status or attention.
    SessionTitle {
        agent: String,
        cwd: String,
        session_id: Option<String>,
        title: String,
    },
    SessionEnd {
        end_reason: String,
        /// Claude Code reports the ending session's id. `None` when the
        /// agent does not provide one — handlers must not treat absence
        /// as a mismatch.
        session_id: Option<String>,
    },
    UserPromptSubmit {
        agent: String,
        cwd: String,
        permission_mode: String,
        prompt: String,
        worktree: Option<WorktreeInfo>,
        agent_id: Option<String>,
        session_id: Option<String>,
    },
    Notification {
        agent: String,
        cwd: String,
        permission_mode: String,
        wait_reason: String,
        /// When true, only refresh pane metadata without changing status/attention.
        /// Used for events like idle_prompt that carry metadata but should not
        /// trigger a visible status change.
        meta_only: bool,
        worktree: Option<WorktreeInfo>,
        agent_id: Option<String>,
        session_id: Option<String>,
    },
    Stop {
        agent: String,
        cwd: String,
        permission_mode: String,
        last_message: String,
        response: Option<String>,
        worktree: Option<WorktreeInfo>,
        agent_id: Option<String>,
        session_id: Option<String>,
    },
    StopFailure {
        agent: String,
        cwd: String,
        permission_mode: String,
        error: String,
        worktree: Option<WorktreeInfo>,
        agent_id: Option<String>,
        session_id: Option<String>,
    },
    SubagentStart {
        agent_type: String,
        agent_id: Option<String>,
    },
    SubagentStop {
        agent_type: String,
        agent_id: Option<String>,
        last_message: String,
        transcript_path: String,
    },
    ActivityLog {
        tool_name: String,
        tool_input: Value,
        tool_response: Value,
    },
    PermissionDenied {
        agent: String,
        cwd: String,
        permission_mode: String,
        worktree: Option<WorktreeInfo>,
        agent_id: Option<String>,
        session_id: Option<String>,
    },
    CwdChanged {
        cwd: String,
        worktree: Option<WorktreeInfo>,
        agent_id: Option<String>,
        session_id: Option<String>,
    },
    TaskCreated {
        task_id: String,
        task_subject: String,
    },
    TaskCompleted {
        task_id: String,
        task_subject: String,
    },
    TeammateIdle {
        teammate_name: String,
        team_name: String,
        idle_reason: String,
    },
}

impl AgentEvent {
    /// The session the event belongs to, when the agent's payload carries
    /// one. Used by the hook dispatch trace to attribute debug lines.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::SessionStart { session_id, .. }
            | Self::SessionTitle { session_id, .. }
            | Self::UserPromptSubmit { session_id, .. }
            | Self::Notification { session_id, .. }
            | Self::Stop { session_id, .. }
            | Self::StopFailure { session_id, .. }
            | Self::PermissionDenied { session_id, .. }
            | Self::CwdChanged { session_id, .. } => session_id.as_deref(),
            _ => None,
        }
    }

    /// Project an `AgentEvent` down to its `AgentEventKind` discriminant.
    pub fn kind(&self) -> AgentEventKind {
        match self {
            Self::SessionStart { .. } => AgentEventKind::SessionStart,
            Self::SessionTitle { .. } => AgentEventKind::SessionTitle,
            Self::SessionEnd { .. } => AgentEventKind::SessionEnd,
            Self::UserPromptSubmit { .. } => AgentEventKind::UserPromptSubmit,
            Self::Notification { .. } => AgentEventKind::Notification,
            Self::Stop { .. } => AgentEventKind::Stop,
            Self::StopFailure { .. } => AgentEventKind::StopFailure,
            Self::SubagentStart { .. } => AgentEventKind::SubagentStart,
            Self::SubagentStop { .. } => AgentEventKind::SubagentStop,
            Self::ActivityLog { .. } => AgentEventKind::ActivityLog,
            Self::PermissionDenied { .. } => AgentEventKind::PermissionDenied,
            Self::CwdChanged { .. } => AgentEventKind::CwdChanged,
            Self::TaskCreated { .. } => AgentEventKind::TaskCreated,
            Self::TaskCompleted { .. } => AgentEventKind::TaskCompleted,
            Self::TeammateIdle { .. } => AgentEventKind::TeammateIdle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_info_default_is_none() {
        let event = AgentEvent::SessionStart {
            agent: "claude".into(),
            cwd: "/tmp".into(),
            permission_mode: "default".into(),
            source: String::new(),
            worktree: None,
            agent_id: None,
            session_id: None,
        };
        match event {
            AgentEvent::SessionStart {
                worktree, agent_id, ..
            } => {
                assert!(worktree.is_none());
                assert!(agent_id.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn worktree_info_with_values() {
        let wt = WorktreeInfo {
            name: "feat-branch".into(),
            path: "/tmp/wt".into(),
            branch: "feat".into(),
            original_repo_dir: "/home/user/repo".into(),
        };
        let event = AgentEvent::SessionStart {
            agent: "claude".into(),
            cwd: "/tmp/wt".into(),
            permission_mode: "default".into(),
            source: String::new(),
            worktree: Some(wt.clone()),
            agent_id: Some("abc-123".into()),
            session_id: None,
        };
        match event {
            AgentEvent::SessionStart {
                worktree, agent_id, ..
            } => {
                let wt = worktree.unwrap();
                assert_eq!(wt.original_repo_dir, "/home/user/repo");
                assert_eq!(agent_id.unwrap(), "abc-123");
            }
            _ => panic!("wrong variant"),
        }
    }
}
