use std::path::Path;

use super::config::{BRANCH_PREFIX_OPTION, SPAWN_LAYOUT_OPTION, WORKTREE_DIR_OPTION};
use crate::{git, tmux};

/// All side effects `spawn_with` / `remove_with` depend on, extracted
/// so failure-path tests can script tmux/git errors without touching a
/// real tmux server or repository. The real implementation lives in
/// `RealEnv` below.
pub(crate) trait SpawnEnv {
    fn branch_prefix(&self) -> Option<String>;
    fn worktree_dir(&self) -> Option<String>;
    /// Raw `@agent-sidebar-spawn-layout` option value; parsed by
    /// [`super::config::spawn_layout`]. `None` = unset (default layout).
    fn spawn_layout(&self) -> Option<String>;
    fn branch_is_free(&self, repo: &str, branch: &str) -> bool;
    /// Whether a branch currently exists in `repo`. Used by the
    /// remove flow to skip `git branch -D` when a previous partial
    /// success already dropped the branch, so retries converge.
    fn branch_exists(&self, repo: &str, branch: &str) -> bool;
    fn worktree_path_is_free(&self, path: &Path) -> bool;
    /// Whether the worktree directory is currently on disk. Distinct
    /// from [`Self::worktree_path_is_free`] only because the fake
    /// implementations in tests need opposite default polarities for
    /// spawn (`is_free=true`, picks a fresh slug) and remove
    /// (`exists=true`, runs the cleanup).
    fn worktree_path_exists(&self, path: &str) -> bool;
    fn worktree_add(&self, repo: &str, worktree_path: &str, branch: &str) -> Result<(), String>;
    fn worktree_remove(&self, repo: &str, worktree_path: &str) -> Result<(), String>;
    fn branch_delete(&self, repo: &str, branch: &str) -> Result<(), String>;
    fn new_window(&self, session: &str, cwd: &str, name: &str) -> Result<(String, String), String>;
    /// Split `target_pane` so the new pane sits to its left at `percent`
    /// of its width, starting in `cwd`, without changing the active pane.
    /// Returns the new pane id. The spawn / resume flows degrade to the
    /// single-pane layout (agent takes over the target pane) on error.
    fn split_window_left(
        &self,
        target_pane: &str,
        cwd: &str,
        percent: u32,
    ) -> Result<String, String>;
    fn kill_window(&self, window_id: &str) -> Result<(), String>;
    /// Move the invoking tmux client to `window_id` inside `session`.
    /// Best-effort tail of the spawn flow: `new-window` only selects
    /// the new window within its own session, so a spawn targeting a
    /// session other than the sidebar's must switch the client over or
    /// the user sees nothing happen.
    fn switch_client_to_window(&self, session: &str, window_id: &str) -> Result<(), String>;
    fn set_window_option(&self, window: &str, key: &str, value: &str) -> Result<(), String>;
    fn send_command(&self, target: &str, command: &str) -> Result<(), String>;
    fn display_message(&self, pane_id: &str, template: &str) -> String;
}

pub(super) struct RealEnv;

impl SpawnEnv for RealEnv {
    fn branch_prefix(&self) -> Option<String> {
        tmux::get_all_global_options()
            .get(BRANCH_PREFIX_OPTION)
            .cloned()
    }
    fn worktree_dir(&self) -> Option<String> {
        tmux::get_all_global_options()
            .get(WORKTREE_DIR_OPTION)
            .cloned()
    }
    fn spawn_layout(&self) -> Option<String> {
        tmux::get_all_global_options()
            .get(SPAWN_LAYOUT_OPTION)
            .cloned()
    }
    fn branch_is_free(&self, repo: &str, branch: &str) -> bool {
        !git::branch_exists(repo, branch)
    }
    fn branch_exists(&self, repo: &str, branch: &str) -> bool {
        git::branch_exists(repo, branch)
    }
    fn worktree_path_is_free(&self, path: &Path) -> bool {
        !path.exists()
    }
    fn worktree_path_exists(&self, path: &str) -> bool {
        !path.is_empty() && Path::new(path).exists()
    }
    fn worktree_add(&self, repo: &str, path: &str, branch: &str) -> Result<(), String> {
        git::worktree_add(repo, path, branch)
    }
    fn worktree_remove(&self, repo: &str, path: &str) -> Result<(), String> {
        git::worktree_remove(repo, path)
    }
    fn branch_delete(&self, repo: &str, branch: &str) -> Result<(), String> {
        git::branch_delete(repo, branch)
    }
    fn new_window(&self, session: &str, cwd: &str, name: &str) -> Result<(String, String), String> {
        tmux::new_window(session, cwd, name)
    }
    fn split_window_left(
        &self,
        target_pane: &str,
        cwd: &str,
        percent: u32,
    ) -> Result<String, String> {
        tmux::split_window_left(target_pane, cwd, percent)
    }
    fn kill_window(&self, window_id: &str) -> Result<(), String> {
        tmux::kill_window(window_id)
    }
    fn switch_client_to_window(&self, session: &str, window_id: &str) -> Result<(), String> {
        tmux::switch_to_window(session, window_id)
    }
    fn set_window_option(&self, window: &str, key: &str, value: &str) -> Result<(), String> {
        tmux::set_window_option(window, key, value)
    }
    fn send_command(&self, target: &str, command: &str) -> Result<(), String> {
        tmux::send_command(target, command)
    }
    fn display_message(&self, pane_id: &str, template: &str) -> String {
        tmux::display_message(pane_id, template)
    }
}
