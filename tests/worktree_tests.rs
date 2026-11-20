use std::path::PathBuf;

use tmux_agent_sidebar::worktree::{
    AGENTS, SPAWN_MODES, agent_command, pick_unique_slug, slugify, worktree_path_for,
};

#[test]
fn slugify_lowercases_and_hyphenates_spaces() {
    assert_eq!(slugify("Add login form"), "add-login-form");
}

#[test]
fn slugify_collapses_runs_of_separators() {
    assert_eq!(slugify("Fix --  the   bug!!"), "fix-the-bug");
}

#[test]
fn slugify_strips_leading_and_trailing_punctuation() {
    assert_eq!(slugify("--hello--"), "hello");
    assert_eq!(slugify("   .. world .. "), "world");
}

#[test]
fn slugify_keeps_digits() {
    assert_eq!(slugify("issue 123 fix"), "issue-123-fix");
}

#[test]
fn slugify_returns_empty_for_pure_punctuation() {
    assert_eq!(slugify("!!!"), "");
    assert_eq!(slugify("   "), "");
    assert_eq!(slugify(""), "");
}

#[test]
fn slugify_drops_unicode_and_symbols() {
    // Non-ASCII characters are dropped; surrounding ASCII is still joined.
    assert_eq!(slugify("日本語 task"), "task");
}

#[test]
fn pick_unique_slug_returns_input_when_free() {
    let picked = pick_unique_slug("foo", |_| true).unwrap();
    assert_eq!(picked, "foo");
}

#[test]
fn pick_unique_slug_appends_suffix_on_collision() {
    let taken = ["foo".to_string()];
    let picked = pick_unique_slug("foo", |s| !taken.contains(&s.to_string())).unwrap();
    assert_eq!(picked, "foo-2");
}

#[test]
fn pick_unique_slug_skips_multiple_taken_suffixes() {
    let taken: Vec<String> = ["foo", "foo-2", "foo-3", "foo-4"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let picked = pick_unique_slug("foo", |s| !taken.contains(&s.to_string())).unwrap();
    assert_eq!(picked, "foo-5");
}

#[test]
fn pick_unique_slug_returns_none_when_exhausted() {
    let picked = pick_unique_slug("foo", |_| false);
    assert!(picked.is_none());
}

#[test]
fn worktree_path_uses_default_repo_local_directory() {
    let repo = PathBuf::from("/home/jess/code/myproj");
    let path = worktree_path_for(&repo, "feature", None).unwrap();
    assert_eq!(
        path,
        PathBuf::from("/home/jess/code/myproj/.worktrees/feature")
    );
}

#[test]
fn worktree_path_uses_custom_repo_relative_directory() {
    let repo = PathBuf::from("/home/jess/code/myproj");
    let path = worktree_path_for(&repo, "feature", Some(".worktrees")).unwrap();
    assert_eq!(
        path,
        PathBuf::from("/home/jess/code/myproj/.worktrees/feature")
    );
}

#[test]
fn worktree_path_handles_nested_custom_directory() {
    let repo = PathBuf::from("/home/jess/code/myproj");
    let path = worktree_path_for(&repo, "task-2", Some("tmp/worktrees")).unwrap();
    assert_eq!(
        path,
        PathBuf::from("/home/jess/code/myproj/tmp/worktrees/task-2")
    );
}

#[test]
fn worktree_path_empty_custom_directory_falls_back_to_default() {
    let repo = PathBuf::from("/tmp/repo");
    let path = worktree_path_for(&repo, "task-2", Some("")).unwrap();
    assert_eq!(path, PathBuf::from("/tmp/repo/.worktrees/task-2"));
}

#[test]
fn worktree_path_rejects_absolute_custom_directory() {
    let repo = PathBuf::from("/tmp/repo");
    assert!(worktree_path_for(&repo, "task-2", Some("/tmp/worktrees")).is_none());
}

#[test]
fn worktree_path_rejects_parent_relative_custom_directory() {
    let repo = PathBuf::from("/tmp/repo");
    assert!(worktree_path_for(&repo, "task-2", Some("../worktrees")).is_none());
}

// ─── agent_command / spawn modes ─────────────────────────────────────────

#[test]
fn agent_command_maps_known_agents_to_their_binaries() {
    assert_eq!(agent_command("claude"), "claude");
    assert_eq!(agent_command("codex"), "codex");
    assert_eq!(agent_command("opencode"), "opencode");
    assert_eq!(agent_command("antigravity"), "agy");
}

#[test]
fn agent_command_unknown_agent_is_echoed_raw() {
    assert_eq!(agent_command("gemini"), "gemini");
    assert_eq!(agent_command(""), "");
}

#[test]
fn spawn_modes_table_starts_with_folder_spawn() {
    // The modal stores the pick as an index; index 0 must stay the
    // plain folder spawn so the default flow never touches git.
    assert_eq!(SPAWN_MODES.first().copied(), Some("default"));
    assert_eq!(SPAWN_MODES.get(1).copied(), Some("worktree"));
}

#[test]
fn agents_list_is_non_empty_and_unique() {
    assert!(!AGENTS.is_empty());
    let mut seen = std::collections::HashSet::new();
    for a in AGENTS {
        assert!(seen.insert(*a), "duplicate agent {a:?}");
    }
}
