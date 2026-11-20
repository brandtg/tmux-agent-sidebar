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

/// Resolve the root of the repository's MAIN checkout — the parent of
/// `--git-common-dir` — so spawning from inside a linked worktree anchors
/// the new worktree at the repo root instead of nesting it under the
/// current one (`/repo/.worktrees/feature/.worktrees/<slug>`; audit
/// 2026-11-19 C5). `--show-toplevel` cannot do this: it returns the
/// worktree's own toplevel. Falls back to it when the common dir is
/// unavailable (old git, exotic setups). Mirrors the resolution the TUI
/// popup path uses (group.rs, pinned by the row_collector test).
pub fn main_repo_root(path: &str) -> Option<String> {
    run_git(path, &["rev-parse", "--git-common-dir"])
        .and_then(|common| {
            // `--git-common-dir` is absolute from a linked worktree and
            // relative (`.git`) from the main checkout — same resolution
            // group.rs applies.
            let abs = if std::path::Path::new(&common).is_absolute() {
                std::path::PathBuf::from(common)
            } else {
                std::path::PathBuf::from(path).join(common)
            };
            abs.canonicalize().ok()
        })
        .and_then(|git_dir| {
            git_dir
                .parent()
                .map(|root| root.to_string_lossy().to_string())
        })
        .filter(|root| !root.is_empty())
        .or_else(|| repo_root(path))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Init a real repo with one commit and a linked worktree, so
    /// `main_repo_root` is exercised against actual git output.
    fn repo_with_worktree() -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("sidebar_git_root_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Canonicalize up front: on macOS /var/folders is a symlink to
        // /private/var/folders, and `main_repo_root` returns canonicalized
        // paths — comparing them against the raw temp_dir prefix would
        // fail on the /private difference alone.
        let dir = std::fs::canonicalize(&dir).unwrap();
        let run = |args: &[&str], cwd: &std::path::Path| {
            let out = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("git must be available");
            assert!(out.status.success(), "git {args:?} failed");
        };
        run(&["init", "-q"], &dir);
        run(
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ],
            &dir,
        );
        let wt = dir.join("wt");
        run(&["worktree", "add", "-q", "wt", "-b", "feature"], &dir);
        (dir, wt)
    }

    #[test]
    fn main_repo_root_from_a_worktree_resolves_the_main_checkout() {
        // Audit 2026-11-19 C5: `--show-toplevel` from inside a linked
        // worktree returns the worktree, so a CLI spawn nested the new
        // worktree under it. The common-dir parent must be the repo root.
        let (repo, wt) = repo_with_worktree();

        assert_eq!(
            repo_root(wt.to_str().unwrap()),
            Some(wt.to_string_lossy().to_string()),
            "sanity: show-toplevel returns the worktree (the nesting bug's fuel)"
        );
        assert_eq!(
            main_repo_root(wt.to_str().unwrap()),
            Some(repo.to_string_lossy().to_string())
        );
        assert_eq!(
            main_repo_root(repo.to_str().unwrap()),
            Some(repo.to_string_lossy().to_string()),
            "the main checkout resolves to itself"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }
}
