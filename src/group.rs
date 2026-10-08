use indexmap::IndexMap;

use crate::git::run_git;
use crate::tmux::{OtherPane, PaneInfo, SessionInfo, WindowStatus};

/// Per-pane git metadata resolved from the pane's working directory.
#[derive(Debug, Clone, Default)]
pub struct PaneGitInfo {
    pub repo_root: Option<String>,
    pub branch: Option<String>,
    pub is_worktree: bool,
    pub worktree_name: Option<String>,
}

/// A group of panes working in the same repository (or directory).
#[derive(Debug, Clone)]
pub struct RepoGroup {
    /// Display name: repo directory basename, or raw path for non-git
    pub name: String,
    /// Whether any pane in the group belongs to the focused (active) window
    pub has_focus: bool,
    /// Panes in this group, with their git info
    pub panes: Vec<(PaneInfo, PaneGitInfo)>,
}

/// A non-agent window attached to a repo group.
#[derive(Debug, Clone)]
pub struct OtherWindow {
    pub window_id: String,
    pub window_index: i64,
    pub window_name: String,
    pub window_active: bool,
    pub session_name: String,
    pub pane_id: String,
    pub pane_active: bool,
    pub pane_pid: Option<u32>,
    pub command: String,
    /// Live cwd of the active pane. The git-info cache is keyed by this
    /// exact path, so it rides along for the worker feed in `app.rs`.
    pub path: String,
    pub git_info: PaneGitInfo,
    pub status: WindowStatus,
}

/// Repo key for a group, mirroring the key `group_panes_by_repo` assigned:
/// the first pane's resolved repo root, else its grouping anchor.
pub fn repo_group_key(group: &RepoGroup) -> String {
    group
        .panes
        .iter()
        .find_map(|(_, git)| git.repo_root.clone())
        .or_else(|| {
            group
                .panes
                .first()
                .map(|(pane, _)| pane.grouping_anchor().to_string())
        })
        .unwrap_or_default()
}

/// Bucket non-agent panes by repo key, one [`OtherWindow`] per pane.
/// Every pane running in a repo window gets its own row so two programs
/// split across panes are tracked independently. Excludes windows that
/// already have an agent pane (their agent row already represents them).
/// Windows in a repo with no agent are still returned; the caller only
/// renders keys that match an existing group.
pub fn group_other_windows_by_repo(
    other_panes: &[OtherPane],
    sessions: &[SessionInfo],
    git_info_cache: &std::collections::HashMap<String, PaneGitInfo>,
) -> IndexMap<String, Vec<OtherWindow>> {
    let agent_windows: std::collections::HashSet<&str> = sessions
        .iter()
        .flat_map(|session| session.windows.iter())
        .map(|window| window.window_id.as_str())
        .collect();

    let mut out: IndexMap<String, Vec<OtherWindow>> = IndexMap::new();
    let mut seen_panes: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for pane in other_panes {
        if agent_windows.contains(pane.window_id.as_str()) {
            continue;
        }
        // Grouped sessions can emit the same pane line twice.
        if !seen_panes.insert((pane.window_id.clone(), pane.pane_id.clone())) {
            continue;
        }
        let git_info = git_info_cache.get(&pane.path).cloned().unwrap_or_default();
        let key = git_info
            .repo_root
            .clone()
            .unwrap_or_else(|| pane.path.clone());
        out.entry(key).or_default().push(OtherWindow {
            window_id: pane.window_id.clone(),
            window_index: pane.window_index,
            window_name: pane.window_name.clone(),
            window_active: pane.window_active,
            session_name: pane.session_name.clone(),
            pane_id: pane.pane_id.clone(),
            pane_active: pane.pane_active,
            pane_pid: pane.pane_pid,
            command: pane.command.clone(),
            path: pane.path.clone(),
            git_info,
            status: crate::tmux::classify_window_status(&pane.command),
        });
    }

    for windows in out.values_mut() {
        windows.sort_by_key(|w| (status_rank(w.status), w.window_index));
    }
    out
}

