use std::path::{Component, Path, PathBuf};

use super::config::{AGENT_PANE_PERCENT, DEFAULT_BRANCH_PREFIX, RemoveMode};
use super::env::{RealEnv, SpawnEnv};
use super::markers::{
    SPAWNED_BRANCH_OPTION, SPAWNED_FROM_OPTION, SPAWNED_OPTION, SPAWNED_WORKTREE_OPTION,
    SpawnMarkers, spawn_markers_template,
};
use super::slug::{MAX_COLLISION_ATTEMPTS, pick_unique_slug, slugify, worktree_path_for};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpawnKind {
    /// Launch the agent directly in the repo checkout: a new tmux
    /// window at the repo root with the usual agent-pane layout, no git
    /// side effects.
    #[default]
    Folder,
    /// Create a git worktree + branch first, then launch the agent
    /// inside it (the original spawn behaviour).
    Worktree,
}

impl SpawnKind {
    /// Map the spawn modal's `MODE` index ([`SPAWN_MODES`]) to a kind.
    pub fn from_mode_index(idx: usize) -> Self {
        match idx {
            1 => Self::Worktree,
            _ => Self::Folder,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub repo_root: PathBuf,
    /// Slug source for the worktree/branch. Unused by [`SpawnKind::Folder`].
    pub task_name: String,
    pub session: String,
    pub agent: String,
    pub kind: SpawnKind,
}

/// Spawn an agent for `req`: either directly in the repo folder or in a
/// fresh git worktree, depending on `req.kind`. Returns the resulting
/// branch name (worktree) or window id (folder) on success.
pub fn spawn(req: &SpawnRequest) -> Result<String, String> {
    spawn_with(&RealEnv, req)
}

pub(crate) fn spawn_with<E: SpawnEnv>(env: &E, req: &SpawnRequest) -> Result<String, String> {
    match req.kind {
        SpawnKind::Folder => spawn_folder(env, req),
        SpawnKind::Worktree => spawn_worktree(env, req),
    }
}

/// Pick the pane the agent command is sent to for a freshly created
/// window, per the `@sidebar_spawn_layout` option. `full` (the
/// default) hands the agent the whole window — the user makes their own
/// splits; `split` keeps the classic layout: a left one-third agent
/// pane beside a blank right shell (the after-new-window hook adds the
/// sidebar pane by geometry either way). A failed split degrades to the
/// single-pane layout — the agent takes over the original pane and the
/// spawn still succeeds.
fn agent_launch_pane<E: SpawnEnv>(env: &E, base_pane: &str, cwd: &str) -> String {
    match super::config::spawn_layout(env.spawn_layout().as_deref()) {
        super::config::SpawnLayout::Full => base_pane.to_string(),
        super::config::SpawnLayout::Split => {
            match env.split_window_left(base_pane, cwd, AGENT_PANE_PERCENT) {
                Ok(pane) => pane,
                Err(e) => {
                    crate::debug::log(&format!(
                        "spawn: agent-pane split failed, single-pane fallback: {e}"
                    ));
                    base_pane.to_string()
                }
            }
        }
    }
}

/// Folder spawn: open a new tmux window at the repo root, add the
/// agent-pane split, stash the spawned markers (without worktree /
/// branch — the remove flow treats those as "window only"), and launch
/// the agent. No git operations at all.
fn spawn_folder<E: SpawnEnv>(env: &E, req: &SpawnRequest) -> Result<String, String> {
    let repo = req
        .repo_root
        .to_str()
        .ok_or("repo root is not valid UTF-8")?;

    let (pane_id, window_id) = env
        .new_window(&req.session, repo, &req.agent)
        .map_err(|e| format!("tmux: {e}"))?;

    let agent_pane = agent_launch_pane(env, &pane_id, repo);

    // Window scope so sub panes (e.g. Claude Code subagents split from
    // the original) inherit the markers via tmux's option fall-through.
    // No worktree / branch markers: an empty worktree path is what tells
    // the remove flow this pane has no git cleanup to do.
    for (key, value) in [(SPAWNED_OPTION, "1"), (SPAWNED_FROM_OPTION, repo)] {
        if let Err(e) = env.set_window_option(&window_id, key, value) {
            let rb = rollback_folder_spawn(env, &window_id);
            return Err(compose_spawn_error(
                format!("tmux: failed to set {key}: {e}"),
                rb,
            ));
        }
    }

    if let Err(e) = env.send_command(&agent_pane, &super::config::agent_command(&req.agent)) {
        let rb = rollback_folder_spawn(env, &window_id);
        return Err(compose_spawn_error(format!("tmux: {e}"), rb));
    }

    switch_to_spawned_window(env, &req.session, &window_id);

    Ok(window_id)
}

/// Best-effort rollback after a partial folder spawn. There is no git
/// state to clean up, so killing the window is the whole job.
fn rollback_folder_spawn<E: SpawnEnv>(env: &E, window_id: &str) -> Vec<String> {
    match env.kill_window(window_id) {
        Ok(()) => Vec::new(),
        Err(e) => vec![format!("kill_window: {e}")],
    }
}

/// Worktree spawn: create a worktree, open a new tmux window in it,
/// launch the agent, and stash markers at window scope so the matching
/// `x` flow can find it later. Returns the resulting branch name on
/// success. On any failure past `git worktree add` the worktree is
/// rolled back.
fn spawn_worktree<E: SpawnEnv>(env: &E, req: &SpawnRequest) -> Result<String, String> {
    let slug = slugify(&req.task_name);
    if slug.is_empty() {
        return Err("name is empty after slugification".into());
    }
    let repo = req
        .repo_root
        .to_str()
        .ok_or("repo root is not valid UTF-8")?;

    let prefix = env
        .branch_prefix()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BRANCH_PREFIX.to_string());
    let worktree_dir = env.worktree_dir();
    if worktree_dir.as_deref().is_some_and(|dir| {
        let dir = Path::new(dir);
        !dir.as_os_str().is_empty()
            && (dir.is_absolute() || dir.components().any(|c| c == Component::ParentDir))
    }) {
        return Err("worktree directory must be repo-relative".into());
    }

    let unique = pick_unique_slug(&slug, |s| {
        let branch = format!("{prefix}{s}");
        env.branch_is_free(repo, &branch)
            && worktree_path_for(&req.repo_root, s, worktree_dir.as_deref())
                .is_some_and(|p| env.worktree_path_is_free(&p))
    })
    .ok_or_else(|| format!("no free branch name found after {MAX_COLLISION_ATTEMPTS} attempts"))?;

    let branch = format!("{prefix}{unique}");
    let worktree_path = worktree_path_for(&req.repo_root, &unique, worktree_dir.as_deref())
        .ok_or("worktree directory must be repo-relative")?;
    let worktree = worktree_path.to_str().ok_or("worktree path is not UTF-8")?;

    env.worktree_add(repo, worktree, &branch)
        .map_err(|e| format!("git: {e}"))?;

    let (pane_id, window_id) = env
        .new_window(&req.session, worktree, &unique)
        .map_err(|e| {
            let rb = rollback_spawn(env, repo, worktree, &branch, None);
            compose_spawn_error(format!("tmux: {e}"), rb)
        })?;

    let agent_pane = agent_launch_pane(env, &pane_id, worktree);

    // Window scope so sub panes (e.g. Claude Code subagents split from
    // the original) inherit the markers via tmux's option fall-through.
    for (key, value) in [
        (SPAWNED_OPTION, "1"),
        (SPAWNED_FROM_OPTION, repo),
        (SPAWNED_WORKTREE_OPTION, worktree),
        (SPAWNED_BRANCH_OPTION, &branch),
    ] {
        if let Err(e) = env.set_window_option(&window_id, key, value) {
            let rb = rollback_spawn(env, repo, worktree, &branch, Some(&window_id));
            return Err(compose_spawn_error(
                format!("tmux: failed to set {key}: {e}"),
                rb,
            ));
        }
    }

    if let Err(e) = env.send_command(&agent_pane, &super::config::agent_command(&req.agent)) {
        let rb = rollback_spawn(env, repo, worktree, &branch, Some(&window_id));
        return Err(compose_spawn_error(format!("tmux: {e}"), rb));
    }

    switch_to_spawned_window(env, &req.session, &window_id);

    Ok(branch)
}

/// Best-effort client switch to the freshly spawned window. `new-window`
/// only selects the new window within its own session, so a spawn that
/// targets a session other than the sidebar's own would otherwise be
/// invisible: the user stays put while the window appears elsewhere. A
/// failure (e.g. no attached client when the sidebar runs detached) is
/// logged and ignored — the spawn itself already succeeded.
fn switch_to_spawned_window<E: SpawnEnv>(env: &E, session: &str, window_id: &str) {
    if let Err(e) = env.switch_client_to_window(session, window_id) {
        crate::debug::log(&format!(
            "spawn: client switch to {session}:{window_id} failed: {e}"
        ));
    }
}

/// Best-effort rollback after a partial spawn. Kills the tmux window
/// (when one was created), removes the git worktree, and deletes the
/// branch ref that `git worktree add -b` created. Each step collects
/// its error so the caller can surface a full picture of what is
/// still lying around on disk / in tmux. Deleting the branch is
/// important: `worktree remove --force` leaves the branch behind,
/// which later spawns would then collide with.
fn rollback_spawn<E: SpawnEnv>(
    env: &E,
    repo: &str,
    worktree_path: &str,
    branch: &str,
    window_id: Option<&str>,
) -> Vec<String> {
    let mut errs = Vec::new();
    if let Some(window_id) = window_id
        && let Err(e) = env.kill_window(window_id)
    {
        errs.push(format!("kill_window: {e}"));
    }
    if let Err(e) = env.worktree_remove(repo, worktree_path) {
        errs.push(format!("worktree_remove: {e}"));
    }
    if let Err(e) = env.branch_delete(repo, branch) {
        errs.push(format!("branch_delete: {e}"));
    }
    errs
}

/// Combine the primary spawn error with any rollback failures so the
/// user sees a single string that covers both the trigger and the
/// state left behind.
fn compose_spawn_error(primary: String, rollback_errs: Vec<String>) -> String {
    if rollback_errs.is_empty() {
        primary
    } else {
        format!(
            "{primary} (rollback incomplete: {})",
            rollback_errs.join("; ")
        )
    }
}

/// Pre-flight consequence summary shown in the remove-confirm dialog.
///
/// The remove flow runs `git worktree remove --force` + `git branch -D`
/// while the agent is still running: uncommitted changes and untracked
/// files are unrecoverable, and unpushed commits survive only in the
/// reflog. The dialog spells those consequences out; when the worktree
/// can still be inspected, the message is tailored with a dirty /
/// unpushed-commits pre-flight check. Unknown state (git failed) warns
/// worst-case.
pub fn removal_warnings(worktree_path: &str) -> Vec<String> {
    let dirty = match crate::git::run_git_capture(worktree_path, &["status", "--porcelain"]) {
        Ok(status) => !status.is_empty(),
        Err(_) => true,
    };
    let unpushed = crate::git::run_git_capture(
        worktree_path,
        &["rev-list", "--count", "HEAD", "--not", "--remotes"],
    )
    .ok()
    .and_then(|out| out.parse::<usize>().ok())
    .unwrap_or(0);

    let mut lines = Vec::new();
    if dirty {
        lines.push("Deletes uncommitted".to_string());
        lines.push("changes, untracked files".to_string());
    }
    if unpushed > 0 {
        lines.push(format!("{unpushed} unpushed"));
        lines.push("commits: reflog only".to_string());
    }
    if lines.is_empty() {
        lines.push("Worktree is clean;".to_string());
        lines.push("deletes branch + checkout".to_string());
    }
    lines
}

/// Tear down a previously-spawned pane. Runs ALL git cleanup
/// (`worktree remove --force`, then `git branch -D`) BEFORE killing
/// the tmux window so a git failure at any step leaves the window
/// (and its markers) intact — the window is the only UI handle the
/// retry path depends on, so killing it first would strand any
/// leftover git state with no way to finish cleanup from the
/// sidebar. Each git step is skipped when its target is already
/// gone (`worktree_path_exists` / `branch_exists`), which lets
/// retries after a partial-success failure converge.
///
/// Folder spawns (`SpawnKind::Folder`) carry no worktree / branch
/// markers, so there is nothing to clean up and the whole flow
/// degenerates to `kill_window` — every remove mode behaves the same.
pub fn remove(pane_id: &str, mode: RemoveMode) -> Result<(), String> {
    remove_with(&RealEnv, pane_id, mode)
}

pub(crate) fn remove_with<E: SpawnEnv>(
    env: &E,
    pane_id: &str,
    mode: RemoveMode,
) -> Result<(), String> {
    let markers = SpawnMarkers::parse(&env.display_message(pane_id, &spawn_markers_template()));
    if !markers.is_spawned() {
        return Err("pane was not created by sidebar spawn".into());
    }
    if markers.window_id.is_empty() {
        return Err("could not resolve window id".into());
    }
    // Empty worktree path = folder spawn (no git state). A worktree
    // spawn always writes both markers, so a missing branch there is
    // still a corruption error.
    let folder_spawn = markers.worktree_path.is_empty();
    if !folder_spawn && markers.branch.is_empty() {
        return Err("spawned branch is unset".into());
    }

    if mode == RemoveMode::WindowAndWorktree && !folder_spawn {
        if env.worktree_path_exists(&markers.worktree_path) {
            env.worktree_remove(&markers.from_repo, &markers.worktree_path)
                .map_err(|e| format!("git: {e}"))?;
        }
        // `git worktree remove` leaves the branch ref behind; drop
        // it here, before `kill_window`, so a failure still leaves
        // the window as a retry handle.
        if env.branch_exists(&markers.from_repo, &markers.branch) {
            env.branch_delete(&markers.from_repo, &markers.branch)
                .map_err(|e| format!("git: {e}"))?;
        }
    }
    env.kill_window(&markers.window_id)
        .map_err(|e| format!("tmux: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod env_tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::Path;

    #[derive(Default)]
    struct FakeEnv {
        calls: RefCell<Vec<String>>,
        set_option_calls: RefCell<usize>,
        worktree_dir: Option<String>,
        /// Raw `@sidebar_spawn_layout` value (`None` = unset,
        /// which resolves to the default `full` layout).
        spawn_layout: Option<String>,
        fail_set_option_at: Option<usize>,
        fail_split: bool,
        fail_kill_window: bool,
        fail_worktree_remove: bool,
        fail_branch_delete: bool,
        fail_send_command: bool,
        display_output: Option<String>,
        /// Programs `worktree_path_is_free`. `None` = default false
        /// (the worktree exists on disk). `Some(true)` = the path was
        /// already cleaned up (e.g. on a retry after a partial
        /// failure) so the remove flow should skip the git step.
        worktree_path_already_gone: Option<bool>,
        /// Programs `branch_exists` for remove tests. `None` / `false`
        /// = branch still present (default), `Some(true)` = the
        /// branch was already dropped by a previous partial success
        /// so the remove flow should skip `git branch -D`.
        branch_already_gone: Option<bool>,
    }

    impl FakeEnv {
        fn log(&self, s: String) {
            self.calls.borrow_mut().push(s);
        }
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl SpawnEnv for FakeEnv {
        fn branch_prefix(&self) -> Option<String> {
            None
        }
        fn worktree_dir(&self) -> Option<String> {
            self.worktree_dir.clone()
        }
        fn spawn_layout(&self) -> Option<String> {
            self.spawn_layout.clone()
        }
        fn branch_is_free(&self, _repo: &str, _branch: &str) -> bool {
            true
        }
        fn branch_exists(&self, _repo: &str, _branch: &str) -> bool {
            !self.branch_already_gone.unwrap_or(false)
        }
        fn worktree_path_is_free(&self, _path: &Path) -> bool {
            true
        }
        fn worktree_path_exists(&self, _path: &str) -> bool {
            !self.worktree_path_already_gone.unwrap_or(false)
        }
        fn worktree_add(&self, repo: &str, path: &str, branch: &str) -> Result<(), String> {
            self.log(format!("worktree_add({repo},{path},{branch})"));
            Ok(())
        }
        fn worktree_remove(&self, repo: &str, path: &str) -> Result<(), String> {
            self.log(format!("worktree_remove({repo},{path})"));
            if self.fail_worktree_remove {
                Err("worktree_remove failed".into())
            } else {
                Ok(())
            }
        }
        fn branch_delete(&self, repo: &str, branch: &str) -> Result<(), String> {
            self.log(format!("branch_delete({repo},{branch})"));
            if self.fail_branch_delete {
                Err("branch_delete failed".into())
            } else {
                Ok(())
            }
        }
        fn new_window(
            &self,
            session: &str,
            cwd: &str,
            name: &str,
        ) -> Result<(String, String), String> {
            self.log(format!("new_window({session},{cwd},{name})"));
            Ok(("%1".into(), "@1".into()))
        }
        fn split_window_left(
            &self,
            target_pane: &str,
            cwd: &str,
            percent: u32,
        ) -> Result<String, String> {
            self.log(format!("split_window_left({target_pane},{cwd},{percent})"));
            if self.fail_split {
                Err("split_window_left failed".into())
            } else {
                Ok("%2".into())
            }
        }
        fn kill_window(&self, window_id: &str) -> Result<(), String> {
            self.log(format!("kill_window({window_id})"));
            if self.fail_kill_window {
                Err("kill_window failed".into())
            } else {
                Ok(())
            }
        }
        fn switch_client_to_window(&self, session: &str, window_id: &str) -> Result<(), String> {
            self.log(format!("switch_client_to_window({session},{window_id})"));
            Ok(())
        }
        fn set_window_option(&self, window: &str, key: &str, _value: &str) -> Result<(), String> {
            let idx = *self.set_option_calls.borrow();
            *self.set_option_calls.borrow_mut() += 1;
            self.log(format!("set_window_option({window},{key})"));
            if Some(idx) == self.fail_set_option_at {
                Err(format!("set {key} failed"))
            } else {
                Ok(())
            }
        }
        fn send_command(&self, target: &str, command: &str) -> Result<(), String> {
            self.log(format!("send_command({target},{command})"));
            if self.fail_send_command {
                Err("send_command failed".into())
            } else {
                Ok(())
            }
        }
        fn display_message(&self, _pane_id: &str, _template: &str) -> String {
            self.display_output
                .clone()
                .unwrap_or_else(|| "1\n/r\n/r/.worktrees/task\nagent/task\n@1\n".into())
        }
    }

    fn sample_req() -> SpawnRequest {
        SpawnRequest {
            repo_root: PathBuf::from("/r"),
            task_name: "task".into(),
            session: "sess".into(),
            agent: "claude".into(),
            kind: SpawnKind::Worktree,
        }
    }

    fn folder_req() -> SpawnRequest {
        SpawnRequest {
            kind: SpawnKind::Folder,
            ..sample_req()
        }
    }

    fn has_call(calls: &[String], prefix: &str) -> bool {
        calls.iter().any(|c| c.starts_with(prefix))
    }

    #[test]
    fn spawn_kind_from_mode_index_maps_both_rows() {
        assert_eq!(SpawnKind::from_mode_index(0), SpawnKind::Folder);
        assert_eq!(SpawnKind::from_mode_index(1), SpawnKind::Worktree);
        // Out-of-range indices fall back to the no-worktree spawn.
        assert_eq!(SpawnKind::from_mode_index(9), SpawnKind::Folder);
    }

    #[test]
    fn folder_spawn_touches_no_git_and_sets_window_only_markers() {
        let env = FakeEnv::default();
        let got = spawn_with(&env, &folder_req()).expect("folder spawn should succeed");
        assert_eq!(got, "@1", "folder spawn returns the new window id");
        let calls = env.calls();
        assert!(
            !has_call(&calls, "worktree_add(")
                && !has_call(&calls, "branch_delete(")
                && !has_call(&calls, "worktree_remove("),
            "folder spawn must not touch git: {calls:?}"
        );
        assert!(
            has_call(&calls, "new_window(sess,/r,claude)"),
            "window opens at the repo root, named after the agent: {calls:?}"
        );
        assert!(
            !has_call(&calls, "split_window_left("),
            "the default `full` layout gives the agent the whole window: {calls:?}"
        );
        assert_eq!(
            *env.set_option_calls.borrow(),
            2,
            "only spawned + from markers, no worktree/branch"
        );
        assert!(
            has_call(&calls, "send_command(%1,claude)"),
            "agent command goes to the window's original pane: {calls:?}"
        );
        assert!(
            !has_call(&calls, "kill_window("),
            "no rollback on happy path"
        );
        assert!(
            has_call(&calls, "switch_client_to_window(sess,@1)"),
            "the client lands on the freshly spawned window: {calls:?}"
        );
    }

    #[test]
    fn split_layout_keeps_the_side_by_side_panes() {
        // `@sidebar_spawn_layout split` restores the classic
        // three-pane layout: agent in a left one-third pane, blank shell
        // on the right, sidebar added by the hook.
        let env = FakeEnv {
            spawn_layout: Some("split".into()),
            ..FakeEnv::default()
        };
        spawn_with(&env, &folder_req()).expect("split-layout spawn should succeed");
        let calls = env.calls();
        assert!(
            has_call(&calls, "split_window_left(%1,/r,33)"),
            "agent pane split left of the original shell: {calls:?}"
        );
        assert!(
            has_call(&calls, "send_command(%2,claude)"),
            "agent command goes to the split agent pane: {calls:?}"
        );
    }

    #[test]
    fn a_failed_client_switch_does_not_fail_the_spawn() {
        // The window and agent exist by the time the switch runs; a
        // failure there (e.g. sidebar started detached, no attached
        // client) must only be logged, never rolled back.
        #[derive(Default)]
        struct SwitchFailingEnv(FakeEnv);
        impl SpawnEnv for SwitchFailingEnv {
            fn branch_prefix(&self) -> Option<String> {
                self.0.branch_prefix()
            }
            fn worktree_dir(&self) -> Option<String> {
                self.0.worktree_dir()
            }
            fn spawn_layout(&self) -> Option<String> {
                self.0.spawn_layout()
            }
            fn branch_is_free(&self, r: &str, b: &str) -> bool {
                self.0.branch_is_free(r, b)
            }
            fn branch_exists(&self, r: &str, b: &str) -> bool {
                self.0.branch_exists(r, b)
            }
            fn worktree_path_is_free(&self, p: &Path) -> bool {
                self.0.worktree_path_is_free(p)
            }
            fn worktree_path_exists(&self, p: &str) -> bool {
                self.0.worktree_path_exists(p)
            }
            fn worktree_add(&self, r: &str, p: &str, b: &str) -> Result<(), String> {
                self.0.worktree_add(r, p, b)
            }
            fn worktree_remove(&self, r: &str, p: &str) -> Result<(), String> {
                self.0.worktree_remove(r, p)
            }
            fn branch_delete(&self, r: &str, b: &str) -> Result<(), String> {
                self.0.branch_delete(r, b)
            }
            fn new_window(&self, s: &str, c: &str, n: &str) -> Result<(String, String), String> {
                self.0.new_window(s, c, n)
            }
            fn split_window_left(&self, t: &str, c: &str, p: u32) -> Result<String, String> {
                self.0.split_window_left(t, c, p)
            }
            fn kill_window(&self, w: &str) -> Result<(), String> {
                self.0.kill_window(w)
            }
            fn switch_client_to_window(&self, _s: &str, _w: &str) -> Result<(), String> {
                Err("no current client".into())
            }
            fn set_window_option(&self, w: &str, k: &str, v: &str) -> Result<(), String> {
                self.0.set_window_option(w, k, v)
            }
            fn send_command(&self, t: &str, c: &str) -> Result<(), String> {
                self.0.send_command(t, c)
            }
            fn display_message(&self, p: &str, t: &str) -> String {
                self.0.display_message(p, t)
            }
        }

        let env = SwitchFailingEnv::default();
        let branch = spawn_with(&env, &sample_req()).expect("spawn must still succeed");
        assert_eq!(branch, "agent/task");
        assert!(
            !has_call(&env.0.calls(), "kill_window("),
            "a failed switch must not roll the spawn back"
        );
    }

    #[test]
    fn folder_spawn_rolls_back_the_window_when_a_marker_fails() {
        let env = FakeEnv {
            fail_set_option_at: Some(1),
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &folder_req()).expect_err("marker failure must fail the spawn");
        assert!(
            err.contains(SPAWNED_FROM_OPTION),
            "error mentions marker: {err}"
        );
        let calls = env.calls();
        assert!(
            has_call(&calls, "kill_window(@1)"),
            "rollback kills the new window: {calls:?}"
        );
        assert!(
            !has_call(&calls, "worktree_remove(") && !has_call(&calls, "branch_delete("),
            "folder rollback must not touch git: {calls:?}"
        );
    }

    #[test]
    fn folder_spawn_rolls_back_the_window_when_send_command_fails() {
        let env = FakeEnv {
            fail_send_command: true,
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &folder_req()).expect_err("send failure must fail the spawn");
        assert!(err.contains("send_command failed"), "{err}");
        assert!(
            has_call(&env.calls(), "kill_window(@1)"),
            "rollback kills the new window"
        );
    }

    #[test]
    fn remove_folder_spawn_skips_git_and_kills_the_window() {
        // Folder spawns carry only the spawned + from markers; removal
        // is a pure kill-window regardless of the requested mode.
        let env = FakeEnv {
            display_output: Some("1\n/r\n\n\n@7\n".into()),
            ..FakeEnv::default()
        };
        remove_with(&env, "%1", RemoveMode::WindowAndWorktree)
            .expect("folder spawn removal should succeed");
        let calls = env.calls();
        assert!(
            !has_call(&calls, "worktree_remove(") && !has_call(&calls, "branch_delete("),
            "folder spawn removal must not touch git: {calls:?}"
        );
        assert!(has_call(&calls, "kill_window(@7)"));
    }

    #[test]
    fn spawn_happy_path_sets_all_markers_then_sends_command() {
        let env = FakeEnv::default();
        let branch = spawn_with(&env, &sample_req()).expect("spawn should succeed");
        assert_eq!(branch, "agent/task");
        let calls = env.calls();
        assert!(has_call(
            &calls,
            "worktree_add(/r,/r/.worktrees/task,agent/task)"
        ));
        assert!(has_call(&calls, "new_window(sess,/r/.worktrees/task,task)"));
        assert!(
            !has_call(&calls, "split_window_left("),
            "the default `full` layout gives the agent the whole window: {calls:?}"
        );
        assert_eq!(*env.set_option_calls.borrow(), 4);
        assert!(
            has_call(&calls, "send_command(%1,claude"),
            "agent command goes to the window's original pane: {calls:?}"
        );
        assert!(
            !has_call(&calls, "kill_window("),
            "no rollback on happy path"
        );
        assert!(
            has_call(&calls, "switch_client_to_window(sess,@1)"),
            "the client lands on the freshly spawned window: {calls:?}"
        );
    }

    #[test]
    fn worktree_split_layout_keeps_the_side_by_side_panes() {
        let env = FakeEnv {
            spawn_layout: Some("split".into()),
            ..FakeEnv::default()
        };
        spawn_with(&env, &sample_req()).expect("split-layout spawn should succeed");
        let calls = env.calls();
        assert!(
            has_call(&calls, "split_window_left(%1,/r/.worktrees/task,33)"),
            "agent pane split left of the original shell: {calls:?}"
        );
        assert!(
            has_call(&calls, "send_command(%2,claude"),
            "agent command goes to the split agent pane: {calls:?}"
        );
    }

    #[test]
    fn spawn_degrades_to_single_pane_when_the_split_fails() {
        let env = FakeEnv {
            spawn_layout: Some("split".into()),
            fail_split: true,
            ..FakeEnv::default()
        };
        let branch =
            spawn_with(&env, &sample_req()).expect("split failure must not fail the spawn");
        assert_eq!(branch, "agent/task");
        let calls = env.calls();
        assert!(has_call(&calls, "split_window_left("));
        assert!(
            has_call(&calls, "send_command(%1,claude"),
            "agent takes over the original pane on split failure: {calls:?}"
        );
        assert!(
            !has_call(&calls, "kill_window("),
            "a usable window must not be rolled back: {calls:?}"
        );
    }

    #[test]
    fn spawn_uses_custom_repo_relative_worktree_dir() {
        let env = FakeEnv {
            worktree_dir: Some(".worktrees".into()),
            ..FakeEnv::default()
        };
        spawn_with(&env, &sample_req()).expect("spawn should succeed");
        let calls = env.calls();
        assert!(has_call(
            &calls,
            "worktree_add(/r,/r/.worktrees/task,agent/task)"
        ));
        assert!(has_call(&calls, "new_window(sess,/r/.worktrees/task,task)"));
    }

    #[test]
    fn spawn_rejects_absolute_worktree_dir() {
        let env = FakeEnv {
            worktree_dir: Some("/tmp/worktrees".into()),
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(
            err.contains("worktree directory must be repo-relative"),
            "absolute worktree dir should be rejected before path allocation: {err}"
        );
        assert!(
            !has_call(&env.calls(), "worktree_add("),
            "spawn must not create a worktree with an absolute configured dir"
        );
    }

    #[test]
    fn spawn_rejects_parent_relative_worktree_dir() {
        let env = FakeEnv {
            worktree_dir: Some("../worktrees".into()),
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(
            err.contains("worktree directory must be repo-relative"),
            "parent-relative worktree dir should be rejected before path allocation: {err}"
        );
        assert!(
            !has_call(&env.calls(), "worktree_add("),
            "spawn must not create a worktree outside the repo"
        );
    }

    #[test]
    fn spawn_rolls_back_when_first_marker_fails() {
        let env = FakeEnv {
            fail_set_option_at: Some(0),
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(err.contains(SPAWNED_OPTION), "error mentions marker: {err}");
        let calls = env.calls();
        assert!(
            has_call(&calls, "kill_window(@1)"),
            "kill_window rollback: {calls:?}"
        );
        assert!(
            has_call(&calls, "worktree_remove("),
            "worktree_remove rollback: {calls:?}"
        );
        assert!(
            !has_call(&calls, "send_command("),
            "send_command must not run after marker failure: {calls:?}"
        );
    }

    #[test]
    fn spawn_rolls_back_when_middle_marker_fails() {
        let env = FakeEnv {
            fail_set_option_at: Some(1),
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(
            err.contains(SPAWNED_FROM_OPTION),
            "error mentions second marker: {err}"
        );
        let calls = env.calls();
        assert!(has_call(&calls, "kill_window(@1)"));
        assert!(has_call(&calls, "worktree_remove("));
        assert!(!has_call(&calls, "send_command("));
    }

    #[test]
    fn remove_runs_all_git_steps_before_kill_window() {
        let env = FakeEnv::default();
        remove_with(&env, "%1", RemoveMode::WindowAndWorktree).expect("remove should succeed");
        let calls = env.calls();
        let wt_idx = calls
            .iter()
            .position(|c| c.starts_with("worktree_remove"))
            .expect("worktree_remove called");
        let branch_idx = calls
            .iter()
            .position(|c| c.starts_with("branch_delete"))
            .expect("branch_delete called");
        let kill_idx = calls
            .iter()
            .position(|c| c.starts_with("kill_window"))
            .expect("kill_window called");
        assert!(
            wt_idx < branch_idx && branch_idx < kill_idx,
            "git cleanup must precede kill_window so a git failure \
             leaves the window as a retry handle: {calls:?}"
        );
    }

    #[test]
    fn remove_does_not_kill_window_when_worktree_remove_fails() {
        let env = FakeEnv {
            fail_worktree_remove: true,
            ..FakeEnv::default()
        };
        let err =
            remove_with(&env, "%1", RemoveMode::WindowAndWorktree).expect_err("remove must fail");
        assert!(err.contains("worktree_remove failed"), "error: {err}");
        let calls = env.calls();
        assert!(
            !has_call(&calls, "kill_window("),
            "kill_window must not run when git cleanup fails — the window is the only retry handle: {calls:?}"
        );
    }

    #[test]
    fn remove_skips_git_step_when_worktree_already_gone() {
        let env = FakeEnv {
            worktree_path_already_gone: Some(true),
            fail_worktree_remove: true,
            ..FakeEnv::default()
        };
        remove_with(&env, "%1", RemoveMode::WindowAndWorktree)
            .expect("remove should still succeed via kill when worktree path is gone");
        let calls = env.calls();
        assert!(
            !has_call(&calls, "worktree_remove("),
            "worktree_remove must be skipped when the path is already gone: {calls:?}"
        );
        assert!(has_call(&calls, "kill_window(@1)"));
    }

    #[test]
    fn remove_window_only_skips_worktree_remove() {
        let env = FakeEnv::default();
        remove_with(&env, "%1", RemoveMode::WindowOnly).expect("remove should succeed");
        let calls = env.calls();
        assert!(has_call(&calls, "kill_window(@1)"));
        assert!(
            !has_call(&calls, "worktree_remove("),
            "WindowOnly must not touch worktree: {calls:?}"
        );
    }

    #[test]
    fn spawn_rolls_back_when_send_command_fails() {
        let env = FakeEnv {
            fail_send_command: true,
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(
            err.contains("send_command failed"),
            "primary error surfaced: {err}"
        );
        let calls = env.calls();
        assert!(
            has_call(&calls, "kill_window(@1)"),
            "kill_window rollback on send failure: {calls:?}"
        );
        assert!(
            has_call(&calls, "worktree_remove("),
            "worktree_remove rollback on send failure: {calls:?}"
        );
        assert!(
            has_call(&calls, "branch_delete(/r,agent/task)"),
            "branch_delete rollback on send failure: {calls:?}"
        );
    }

    #[test]
    fn spawn_rolls_back_branch_when_marker_fails() {
        let env = FakeEnv {
            fail_set_option_at: Some(0),
            ..FakeEnv::default()
        };
        spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        let calls = env.calls();
        assert!(
            has_call(&calls, "branch_delete(/r,agent/task)"),
            "rollback must delete the branch `git worktree add -b` created: {calls:?}"
        );
    }

    #[test]
    fn spawn_rolls_back_branch_when_new_window_fails() {
        #[derive(Default)]
        struct NewWindowFailingEnv(FakeEnv);
        impl SpawnEnv for NewWindowFailingEnv {
            fn branch_prefix(&self) -> Option<String> {
                self.0.branch_prefix()
            }
            fn worktree_dir(&self) -> Option<String> {
                self.0.worktree_dir()
            }
            fn spawn_layout(&self) -> Option<String> {
                self.0.spawn_layout()
            }
            fn branch_is_free(&self, r: &str, b: &str) -> bool {
                self.0.branch_is_free(r, b)
            }
            fn branch_exists(&self, r: &str, b: &str) -> bool {
                self.0.branch_exists(r, b)
            }
            fn worktree_path_is_free(&self, p: &Path) -> bool {
                self.0.worktree_path_is_free(p)
            }
            fn worktree_path_exists(&self, p: &str) -> bool {
                self.0.worktree_path_exists(p)
            }
            fn worktree_add(&self, r: &str, p: &str, b: &str) -> Result<(), String> {
                self.0.worktree_add(r, p, b)
            }
            fn worktree_remove(&self, r: &str, p: &str) -> Result<(), String> {
                self.0.worktree_remove(r, p)
            }
            fn branch_delete(&self, r: &str, b: &str) -> Result<(), String> {
                self.0.branch_delete(r, b)
            }
            fn new_window(&self, _s: &str, _c: &str, _n: &str) -> Result<(String, String), String> {
                self.0.log("new_window(fail)".into());
                Err("new_window failed".into())
            }
            fn split_window_left(&self, _t: &str, _c: &str, _p: u32) -> Result<String, String> {
                self.0.split_window_left(_t, _c, _p)
            }
            fn kill_window(&self, w: &str) -> Result<(), String> {
                self.0.kill_window(w)
            }
            fn switch_client_to_window(&self, s: &str, w: &str) -> Result<(), String> {
                self.0.switch_client_to_window(s, w)
            }
            fn set_window_option(&self, w: &str, k: &str, v: &str) -> Result<(), String> {
                self.0.set_window_option(w, k, v)
            }
            fn send_command(&self, t: &str, c: &str) -> Result<(), String> {
                self.0.send_command(t, c)
            }
            fn display_message(&self, p: &str, t: &str) -> String {
                self.0.display_message(p, t)
            }
        }

        let env = NewWindowFailingEnv::default();
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(err.contains("new_window failed"));
        let calls = env.0.calls();
        assert!(
            !has_call(&calls, "kill_window("),
            "no window was ever created: {calls:?}"
        );
        assert!(
            has_call(&calls, "worktree_remove("),
            "worktree must be cleaned up after new_window failure: {calls:?}"
        );
        assert!(
            has_call(&calls, "branch_delete(/r,agent/task)"),
            "branch must be deleted after new_window failure: {calls:?}"
        );
    }

    #[test]
    fn spawn_surfaces_rollback_failure_when_kill_window_also_fails() {
        let env = FakeEnv {
            fail_set_option_at: Some(0),
            fail_kill_window: true,
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(
            err.contains("rollback incomplete"),
            "rollback failure surfaced: {err}"
        );
        assert!(
            err.contains("kill_window"),
            "rollback error names kill_window: {err}"
        );
    }

    #[test]
    fn spawn_surfaces_rollback_failure_when_worktree_remove_also_fails() {
        let env = FakeEnv {
            fail_send_command: true,
            fail_worktree_remove: true,
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(
            err.contains("send_command failed"),
            "primary error surfaced: {err}"
        );
        assert!(
            err.contains("rollback incomplete"),
            "rollback failure surfaced: {err}"
        );
        assert!(
            err.contains("worktree_remove"),
            "rollback error names worktree_remove: {err}"
        );
    }

    #[test]
    fn remove_rejects_pane_missing_spawned_marker() {
        let env = FakeEnv {
            display_output: Some("\n/r\n/r/.worktrees/task\nagent/task\n@1\n".into()),
            ..FakeEnv::default()
        };
        let err =
            remove_with(&env, "%1", RemoveMode::WindowAndWorktree).expect_err("remove must fail");
        assert!(err.contains("not created by sidebar spawn"));
        assert!(!has_call(&env.calls(), "kill_window("));
    }

    #[test]
    fn remove_deletes_branch_between_worktree_and_kill_window() {
        let env = FakeEnv::default();
        remove_with(&env, "%1", RemoveMode::WindowAndWorktree).expect("remove should succeed");
        let calls = env.calls();
        let wt_idx = calls
            .iter()
            .position(|c| c.starts_with("worktree_remove"))
            .expect("worktree_remove called");
        let branch_idx = calls
            .iter()
            .position(|c| c.starts_with("branch_delete"))
            .expect("branch_delete called");
        let kill_idx = calls
            .iter()
            .position(|c| c.starts_with("kill_window"))
            .expect("kill_window called");
        assert!(
            wt_idx < branch_idx && branch_idx < kill_idx,
            "expected worktree → branch → kill order: {calls:?}"
        );
        assert!(has_call(&calls, "branch_delete(/r,agent/task)"));
    }

    #[test]
    fn remove_window_only_does_not_delete_branch() {
        let env = FakeEnv::default();
        remove_with(&env, "%1", RemoveMode::WindowOnly).expect("remove should succeed");
        let calls = env.calls();
        assert!(
            !has_call(&calls, "branch_delete("),
            "WindowOnly must not touch branch: {calls:?}"
        );
    }

    #[test]
    fn remove_rejects_pane_with_empty_branch_marker() {
        let env = FakeEnv {
            display_output: Some("1\n/r\n/r/.worktrees/task\n\n@1\n".into()),
            ..FakeEnv::default()
        };
        let err =
            remove_with(&env, "%1", RemoveMode::WindowAndWorktree).expect_err("remove must fail");
        assert!(err.contains("branch is unset"), "error: {err}");
        assert!(!has_call(&env.calls(), "worktree_remove("));
        assert!(!has_call(&env.calls(), "kill_window("));
    }

    #[test]
    fn remove_does_not_kill_window_when_branch_delete_fails() {
        let env = FakeEnv {
            fail_branch_delete: true,
            ..FakeEnv::default()
        };
        let err =
            remove_with(&env, "%1", RemoveMode::WindowAndWorktree).expect_err("remove must fail");
        assert!(err.contains("branch_delete failed"), "error: {err}");
        let calls = env.calls();
        assert!(
            has_call(&calls, "worktree_remove("),
            "worktree_remove ran first: {calls:?}"
        );
        assert!(
            !has_call(&calls, "kill_window("),
            "kill_window must not run when branch_delete fails — \
             the window is the only retry handle for the orphaned \
             branch: {calls:?}"
        );
    }

    #[test]
    fn remove_skips_branch_delete_when_branch_already_gone() {
        // Simulates a retry after a previous partial success already
        // dropped the branch (e.g. branch_delete succeeded but
        // kill_window then failed on a prior attempt). The flow must
        // converge instead of re-running `git branch -D` and erroring
        // on a missing ref.
        let env = FakeEnv {
            worktree_path_already_gone: Some(true),
            branch_already_gone: Some(true),
            fail_branch_delete: true,
            ..FakeEnv::default()
        };
        remove_with(&env, "%1", RemoveMode::WindowAndWorktree)
            .expect("retry should converge once git state is gone");
        let calls = env.calls();
        assert!(
            !has_call(&calls, "branch_delete("),
            "branch_delete must be skipped when branch is gone: {calls:?}"
        );
        assert!(
            !has_call(&calls, "worktree_remove("),
            "worktree_remove must also stay skipped: {calls:?}"
        );
        assert!(has_call(&calls, "kill_window(@1)"));
    }
}

#[cfg(test)]
mod removal_warning_tests {
    use super::*;

    #[test]
    fn unknown_state_warns_worst_case() {
        // Nonexistent worktree: git status fails, so the dialog must
        // assume the worst instead of claiming the worktree is clean.
        let lines = removal_warnings("/nonexistent/warning/wt");
        assert_eq!(
            lines,
            vec!["Deletes uncommitted", "changes, untracked files"]
        );
    }

    #[test]
    fn unpushed_commits_and_dirty_tree_are_reported() {
        let dir = std::env::temp_dir().join(format!("sidebar_wt_warn_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("git must be available");
            assert!(out.status.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ]);
        // No remotes exist, so the root commit counts as unpushed; the
        // tree is clean, so no dirty warning.
        let lines = removal_warnings(dir.to_str().unwrap());
        assert!(
            lines.contains(&"1 unpushed".to_string())
                && lines.contains(&"commits: reflog only".to_string()),
            "commit with no remotes must be flagged unpushed: {lines:?}"
        );
        assert!(!lines.contains(&"Deletes uncommitted".to_string()));

        // An untracked file flips the dirty warning on.
        std::fs::write(dir.join("untracked.txt"), b"x").unwrap();
        let lines = removal_warnings(dir.to_str().unwrap());
        assert!(
            lines.contains(&"Deletes uncommitted".to_string()),
            "untracked file must trigger the dirty warning: {lines:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
