use std::process::Command;
use std::time::Duration;

use crate::subprocess;

/// Deadline for every git subprocess; a hung git (stale NFS mount, blocked
/// index.lock, credential prompt) must not stall the TUI event loop or the
/// git polling thread beyond this.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn run_git(path: &str, args: &[&str]) -> Option<String> {
    let mut cmd_args = vec!["-C", path];
    cmd_args.extend_from_slice(args);
    let mut command = Command::new("git");
    command.env("GIT_OPTIONAL_LOCKS", "0").args(&cmd_args);
    let output = subprocess::run_with_timeout(&mut command, GIT_TIMEOUT).ok()?;
    if output.status.success() {
        // `trim_end`, not `trim`: leading whitespace can be meaningful in
        // git output (e.g. `status --short` worktree column). Trimming the
        // whole output would corrupt such lines; only trailing whitespace
        // (the final newline) is safe to strip.
        let s = String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string();
        if s.is_empty() { None } else { Some(s) }
    } else {
        None
    }
}

/// Run a git command in `path` and return stderr on non-zero exit. Used by the
/// worktree spawn/remove flow so the UI can show an actionable error message.
pub fn run_git_capture(path: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd_args = vec!["-C", path];
    cmd_args.extend_from_slice(args);
    let mut command = Command::new("git");
    command.env("GIT_OPTIONAL_LOCKS", "0").args(&cmd_args);
    let output = subprocess::run_with_timeout(&mut command, GIT_TIMEOUT)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("git exited with status {}", output.status)
        } else {
            stderr
        })
    }
}

/// Resolve the top-level directory of the git repository containing `path`.
pub fn repo_root(path: &str) -> Option<String> {
    run_git(path, &["rev-parse", "--show-toplevel"])
}

/// `true` when `<repo>/refs/heads/<branch>` exists, i.e. the branch name is
/// already taken.
pub fn branch_exists(repo: &str, branch: &str) -> bool {
    run_git_capture(
        repo,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

/// `git worktree add <worktree_path> -b <branch>` from inside `repo`. Errors
/// bubble up with stderr.
pub fn worktree_add(repo: &str, worktree_path: &str, branch: &str) -> Result<(), String> {
    run_git_capture(repo, &["worktree", "add", worktree_path, "-b", branch]).map(|_| ())
}

/// `git worktree remove --force <worktree_path>`. `--force` is used
/// because the sidebar's remove flow only runs when the user explicitly
/// picks "close window + remove worktree" — agent sessions routinely
/// leave untracked state behind and git would otherwise strand the
/// worktree. Users who want to keep the checkout have `w` (window only).
pub fn worktree_remove(repo: &str, worktree_path: &str) -> Result<(), String> {
    run_git_capture(repo, &["worktree", "remove", "--force", worktree_path]).map(|_| ())
}

/// `git branch -D <branch>`. Used by the spawn rollback path to drop
/// the branch ref that `git worktree add -b` just created — removing
/// only the worktree leaves the branch behind, which later spawns
/// would then collide with via `branch_exists`.
pub fn branch_delete(repo: &str, branch: &str) -> Result<(), String> {
    run_git_capture(repo, &["branch", "-D", branch]).map(|_| ())
}
