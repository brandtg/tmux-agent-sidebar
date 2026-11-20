pub const DEFAULT_BRANCH_PREFIX: &str = "agent/";
pub const DEFAULT_WORKTREE_DIR: &str = ".worktrees";
pub const DEFAULT_AGENT: &str = "claude";

/// Values the spawn modal's `MODE` field cycles through. `default`
/// launches the agent directly in the repo checkout; `worktree` creates
/// a dedicated git worktree first. Index into this table is how the
/// popup state stores the pick (`kind_idx`).
pub const SPAWN_MODES: &[&str] = &["default", "worktree"];

/// Width of the agent pane as a percentage of the pane it is split from
/// at spawn / resume time, leaving the rest of the window as a blank
/// shell the user can work in. The sidebar pane is added separately by
/// the after-new-window hook on the far left, so the agent ends up with
/// roughly a third of the screen right of the sidebar.
pub const AGENT_PANE_PERCENT: u32 = 33;

pub const AGENT_OPTION: &str = "@agent-sidebar-default-agent";
pub const BRANCH_PREFIX_OPTION: &str = "@agent-sidebar-branch-prefix";
pub const WORKTREE_DIR_OPTION: &str = "@agent-sidebar-worktree-dir";

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
