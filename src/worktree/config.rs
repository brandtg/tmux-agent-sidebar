pub const DEFAULT_BRANCH_PREFIX: &str = "agent/";
pub const DEFAULT_WORKTREE_DIR: &str = ".worktrees";
pub const DEFAULT_AGENT: &str = "claude";

/// Values the spawn modal's `MODE` field cycles through. `default`
/// launches the agent directly in the repo checkout; `worktree` creates
/// a dedicated git worktree first. Index into this table is how the
/// popup state stores the pick (`kind_idx`).
pub const SPAWN_MODES: &[&str] = &["default", "worktree"];

/// Width of the agent pane as a percentage of the pane it is split from
/// when the spawn layout is `split`, leaving the rest of the window as
/// a blank shell the user can work in. The sidebar pane is added
/// separately by the after-new-window hook on the far left, so the
/// agent ends up with roughly a third of the screen right of the
/// sidebar. The default `full` layout does not split at all.
pub const AGENT_PANE_PERCENT: u32 = 33;

pub const AGENT_OPTION: &str = "@agent-sidebar-default-agent";
pub const BRANCH_PREFIX_OPTION: &str = "@agent-sidebar-branch-prefix";
pub const WORKTREE_DIR_OPTION: &str = "@agent-sidebar-worktree-dir";
/// Window layout for freshly launched agent windows (spawn + resume):
/// `full` hands the agent the whole window (the user makes their own
/// splits), `split` keeps the classic side-by-side — a left one-third
/// agent pane beside a blank shell. Anything unset or unrecognised
/// falls back to [`DEFAULT_SPAWN_LAYOUT`].
pub const SPAWN_LAYOUT_OPTION: &str = "@sidebar_spawn_layout";
pub const SPAWN_LAYOUT_FULL: &str = "full";
pub const SPAWN_LAYOUT_SPLIT: &str = "split";
pub const DEFAULT_SPAWN_LAYOUT: &str = SPAWN_LAYOUT_FULL;

/// Parsed view of [`SPAWN_LAYOUT_OPTION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpawnLayout {
    /// The agent takes the whole window; the user makes their own splits.
    #[default]
    Full,
    /// Agent in a left one-third pane beside a blank right shell.
    Split,
}

/// Resolve the raw `@sidebar_spawn_layout` option value. Unset /
/// empty / unknown values fall back to the default (`full`) so a stale
/// or mistyped option can never block spawning.
pub fn spawn_layout(value: Option<&str>) -> SpawnLayout {
    match value {
        Some(SPAWN_LAYOUT_SPLIT) => SpawnLayout::Split,
        _ => SpawnLayout::Full,
    }
}

pub const AGENTS: &[&str] = &["claude", "codex", "opencode", "antigravity"];

/// Compose the shell command to run inside the new pane. Unknown
/// agents fall back to launching the name verbatim.
pub fn agent_command(agent: &str) -> String {
    match agent {
        "claude" => "claude".into(),
        "codex" => "codex".into(),
        "opencode" => "opencode".into(),
        "antigravity" => "agy".into(),
        other => other.to_string(),
    }
}

/// How much to clean up when the user presses `x` on a spawn-created pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveMode {
    /// Only `tmux kill-window`. The git worktree and branch stay.
    WindowOnly,
    /// Kill the window AND `git worktree remove --force`.
    WindowAndWorktree,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_modes_order_is_stable() {
        // The popup stores the MODE pick as an index into SPAWN_MODES;
        // 0 must stay the no-worktree spawn and 1 the worktree spawn.
        assert_eq!(SPAWN_MODES, &["default", "worktree"]);
    }

    #[test]
    fn spawn_layout_parses_the_split_value_only() {
        // `full` is the default: unset, empty, unknown, and the literal
        // `full` all resolve to it; only `split` selects the split.
        assert_eq!(spawn_layout(None), SpawnLayout::Full);
        assert_eq!(spawn_layout(Some("")), SpawnLayout::Full);
        assert_eq!(spawn_layout(Some("full")), SpawnLayout::Full);
        assert_eq!(spawn_layout(Some("sideways")), SpawnLayout::Full);
        assert_eq!(spawn_layout(Some(SPAWN_LAYOUT_SPLIT)), SpawnLayout::Split);
    }

    #[test]
    fn agent_command_maps_known_agents() {
        assert_eq!(agent_command("claude"), "claude");
        assert_eq!(agent_command("codex"), "codex");
        assert_eq!(agent_command("opencode"), "opencode");
        // Antigravity's executable is `agy`, not its label.
        assert_eq!(agent_command("antigravity"), "agy");
    }

    #[test]
    fn agent_command_unknown_agent_passes_through() {
        assert_eq!(agent_command("gemini"), "gemini");
        assert_eq!(agent_command(""), "");
    }

    #[test]
    fn agents_list_commands_are_non_empty() {
        // Every agent listed in the public AGENTS catalog must resolve
        // to a launchable command; antigravity's executable differs
        // from its label (`agy`).
        for agent in AGENTS {
            assert!(
                !agent_command(agent).is_empty(),
                "agent {agent} must resolve to a non-empty command"
            );
        }
        assert_eq!(agent_command("antigravity"), "agy");
    }
}