fn status_rank(status: WindowStatus) -> u8 {
    match status {
        WindowStatus::Task => 0,
        WindowStatus::Busy => 1,
        WindowStatus::Idle => 2,
    }
}

/// Resolve git info for a single pane path.
pub fn resolve_pane_git_info(path: &str) -> PaneGitInfo {
    if path.is_empty() {
        return PaneGitInfo::default();
    }

    // Single git call for all three values (one line per arg)
    let combined = run_git(
        path,
        &[
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
            "--git-common-dir",
            "--git-dir",
        ],
    );
    let (branch, git_common_dir, git_dir) = match combined {
        Some(output) => {
            let mut lines = output.lines();
            let b = lines.next().map(|s| s.to_string());
            let c = lines.next().map(|s| s.to_string());
            let d = lines.next().map(|s| s.to_string());
            (b, c, d)
        }
        None => (None, None, None),
    };

    let is_worktree = match (&git_common_dir, &git_dir) {
        (Some(common), Some(dir)) => {
            let common_path = resolve_git_path(path, common);
            let dir_path = resolve_git_path(path, dir);
            common_path != dir_path
        }
        _ => false,
    };

    // --git-common-dir returns the .git dir of the main worktree;
    // its parent is the repo root, so worktrees share the same group key.
    let repo_root = git_common_dir
        .as_ref()
        .and_then(|common| {
            let abs = resolve_git_path(path, common);
            abs.parent().map(|p| p.to_string_lossy().to_string())
        })
        .or_else(|| run_git(path, &["rev-parse", "--show-toplevel"]));

    PaneGitInfo {
        repo_root,
        branch,
        is_worktree,
        worktree_name: None,
    }
}

/// Group all panes across all sessions by repo root.
/// Returns groups sorted alphabetically by display name (case-insensitive),
/// so the order is stable regardless of which pane is encountered first.
///
/// Git metadata comes from `git_info`, the path→[`PaneGitInfo`] cache owned
/// by the background `git_info_poll_loop`. A path missing from the cache
/// (first tick of a new pane, before the worker has answered) falls back to
/// default info, which groups the pane under its raw directory name until
/// the next tick regroups it by repo root — no git subprocess is ever
/// spawned on the render thread here.
pub fn group_panes_by_repo(
    sessions: &[crate::tmux::SessionInfo],
    git_info_cache: &std::collections::HashMap<String, PaneGitInfo>,
) -> Vec<RepoGroup> {
    let mut groups: IndexMap<String, RepoGroup> = IndexMap::new();

    for session in sessions {
        for window in &session.windows {
            for pane in &window.panes {
                let mut git_info = git_info_cache
                    .get(pane.grouping_anchor())
                    .cloned()
                    .unwrap_or_default();

                // Override with hook-provided worktree info (Claude Code
                // provides this; Codex does not, so the git-command base
                // remains as fallback).
                if !pane.worktree.name.is_empty() {
                    git_info.worktree_name = Some(pane.worktree.name.clone());
                    git_info.is_worktree = true;
                }
                if !pane.worktree.branch.is_empty() {
                    git_info.branch = Some(pane.worktree.branch.clone());
                    git_info.is_worktree = true;
                }

                let group_key = match &git_info.repo_root {
                    Some(root) => root.clone(),
                    None => pane.grouping_anchor().to_string(),
                };

                let display_name = group_key
                    .rsplit('/')
                    .next()
                    .unwrap_or(&group_key)
                    .to_string();

                let has_focus = window.window_active && pane.pane_active;

                let group = groups.entry(group_key).or_insert_with(|| RepoGroup {
                    name: display_name,
                    has_focus: false,
                    panes: Vec::new(),
                });

                if has_focus {
                    group.has_focus = true;
                }

                group.panes.push((pane.clone(), git_info));
            }
        }
    }

    let mut result: Vec<RepoGroup> = groups.into_values().collect();
    result.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    result
}

