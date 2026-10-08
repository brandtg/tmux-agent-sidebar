pub const CLAUDE_AGENT: &str = "claude";
pub const CODEX_AGENT: &str = "codex";
pub const OPENCODE_AGENT: &str = "opencode";

/// Visual attention state stored in `@pane_attention`. `Notification`
/// means a hook wants the user's eye right now (waiting / permission /
/// teammate idle); `Done` means the agent finished its turn and the
/// user has not looked at the output yet. Both clear when the user
/// focuses the pane (sidebar-side) or on the next lifecycle hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaneAttention {
    #[default]
    None,
    Notification,
    Done,
}

impl PaneAttention {
    /// Parse the `@pane_attention` value written by hooks. Any
    /// non-empty unknown value still counts as attention (legacy
    /// `notification` semantics), so an older hook string can never
    /// silently drop a flag.
    pub fn from_label(s: &str) -> Self {
        match s {
            "done" => Self::Done,
            "" => Self::None,
            _ => Self::Notification,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub pane_id: String,
    pub pane_active: bool,
    pub status: PaneStatus,
    pub attention: PaneAttention,
    pub agent: AgentType,
    pub path: String,
    /// Launch-time cwd captured by hooks (`@pane_launch_cwd`). Repo
    /// grouping resolves this instead of `path` when set, so cd-ing into
    /// another checkout mid-session doesn't move the pane's group.
    pub launch_cwd: String,
    pub current_command: String,
    pub prompt: String,
    pub prompt_is_response: bool,
    pub started_at: Option<u64>,
    pub wait_reason: String,
    pub permission_mode: PermissionMode,
    pub subagents: Vec<String>,
    pub pane_pid: Option<u32>,
    pub worktree: WorktreeMetadata,
    pub session_id: Option<String>,
    /// Row title. Two sources: `@pane_session_title` (hook-provided,
    /// e.g. opencode) parsed straight from tmux, or the `session_id →
    /// name` map (Claude `/rename` labels) applied each tick by
    /// `refresh_session_names`, which leaves hook-provided titles alone.
    pub session_name: String,
    /// `true` when the window this pane lives in was created by the
    /// sidebar's spawn flow (via the `@agent-sidebar-spawned` window
    /// option). Used by the row renderer to show a clickable red `×`
    /// in place of the usual `+` worktree marker.
    pub sidebar_spawned: bool,
    /// Most recent backgrounded Bash command, if any. Populated while the
    /// pane status is `Background` (or `Running` with a backgrounded shell
    /// still alive) so the row body can surface the actual command.
    pub bg_shell_cmd: Option<String>,
}

impl PaneInfo {
    /// Directory repo grouping and the git-info cache are keyed by: the
    /// launch-time cwd when hooks captured one, else the live cwd.
    pub fn grouping_anchor(&self) -> &str {
        if self.launch_cwd.is_empty() {
            &self.path
        } else {
            &self.launch_cwd
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WorktreeMetadata {
    pub name: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PaneStatus {
    Running,
    Background,
    Waiting,
    Idle,
    Error,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PermissionMode {
    Default,
    Plan,
    AcceptEdits,
    Auto,
    DontAsk,
    BypassPermissions,
    Defer,
}

impl PermissionMode {
    /// Parse the permission-mode label written by agent hooks. Unknown
    /// values fall back to `Default`.
    pub fn from_label(s: &str) -> Self {
        match s {
            "plan" => Self::Plan,
            "acceptEdits" => Self::AcceptEdits,
            "auto" => Self::Auto,
            "dontAsk" => Self::DontAsk,
            "bypassPermissions" => Self::BypassPermissions,
            "defer" => Self::Defer,
            _ => Self::Default,
        }
    }

    pub fn badge(&self) -> &str {
        match self {
            Self::Default => "",
            Self::Plan => "plan",
            Self::AcceptEdits => "edit",
            Self::Auto => "auto",
            Self::DontAsk => "dontAsk",
            Self::BypassPermissions => "!",
            Self::Defer => "defer",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentType {
    Claude,
    Codex,
    OpenCode,
    #[allow(dead_code)]
    Unknown,
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub window_id: String,
    pub window_name: String,
    pub window_active: bool,
    pub auto_rename: bool,
    pub panes: Vec<PaneInfo>,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_name: String,
    pub windows: Vec<WindowInfo>,
}

/// A non-agent, non-sidebar pane. Retained by the tmux parse so the
/// sidebar can show the "other windows" sharing a repo with the agents.
#[derive(Debug, Clone)]
pub struct OtherPane {
    pub session_name: String,
    pub window_id: String,
    pub window_index: i64,
    pub window_name: String,
    pub window_active: bool,
    pub pane_id: String,
    pub pane_active: bool,
    pub path: String,
    pub command: String,
    pub pane_pid: Option<u32>,
    /// Basename of the last foreground command the pane's shell recorded
    /// via the shell-integration snippet. Empty/None when no integration
    /// is active; then task-finish confirmation falls back to the plain
    /// task→shell command transition.
    pub last_cmd: Option<String>,
    /// Exit code of the last completed foreground command (same snippet).
    /// `None` when no integration is active or the value failed to parse.
    pub last_exit: Option<i64>,
}

/// Coarse status for a non-agent window row. Distinct from
/// [`PaneStatus`] because windows have no hooks and must never be
/// counted by the agent status filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowStatus {
    Idle,
    Busy,
    Task,
}

impl AgentType {
    /// Parse the agent label set by hooks. Returns `None` for unknown
    /// values so callers can skip non-agent panes.
    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            CLAUDE_AGENT => Some(Self::Claude),
            CODEX_AGENT => Some(Self::Codex),
            OPENCODE_AGENT => Some(Self::OpenCode),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Claude => CLAUDE_AGENT,
            Self::Codex => CODEX_AGENT,
            Self::OpenCode => OPENCODE_AGENT,
            Self::Unknown => "unknown",
        }
    }

    pub fn label(&self) -> &str {
        self.as_str()
    }
}

impl PaneStatus {
    /// Parse the status label written by agent hooks. Unknown values
    /// map to `Unknown`.
    pub fn from_label(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "background" => Self::Background,
            "waiting" | "notification" => Self::Waiting,
            "idle" => Self::Idle,
            "error" => Self::Error,
            _ => Self::Unknown,
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Running => "●",
            Self::Background => "◎",
            Self::Waiting => "◐",
            Self::Idle => "○",
            Self::Error => "✕",
            Self::Unknown => "·",
        }
    }

    /// `true` when the agent (or an owned background shell) is still
    /// doing work the user would expect to be timed or updated live.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Running | Self::Background | Self::Waiting)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_status_from_str_all_variants() {
        assert_eq!(PaneStatus::from_label("running"), PaneStatus::Running);
        assert_eq!(PaneStatus::from_label("background"), PaneStatus::Background);
        assert_eq!(PaneStatus::from_label("waiting"), PaneStatus::Waiting);
        assert_eq!(PaneStatus::from_label("notification"), PaneStatus::Waiting);
        assert_eq!(PaneStatus::from_label("idle"), PaneStatus::Idle);
        assert_eq!(PaneStatus::from_label("error"), PaneStatus::Error);
        assert_eq!(PaneStatus::from_label("anything"), PaneStatus::Unknown);
        assert_eq!(PaneStatus::from_label(""), PaneStatus::Unknown);
    }

    #[test]
    fn pane_status_icon_all_variants() {
        assert_eq!(PaneStatus::Running.icon(), "●");
        assert_eq!(PaneStatus::Background.icon(), "◎");
        assert_eq!(PaneStatus::Waiting.icon(), "◐");
        assert_eq!(PaneStatus::Idle.icon(), "○");
        assert_eq!(PaneStatus::Error.icon(), "✕");
        assert_eq!(PaneStatus::Unknown.icon(), "·");
    }

    #[test]
    fn pane_attention_from_label_all_values() {
        assert_eq!(PaneAttention::from_label(""), PaneAttention::None);
        assert_eq!(
            PaneAttention::from_label("notification"),
            PaneAttention::Notification
        );
        assert_eq!(PaneAttention::from_label("done"), PaneAttention::Done);
        // Unknown non-empty values keep the legacy "any flag = attention"
        // contract instead of silently dropping the indicator.
        assert_eq!(
            PaneAttention::from_label("something-else"),
            PaneAttention::Notification
        );
    }

    #[test]
    fn agent_type_from_str_all() {
        assert_eq!(AgentType::from_label("claude"), Some(AgentType::Claude));
        assert_eq!(AgentType::from_label("codex"), Some(AgentType::Codex));
        assert_eq!(AgentType::from_label("opencode"), Some(AgentType::OpenCode));
        assert_eq!(AgentType::from_label("unknown"), None);
        assert_eq!(AgentType::from_label(""), None);
    }

    #[test]
    fn agent_type_label() {
        assert_eq!(AgentType::Claude.label(), "claude");
        assert_eq!(AgentType::Codex.label(), "codex");
        assert_eq!(AgentType::OpenCode.label(), "opencode");
        assert_eq!(AgentType::Unknown.label(), "unknown");
    }

    #[test]
    fn agent_type_as_str_matches_constants() {
        assert_eq!(AgentType::Claude.as_str(), CLAUDE_AGENT);
        assert_eq!(AgentType::Codex.as_str(), CODEX_AGENT);
        assert_eq!(AgentType::OpenCode.as_str(), OPENCODE_AGENT);
    }

    #[test]
    fn permission_mode_from_str_all() {
        assert_eq!(
            PermissionMode::from_label("default"),
            PermissionMode::Default
        );
        assert_eq!(PermissionMode::from_label("plan"), PermissionMode::Plan);
        assert_eq!(
            PermissionMode::from_label("acceptEdits"),
            PermissionMode::AcceptEdits
        );
        assert_eq!(PermissionMode::from_label("auto"), PermissionMode::Auto);
        assert_eq!(
            PermissionMode::from_label("dontAsk"),
            PermissionMode::DontAsk
        );
        assert_eq!(
            PermissionMode::from_label("bypassPermissions"),
            PermissionMode::BypassPermissions
        );
        assert_eq!(PermissionMode::from_label("defer"), PermissionMode::Defer);
        assert_eq!(PermissionMode::from_label(""), PermissionMode::Default);
        assert_eq!(
            PermissionMode::from_label("unknown"),
            PermissionMode::Default
        );
    }

    #[test]
    fn permission_mode_badge() {
        assert_eq!(PermissionMode::Default.badge(), "");
        assert_eq!(PermissionMode::Plan.badge(), "plan");
        assert_eq!(PermissionMode::AcceptEdits.badge(), "edit");
        assert_eq!(PermissionMode::Auto.badge(), "auto");
        assert_eq!(PermissionMode::DontAsk.badge(), "dontAsk");
        assert_eq!(PermissionMode::BypassPermissions.badge(), "!");
        assert_eq!(PermissionMode::Defer.badge(), "defer");
    }
}