/// Resolve a possibly-relative git path to an absolute canonical path.
fn resolve_git_path(base: &str, git_path: &str) -> std::path::PathBuf {
    let p = if std::path::Path::new(git_path).is_absolute() {
        std::path::PathBuf::from(git_path)
    } else {
        std::path::PathBuf::from(base).join(git_path)
    };
    p.canonicalize().unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::PaneAttention;
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn resolve_git_info_returns_none_for_empty_path() {
        let info = resolve_pane_git_info("");
        assert!(info.branch.is_none());
        assert!(!info.is_worktree);
        assert!(info.repo_root.is_none());
    }

    #[test]
    fn resolve_git_info_for_real_repo() {
        // Smoke test against this checkout. Must hold in main checkouts
        // and worktrees alike: repo_root intentionally resolves to the
        // shared main-repo root, which differs from CARGO_MANIFEST_DIR
        // when this crate itself is built in a linked worktree. The
        // exact main-vs-worktree expectations live in the hermetic
        // fixture tests below.
        let info = resolve_pane_git_info(env!("CARGO_MANIFEST_DIR"));
        assert!(info.repo_root.is_some(), "should detect git repo");
        assert!(info.branch.is_some(), "should detect branch");
    }

    #[test]
    fn worktree_and_main_share_same_repo_root() {
        // Linked worktrees must resolve to the main repo's root so panes
        // from both checkouts collapse into one group, while the main
        // checkout itself is not flagged as a worktree. Exercised on a
        // hermetic fixture because CARGO_MANIFEST_DIR is only a main
        // checkout in the primary clone — a worktree build would fail
        // the !is_worktree assertion.
        let (_tmp, main, worktree) = temp_repo_with_worktree();

        let main_info = resolve_pane_git_info(main.to_str().unwrap());
        assert!(
            !main_info.is_worktree,
            "main checkout should not be detected as worktree"
        );
        assert!(main_info.branch.is_some());
        let main_root =
            std::fs::canonicalize(main_info.repo_root.expect("main repo root")).unwrap();

        let wt_info = resolve_pane_git_info(worktree.to_str().unwrap());
        assert!(wt_info.is_worktree, "linked worktree must be detected");
        let wt_root =
            std::fs::canonicalize(wt_info.repo_root.expect("worktree repo root")).unwrap();

        assert_eq!(
            wt_root, main_root,
            "worktree and main must share the same repo root"
        );
    }

    /// Build a hermetic git fixture: a main repo with one commit plus a
    /// linked worktree, both inside one temp dir. Returns
    /// `(temp_dir, main_repo_path, worktree_path)`; the temp dir cleans
    /// itself up on drop.
    fn temp_repo_with_worktree() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        fn git_ok(dir: &Path, args: &[&str]) {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .expect("spawn git");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let tmp = tempfile::tempdir().expect("temp dir");
        let main = tmp.path().join("main-repo");
        let worktree = tmp.path().join("linked-wt");

        git_ok(tmp.path(), &["init", "-q", main.to_str().unwrap()]);
        git_ok(
            &main,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "--no-verify",
                "-m",
                "init",
            ],
        );
        git_ok(
            &main,
            &["worktree", "add", "-q", worktree.to_str().unwrap()],
        );

        (tmp, main, worktree)
    }

    // ─── resolve_git_path tests ─────────────────────────────────────

    #[test]
    fn resolve_git_path_absolute() {
        let result = resolve_git_path("/base", "/absolute/path");
        assert_eq!(result, std::path::PathBuf::from("/absolute/path"));
    }

    #[test]
    fn resolve_git_path_relative() {
        let result = resolve_git_path("/base/dir", "relative");
        assert_eq!(result, std::path::PathBuf::from("/base/dir/relative"));
    }

    // ─── group_panes_by_repo tests ──────────────────────────────────

    fn test_pane(id: &str, path: &str) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status: crate::tmux::PaneStatus::Running,
            attention: PaneAttention::None,
            agent: crate::tmux::AgentType::Claude,
            path: path.into(),
            launch_cwd: String::new(),
            current_command: String::new(),
            prompt: String::new(),
            prompt_is_response: false,
            started_at: None,
            wait_reason: String::new(),
            permission_mode: crate::tmux::PermissionMode::Default,
            subagents: vec![],
            pane_pid: None,
            worktree: crate::tmux::WorktreeMetadata::default(),
            session_id: None,
            session_name: String::new(),
            sidebar_spawned: false,
            bg_shell_cmd: None,
        }
    }

    fn test_window(panes: Vec<PaneInfo>, active: bool) -> crate::tmux::WindowInfo {
        crate::tmux::WindowInfo {
            window_id: "@0".into(),
            window_name: "test".into(),
            window_active: active,
            auto_rename: false,
            panes,
        }
    }

    fn test_session(windows: Vec<crate::tmux::WindowInfo>) -> crate::tmux::SessionInfo {
        crate::tmux::SessionInfo {
            session_name: "main".into(),
            windows,
        }
    }

    #[test]
    fn group_panes_empty_sessions() {
        let groups = group_panes_by_repo(&[], &HashMap::new());
        assert!(groups.is_empty());
    }

    /// Build a single hermetic git repo at `<tmp>/<name>` with one commit.
    fn temp_git_repo(tmp: &Path, name: &str) -> std::path::PathBuf {
        let repo = tmp.join(name);
        std::fs::create_dir_all(&repo).expect("mkdir repo");
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "init",
                "-q",
            ])
            .output()
            .expect("spawn git init");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        repo
    }

    /// Build a git-info cache with freshly resolved entries for `paths`,
    /// mirroring what `git_info_poll_loop` would have delivered for them.
    fn cache_with(paths: &[&str]) -> HashMap<String, PaneGitInfo> {
        paths
            .iter()
            .map(|path| (path.to_string(), resolve_pane_git_info(path)))
            .collect()
    }

    #[test]
    fn group_panes_anchor_keeps_launch_repo_when_agent_moves() {
        // Regression for the live-cwd regroup bug: an agent launched in
        // repo A that cd-s into repo B mid-session must stay grouped under
        // A. The anchor (`@pane_launch_cwd`) wins over the live path.
        let tmp = tempfile::tempdir().expect("temp dir");
        let intel_eng = temp_git_repo(tmp.path(), "intel-eng");
        let wisy_cloud = temp_git_repo(tmp.path(), "wisy-cloud");

        let mut pane = test_pane("%1", wisy_cloud.to_str().unwrap());
        pane.launch_cwd = intel_eng.to_str().unwrap().to_string();

        let sessions = vec![test_session(vec![test_window(vec![pane], true)])];
        let groups = group_panes_by_repo(&sessions, &cache_with(&[intel_eng.to_str().unwrap()]));

        assert_eq!(
            groups.len(),
            1,
            "anchored pane must not spawn a second group"
        );
        assert_eq!(
            groups[0].name, "intel-eng",
            "group must stay at the launch repo, not the agent's live cwd"
        );
    }

    #[test]
    fn group_panes_without_anchor_uses_live_path() {
        // No anchor (hooks never fired): grouping behaves exactly as
        // before, keying off the live cwd.
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo_a = temp_git_repo(tmp.path(), "repo-a");
        let repo_b = temp_git_repo(tmp.path(), "repo-b");

        let pane = test_pane("%1", repo_b.to_str().unwrap());

        let sessions = vec![test_session(vec![test_window(vec![pane], true)])];
        let groups = group_panes_by_repo(&sessions, &cache_with(&[repo_b.to_str().unwrap()]));

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "repo-b");
        assert_ne!(repo_a, repo_b, "sanity: fixtures are distinct repos");
    }

    #[test]
    fn group_panes_anchored_and_unanchored_same_repo_merge() {
        // A pane anchored at repo A and a pane still sitting in repo A
        // must collapse into one group.
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo = temp_git_repo(tmp.path(), "shared");

        let mut anchored = test_pane("%1", "/somewhere/else");
        anchored.launch_cwd = repo.to_str().unwrap().to_string();
        let live = test_pane("%2", repo.to_str().unwrap());

        let sessions = vec![test_session(vec![test_window(vec![anchored, live], true)])];
        let groups = group_panes_by_repo(&sessions, &cache_with(&[repo.to_str().unwrap()]));

        assert_eq!(groups.len(), 1, "anchor resolves to the same repo root");
        assert_eq!(groups[0].panes.len(), 2);
    }

    #[test]
    fn group_panes_same_repo() {
        // Two panes in the same real repo should be grouped together
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let pane1 = test_pane("%1", manifest_dir);
        let pane2 = test_pane("%2", manifest_dir);

        let sessions = vec![test_session(vec![test_window(vec![pane1, pane2], true)])];
        let groups = group_panes_by_repo(&sessions, &HashMap::new());

        assert_eq!(groups.len(), 1, "same repo path should produce one group");
        assert_eq!(groups[0].panes.len(), 2);
        assert_eq!(groups[0].panes[0].0.pane_id, "%1");
        assert_eq!(groups[0].panes[1].0.pane_id, "%2");
    }

    #[test]
    fn group_panes_non_git_path_uses_raw_path() {
        // A non-git path should use the raw path as the group key
        let pane = test_pane("%1", "/tmp/no-git-here");

        let sessions = vec![test_session(vec![test_window(vec![pane], true)])];
        let groups = group_panes_by_repo(&sessions, &HashMap::new());

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "no-git-here");
    }

    #[test]
    fn group_panes_display_name_is_basename() {
        // The group key is the repo root, so the display name is the
        // main repo's basename. A hermetic fixture keeps the expected
        // value stable: in a worktree checkout of this crate the
        // manifest-dir basename is not the repo-root basename.
        let (_tmp, main, _worktree) = temp_repo_with_worktree();
        let pane = test_pane("%1", main.to_str().unwrap());

        let mut cache = HashMap::new();
        cache.insert(
            main.to_str().unwrap().to_string(),
            resolve_pane_git_info(main.to_str().unwrap()),
        );
        let sessions = vec![test_session(vec![test_window(vec![pane], true)])];
        let groups = group_panes_by_repo(&sessions, &cache);

        assert_eq!(groups.len(), 1);
        let expected_name = main.file_name().unwrap().to_string_lossy();
        assert_eq!(
            groups[0].name, expected_name,
            "display name should be repo basename"
        );
    }

    #[test]
    fn group_panes_uses_cached_git_info_without_resolving() {
        // The cache is authoritative: a hit is consumed as-is (branch
        // surfaced on the pane), a miss falls back to default info and
        // groups by the raw path instead of spawning git.
        let pane = test_pane("%1", "/tmp/whatever");

        let mut cache = HashMap::new();
        cache.insert(
            "/tmp/whatever".to_string(),
            PaneGitInfo {
                repo_root: Some("/repos/shared".into()),
                branch: Some("cached-branch".into()),
                is_worktree: false,
                worktree_name: None,
            },
        );
        let sessions = vec![test_session(vec![test_window(vec![pane], true)])];
        let groups = group_panes_by_repo(&sessions, &cache);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "shared");
        assert_eq!(groups[0].panes[0].1.branch, Some("cached-branch".into()));
    }

    #[test]
    fn group_panes_has_focus_from_active_window_and_pane() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let mut pane = test_pane("%1", manifest_dir);
        pane.pane_active = true;

        let sessions = vec![test_session(vec![test_window(vec![pane], true)])];
        let groups = group_panes_by_repo(&sessions, &HashMap::new());

        assert!(
            groups[0].has_focus,
            "active pane in active window should set has_focus"
        );
    }

    #[test]
    fn group_panes_no_focus_when_window_inactive() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let mut pane = test_pane("%1", manifest_dir);
        pane.pane_active = true;

        let sessions = vec![test_session(vec![test_window(vec![pane], false)])]; // window_active=false
        let groups = group_panes_by_repo(&sessions, &HashMap::new());

        assert!(
            !groups[0].has_focus,
            "active pane in inactive window should not set has_focus"
        );
    }

    #[test]
    fn group_panes_empty_path_pane() {
        let pane = test_pane("%1", "");

        let sessions = vec![test_session(vec![test_window(vec![pane], true)])];
        let groups = group_panes_by_repo(&sessions, &HashMap::new());

        // Empty path pane should still be grouped (by empty key)
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn group_panes_multiple_sessions() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let pane1 = test_pane("%1", manifest_dir);
        let pane2 = test_pane("%2", "/tmp/other-project");

        let sessions = vec![
            test_session(vec![test_window(vec![pane1], true)]),
            crate::tmux::SessionInfo {
                session_name: "other".into(),
                windows: vec![test_window(vec![pane2], false)],
            },
        ];
        let groups = group_panes_by_repo(&sessions, &HashMap::new());

        assert_eq!(
            groups.len(),
            2,
            "different repos across sessions should produce separate groups"
        );
    }

    #[test]
    fn group_panes_same_repo_across_sessions_merge_into_one_group() {
        // Regression for the `state.sessions` field removal: panes that
        // live in different tmux sessions but share the same repo path
        // must still collapse into a single `RepoGroup`. This is what
        // makes the sidebar usable across multi-session workflows.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let pane_session_a = test_pane("%1", manifest_dir);
        let pane_session_b = test_pane("%2", manifest_dir);

        let sessions = vec![
            crate::tmux::SessionInfo {
                session_name: "alpha".into(),
                windows: vec![test_window(vec![pane_session_a], true)],
            },
            crate::tmux::SessionInfo {
                session_name: "beta".into(),
                windows: vec![test_window(vec![pane_session_b], false)],
            },
        ];
        let groups = group_panes_by_repo(&sessions, &HashMap::new());

        assert_eq!(
            groups.len(),
            1,
            "panes in the same repo across sessions must merge into one group"
        );
        assert_eq!(groups[0].panes.len(), 2);
        let pane_ids: Vec<&str> = groups[0]
            .panes
            .iter()
            .map(|(p, _)| p.pane_id.as_str())
            .collect();
        assert!(pane_ids.contains(&"%1"));
        assert!(pane_ids.contains(&"%2"));
    }

    #[test]
    fn group_panes_sorted_by_name_case_insensitive() {
        // Groups should be sorted alphabetically regardless of encounter order
        let pane1 = test_pane("%1", "/tmp/zzz");
        let pane2 = test_pane("%2", "/tmp/Aaa");
        let pane3 = test_pane("%3", "/tmp/mmm");
        let pane4 = test_pane("%4", "/tmp/zzz");

        let sessions = vec![test_session(vec![test_window(
            vec![pane1, pane2, pane3, pane4],
            true,
        )])];
        let groups = group_panes_by_repo(&sessions, &HashMap::new());

        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].name, "Aaa");
        assert_eq!(groups[1].name, "mmm");
        assert_eq!(groups[2].name, "zzz");
        assert_eq!(groups[2].panes.len(), 2, "zzz should have 2 panes");
    }

    // ─── group_other_windows_by_repo ────────────────────────────────

    fn other_pane(
        window_id: &str,
        pane_id: &str,
        path: &str,
        command: &str,
        pane_active: bool,
    ) -> OtherPane {
        OtherPane {
            session_name: "main".into(),
            window_id: window_id.into(),
            window_index: 0,
            window_name: "win".into(),
            window_active: false,
            pane_id: pane_id.into(),
            pane_active,
            path: path.into(),
            command: command.into(),
            pane_pid: None,
            last_cmd: None,
            last_exit: None,
        }
    }

    fn cache_with_repo(path: &str, root: &str) -> HashMap<String, PaneGitInfo> {
        let mut cache = HashMap::new();
        cache.insert(
            path.to_string(),
            PaneGitInfo {
                repo_root: Some(root.into()),
                ..Default::default()
            },
        );
        cache
    }

    fn agent_window(window_id: &str, pane_id: &str, path: &str) -> crate::tmux::SessionInfo {
        crate::tmux::SessionInfo {
            session_name: "main".into(),
            windows: vec![crate::tmux::WindowInfo {
                window_id: window_id.into(),
                window_name: "agent".into(),
                window_active: true,
                auto_rename: false,
                panes: vec![test_pane(pane_id, path)],
            }],
        }
    }

    #[test]
    fn group_other_windows_buckets_by_repo_and_skips_agent_windows() {
        let shell = other_pane("@5", "%5", "/repo", "zsh", true);
        let agent_sibling = other_pane("@6", "%6", "/repo", "zsh", true);
        let cache = cache_with_repo("/repo", "/repo");
        // @6 already hosts an agent pane, so it is not an "other" window.
        let sessions = vec![agent_window("@6", "%9", "/repo")];

        let grouped = group_other_windows_by_repo(&[shell, agent_sibling], &sessions, &cache);

        let windows = grouped.get("/repo").expect("repo bucket");
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].window_id, "@5");
    }

    #[test]
    fn group_other_windows_emits_one_row_per_pane() {
        // Every pane of a non-agent window gets its own row, so two
        // programs running in two splits are tracked independently.
        let inactive = other_pane("@5", "%5", "/repo", "zsh", false);
        let active = other_pane("@5", "%6", "/repo", "cargo", true);
        let cache = cache_with_repo("/repo", "/repo");

        let grouped = group_other_windows_by_repo(&[inactive, active], &[], &cache);

        let windows = grouped.get("/repo").expect("repo bucket");
        assert_eq!(
            windows.len(),
            2,
            "one row per pane, not one representative per window"
        );
        let active_row = windows
            .iter()
            .find(|w| w.pane_id == "%6")
            .expect("active pane row");
        assert!(active_row.pane_active);
        assert_eq!(active_row.command, "cargo");
        assert_eq!(active_row.status, WindowStatus::Task);
        assert!(windows.iter().any(|w| w.pane_id == "%5"));
    }

    #[test]
    fn group_other_windows_dedups_grouped_session_lines() {
        // Grouped sessions can emit the same pane line twice; the pane id
        // must be deduped so one pane never renders two rows.
        let pane = other_pane("@5", "%5", "/repo", "zsh", true);
        let cache = cache_with_repo("/repo", "/repo");

        let grouped = group_other_windows_by_repo(&[pane.clone(), pane], &[], &cache);

        assert_eq!(grouped.get("/repo").expect("repo bucket").len(), 1);
    }

    #[test]
    fn group_other_windows_sorts_task_before_idle() {
        let idle = other_pane("@1", "%1", "/repo", "zsh", true);
        let task = other_pane("@2", "%2", "/repo", "cargo", true);
        let cache = cache_with_repo("/repo", "/repo");

        let grouped = group_other_windows_by_repo(&[idle, task], &[], &cache);

        let windows = grouped.get("/repo").expect("repo bucket");
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].status, WindowStatus::Task);
        assert_eq!(windows[1].status, WindowStatus::Idle);
    }

    #[test]
    fn group_other_windows_non_git_path_keys_by_raw_path() {
        let shell = other_pane("@5", "%5", "/tmp/no-git", "zsh", true);

        let grouped = group_other_windows_by_repo(&[shell], &[], &HashMap::new());

        assert!(grouped.contains_key("/tmp/no-git"));
    }

    #[test]
    fn repo_group_key_prefers_repo_root_then_anchor() {
        let mut cache = HashMap::new();
        cache.insert(
            "/work/dir".to_string(),
            PaneGitInfo {
                repo_root: Some("/repos/shared".into()),
                ..Default::default()
            },
        );
        let sessions = vec![test_session(vec![test_window(
            vec![test_pane("%1", "/work/dir")],
            true,
        )])];
        let groups = group_panes_by_repo(&sessions, &cache);
        assert_eq!(repo_group_key(&groups[0]), "/repos/shared");

        let sessions = vec![test_session(vec![test_window(
            vec![test_pane("%1", "/tmp/plain")],
            true,
        )])];
        let groups = group_panes_by_repo(&sessions, &HashMap::new());
        assert_eq!(repo_group_key(&groups[0]), "/tmp/plain");
    }
}
