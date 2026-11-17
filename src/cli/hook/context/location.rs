use std::path::Path;

use super::super::activity::lock_exclusive;
use crate::event::WorktreeInfo;
use crate::time::now_epoch_secs;
use crate::tmux;

/// Returns whether the pane's cwd should be updated.
/// When subagents are active, events may come from a subagent running in a
/// worktree, so we should NOT overwrite the parent agent's cwd.
pub(in crate::cli::hook) fn should_update_cwd(current_subagents: &str) -> bool {
    current_subagents.is_empty()
}

/// Resolve the effective cwd for pane metadata.
/// When a worktree is active, prefer `original_repo_dir` so the sidebar
/// groups the pane under the original repository, not the worktree path.
pub(in crate::cli::hook) fn resolve_cwd<'a>(
    raw_cwd: &'a str,
    worktree: &'a Option<WorktreeInfo>,
) -> &'a str {
    if let Some(wt) = worktree
        && !wt.original_repo_dir.is_empty()
    {
        return &wt.original_repo_dir;
    }
    raw_cwd
}

/// Collect the worktree name/branch writes for `worktree` into `writes`
/// (clears both options when worktree is `None`; when worktree is `Some`
/// but an individual field is empty, clears that field so an explicit
/// "no longer in a worktree" payload cannot leave stale metadata from a
/// previous run behind).
fn push_worktree_writes<'a>(
    worktree: &'a Option<WorktreeInfo>,
    writes: &mut Vec<tmux::PaneOptionWrite<'a>>,
) {
    if let Some(wt) = worktree {
        if wt.name.is_empty() {
            writes.push(tmux::PaneOptionWrite::Unset {
                key: tmux::PANE_WORKTREE_NAME,
            });
        } else {
            writes.push(tmux::PaneOptionWrite::Set {
                key: tmux::PANE_WORKTREE_NAME,
                value: &wt.name,
            });
        }
        if wt.branch.is_empty() {
            writes.push(tmux::PaneOptionWrite::Unset {
                key: tmux::PANE_WORKTREE_BRANCH,
            });
        } else {
            writes.push(tmux::PaneOptionWrite::Set {
                key: tmux::PANE_WORKTREE_BRANCH,
                value: &wt.branch,
            });
        }
    } else {
        writes.push(tmux::PaneOptionWrite::Unset {
            key: tmux::PANE_WORKTREE_NAME,
        });
        writes.push(tmux::PaneOptionWrite::Unset {
            key: tmux::PANE_WORKTREE_BRANCH,
        });
    }
}

/// The `@pane_launch_cwd` value this event should store, or `None` when
/// the existing anchor must be kept (or there is no cwd to anchor).
///
/// The anchor is the cwd resolved at the pane's first observed hook event
/// and is what repo grouping keys off, so an agent cd-ing into another
/// checkout mid-session never moves its group. `overwrite` is only true
/// for a fresh `startup` SessionStart — the one event that authoritatively
/// re-launches an agent in the pane and can therefore heal a stale anchor
/// left behind by a hard-killed previous run. Every other event seeds the
/// anchor only when unset.
fn launch_anchor_value(
    pane: &str,
    cwd: &str,
    worktree: &Option<WorktreeInfo>,
    overwrite: bool,
) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    if !overwrite && !tmux::get_pane_option_value(pane, tmux::PANE_LAUNCH_CWD).is_empty() {
        return None;
    }
    Some(resolve_cwd(cwd, worktree).to_string())
}

/// Seed or overwrite the launch anchor (`@pane_launch_cwd`). See
/// [`launch_anchor_value`] for the anchoring rules. SessionStart is the
/// only caller outside [`sync_pane_location`]'s batched write.
pub(in crate::cli::hook) fn sync_launch_anchor(
    pane: &str,
    cwd: &str,
    worktree: &Option<WorktreeInfo>,
    overwrite: bool,
) {
    if let Some(anchor) = launch_anchor_value(pane, cwd, worktree, overwrite) {
        tmux::set_pane_option(pane, tmux::PANE_LAUNCH_CWD, &anchor);
    }
}

/// Whether this hook event may record `@pane_session_id` on the pane.
///
/// The OpenCode bridge resolves the target pane per event and commits the
/// resolution unless it had to guess (every candidate pane bound, none
/// provably stale). A guessed pane can belong to another live session, and
/// recording the binding there would permanently misroute that session's
/// later events — so the bridge flags guesses with
/// `TMUX_AGENT_SIDEBAR_BIND_SESSION=0` and the binding write is skipped.
/// Every other agent and in-pane setups never set the variable and keep
/// the unconditional write.
pub(in crate::cli::hook) fn session_bind_allowed() -> bool {
    match std::env::var("TMUX_AGENT_SIDEBAR_BIND_SESSION") {
        Ok(value) => value != "0",
        Err(_) => true,
    }
}

pub(in crate::cli::hook) fn sync_pane_location(
    pane: &str,
    cwd: &str,
    worktree: &Option<WorktreeInfo>,
    session_id: &Option<String>,
) {
    // Subagents share the parent's $TMUX_PANE and can fire their own hook
    // events with a different session_id, cwd, or worktree. While children
    // are active, every pane-scoped write must be skipped so the parent's
    // identity is preserved — including `@pane_worktree_*`, which used to
    // leak through and misgroup the pane under the child's repo.
    let current_subagents = tmux::get_pane_option_value(pane, tmux::PANE_SUBAGENTS);
    if !should_update_cwd(&current_subagents) {
        return;
    }
    // Reads first, then a single batched write: each of these options is
    // otherwise its own tmux spawn, paid on every identity-bearing event
    // (audit 2026-11-16 #5).
    let anchor = launch_anchor_value(pane, cwd, worktree, false);
    //
    // A bridge-guessed pane must not have its session identity rewritten:
    // the binding is what later events route by (see session_bind_allowed).
    // The read-modify-write of `@pane_session_id` / `@pane_session_title`
    // holds the same per-pane flock as `on_session_title`'s check+write,
    // so the two identity writers on one pane cannot interleave their
    // check and write across independent hook processes (audit
    // 2026-11-16: unguarded session-title get-check-set).
    let mut writes: Vec<tmux::PaneOptionWrite> = Vec::new();
    crate::cli::hook::handlers::with_subagent_lock(pane, || {
        if !session_bind_allowed() {
            return;
        }
        match session_id.as_deref() {
            Some(sid) if !sid.is_empty() => {
                // Pane switched sessions: drop the previous session's title so
                // it cannot outlive the session that set it. The replacement
                // arrives via a later session-title event.
                let prev = tmux::get_pane_option_value(pane, tmux::PANE_SESSION_ID);
                if !prev.is_empty() && prev != sid {
                    writes.push(tmux::PaneOptionWrite::Unset {
                        key: tmux::PANE_SESSION_TITLE,
                    });
                }
                writes.push(tmux::PaneOptionWrite::Set {
                    key: tmux::PANE_SESSION_ID,
                    value: sid,
                });
            }
            _ => {
                writes.push(tmux::PaneOptionWrite::Unset {
                    key: tmux::PANE_SESSION_ID,
                });
                writes.push(tmux::PaneOptionWrite::Unset {
                    key: tmux::PANE_SESSION_TITLE,
                });
            }
        }
    });
    if !cwd.is_empty() {
        let effective_cwd = resolve_cwd(cwd, worktree);
        writes.push(tmux::PaneOptionWrite::Set {
            key: tmux::PANE_CWD,
            value: effective_cwd,
        });
        // Seed the launch anchor if the pane doesn't have one yet. Like
        // @pane_cwd this is parent-owned: the subagent guard above already
        // returned when children are active.
        if let Some(anchor) = &anchor {
            writes.push(tmux::PaneOptionWrite::Set {
                key: tmux::PANE_LAUNCH_CWD,
                value: anchor,
            });
        }
    }
    let live_worktree = with_live_worktree(pane, worktree);
    push_worktree_writes(&live_worktree, &mut writes);
    tmux::apply_pane_options(pane, &writes);
}

/// Fill in a worktree name and branch the payload left out. Claude sends
/// neither when a session enters an existing worktree by path, but the
/// agent process itself moves there, so the pane's live directory says
/// which linked worktree (if any) it is working in.
///
/// The probe is a git subprocess per event; its verdict cannot change
/// while the pane's cwd is unchanged, so it is cached per pane+path in a
/// runtime-dir file (audit 2026-11-16 #5).
fn with_live_worktree(pane: &str, worktree: &Option<WorktreeInfo>) -> Option<WorktreeInfo> {
    if let Some(wt) = worktree
        && (!wt.name.is_empty() || !wt.branch.is_empty())
    {
        return worktree.clone();
    }
    let path = match worktree {
        Some(wt) if !wt.path.is_empty() => wt.path.clone(),
        _ => tmux::display_message(pane, "#{pane_current_path}"),
    };
    let cache = worktree_cache_path();
    if let Some(cached) = cached_probe(&cache, pane, &path) {
        return finish_live_worktree(cached.probe, worktree);
    }
    let probed = linked_worktree_at(&path);
    store_probe(&cache, pane, &path, &probed);
    finish_live_worktree(probed, worktree)
}

fn finish_live_worktree(
    probe: Option<(String, String)>,
    worktree: &Option<WorktreeInfo>,
) -> Option<WorktreeInfo> {
    match probe {
        Some((root, branch)) => Some(WorktreeInfo {
            name: root.rsplit('/').next().unwrap_or_default().to_string(),
            path: root,
            branch,
            original_repo_dir: worktree
                .as_ref()
                .map(|wt| wt.original_repo_dir.clone())
                .unwrap_or_default(),
        }),
        None => worktree.clone(),
    }
}

/// Root and branch of the linked worktree containing `path`, or `None`
/// for a main checkout, a non-repo directory, or a detached HEAD.
fn linked_worktree_at(path: &str) -> Option<(String, String)> {
    if path.is_empty() {
        return None;
    }
    let out = crate::git::run_git(
        path,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
            "--show-toplevel",
            "--abbrev-ref",
            "HEAD",
        ],
    )?;
    let lines: Vec<&str> = out.lines().collect();
    let [git_dir, common_dir, root, branch] = lines.as_slice() else {
        return None;
    };
    if git_dir == common_dir || *branch == "HEAD" {
        return None;
    }
    Some((root.to_string(), branch.to_string()))
}

/// How long a cached worktree probe stays trustworthy. The verdict is a
/// pure function of the directory (repo layout), but the branch can be
/// changed by a `git checkout` under an unchanged cwd; the TTL bounds how
/// long a row can show a stale branch after such a checkout.
const WORKTREE_CACHE_TTL_SECS: u64 = 60;

/// A cached worktree probe: the path it was probed at and the
/// [`linked_worktree_at`] verdict (`None` = no linked worktree there).
struct CachedProbe {
    probe: Option<(String, String)>,
}

/// Runtime-dir scratch file holding per-pane worktree-probe verdicts.
/// Hook processes are fresh per event, so the cache must live on disk to
/// outlive a single spawn (audit 2026-11-16 #5).
fn worktree_cache_path() -> std::path::PathBuf {
    crate::paths::runtime_dir().join("tmux-agent-worktree-cache.tsv")
}

/// The cached probe verdict for `pane` at `path`, when the pane's cwd is
/// unchanged since the probe and the entry is fresh.
fn cached_probe(cache: &Path, pane: &str, path: &str) -> Option<CachedProbe> {
    if path.is_empty() {
        return None;
    }
    let mut file = crate::paths::open_read_write_private(cache).ok()?;
    if !lock_exclusive(&file) {
        return None;
    }
    let rows = read_probe_cache(&mut file);
    let now = now_epoch_secs();
    let (_, _, root, branch, _) = rows.into_iter().find(|(row_pane, row_path, _, _, at)| {
        row_pane == pane && row_path == path && now.saturating_sub(*at) < WORKTREE_CACHE_TTL_SECS
    })?;
    let probe = if root.is_empty() && branch.is_empty() {
        None
    } else {
        Some((root, branch))
    };
    Some(CachedProbe { probe })
}

/// Record the probe verdict for `pane` at `path`, dropping expired rows
/// while the file is held.
fn store_probe(cache: &Path, pane: &str, path: &str, probe: &Option<(String, String)>) {
    if path.is_empty() {
        return;
    }
    let Ok(mut file) = crate::paths::open_read_write_private(cache) else {
        return;
    };
    if !lock_exclusive(&file) {
        return;
    }
    let now = now_epoch_secs();
    let (root, branch) = match probe {
        Some((root, branch)) => (root.as_str(), branch.as_str()),
        None => ("", ""),
    };
    let mut rows: Vec<(String, String, String, String, u64)> = read_probe_cache(&mut file)
        .into_iter()
        .filter(|(_, _, _, _, at)| now.saturating_sub(*at) < WORKTREE_CACHE_TTL_SECS)
        .filter(|(row_pane, ..)| row_pane != pane)
        .collect();
    rows.push((
        pane.to_string(),
        path.to_string(),
        root.to_string(),
        branch.to_string(),
        now,
    ));
    write_probe_cache(&mut file, &rows);
}

/// Tab-separated rows of `pane, path, root, branch, cached_at`; malformed
/// lines are skipped so a corrupted file degrades to a cache miss.
fn read_probe_cache(file: &mut std::fs::File) -> Vec<(String, String, String, String, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let _ = file.seek(SeekFrom::Start(0));
    let mut content = String::new();
    if file.read_to_string(&mut content).is_err() {
        return Vec::new();
    }
    content
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != 5 {
                return None;
            }
            Some((
                fields[0].to_string(),
                fields[1].to_string(),
                fields[2].to_string(),
                fields[3].to_string(),
                fields[4].parse().ok()?,
            ))
        })
        .collect()
}

fn write_probe_cache(file: &mut std::fs::File, rows: &[(String, String, String, String, u64)]) {
    use std::io::{Seek, SeekFrom, Write};
    let mut body = String::new();
    for (pane, path, root, branch, at) in rows {
        body.push_str(pane);
        body.push('\t');
        body.push_str(path);
        body.push('\t');
        body.push_str(root);
        body.push('\t');
        body.push_str(branch);
        body.push('\t');
        body.push_str(&at.to_string());
        body.push('\n');
    }
    let _ = file.seek(SeekFrom::Start(0));
    if file.write_all(body.as_bytes()).is_ok() {
        let _ = file.set_len(body.len() as u64);
    }
}

/// Returns true if pane-scoped writes from this hook event are safe to
/// apply to the pane's metadata. False while subagents are active so a
/// child hook cannot clobber the parent pane's identity.
pub(in crate::cli::hook) fn pane_writes_allowed(pane: &str) -> bool {
    let current_subagents = tmux::get_pane_option_value(pane, tmux::PANE_SUBAGENTS);
    should_update_cwd(&current_subagents)
}

#[cfg(test)]
mod tests {
    use super::*;

    // std::env is process-global and cargo runs tests in parallel threads;
    // every test that touches TMUX_AGENT_SIDEBAR_BIND_SESSION holds this so
    // concurrent sync_pane_location tests cannot observe the flag.
    static BIND_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn git(dir: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn linked_worktree_at_reports_linked_worktrees_only() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).expect("mkdir main");
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let linked = tmp.path().join("feat-wt");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat/x",
                linked.to_str().unwrap(),
            ],
        );

        assert_eq!(linked_worktree_at(main.to_str().unwrap()), None);
        let (root, branch) = linked_worktree_at(linked.to_str().unwrap()).expect("linked worktree");
        assert!(root.ends_with("/feat-wt"), "unexpected root {root}");
        assert_eq!(branch, "feat/x");
        assert_eq!(linked_worktree_at(tmp.path().to_str().unwrap()), None);
    }

    #[test]
    fn worktree_probe_cache_hits_only_while_pane_path_and_ttl_hold() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let cache = tmp.path().join("wt-cache.tsv");
        let pane = "%WT_CACHE";
        let path_a = "/repo/a";
        let path_b = "/repo/b";

        store_probe(
            &cache,
            pane,
            path_a,
            &Some(("/wt/feat".to_string(), "feat/x".to_string())),
        );
        // Same pane, same path: hit.
        let hit = cached_probe(&cache, pane, path_a).expect("fresh entry");
        assert_eq!(
            hit.probe,
            Some(("/wt/feat".to_string(), "feat/x".to_string()))
        );
        // The pane moved: miss.
        assert!(cached_probe(&cache, pane, path_b).is_none());
        // A different pane: miss.
        assert!(cached_probe(&cache, "%WT_CACHE_OTHER", path_a).is_none());

        // An entry past the TTL is a miss, and a fresh store drops it.
        let expired = vec![(
            pane.to_string(),
            path_a.to_string(),
            "/wt/old".to_string(),
            "old".to_string(),
            now_epoch_secs() - WORKTREE_CACHE_TTL_SECS - 1,
        )];
        let mut file = crate::paths::open_read_write_private(&cache).unwrap();
        write_probe_cache(&mut file, &expired);
        drop(file);
        assert!(
            cached_probe(&cache, pane, path_a).is_none(),
            "expired verdicts must re-probe"
        );
        store_probe(
            &cache,
            pane,
            path_a,
            &Some(("/wt/new".to_string(), "new".to_string())),
        );
        let hit = cached_probe(&cache, pane, path_a).expect("re-stored entry");
        assert_eq!(hit.probe, Some(("/wt/new".to_string(), "new".to_string())));
    }

    #[test]
    fn worktree_probe_cache_stores_none_verdicts() {
        // A "no linked worktree here" verdict is the common case for main
        // checkouts — it must cache too, or the git spawn is paid forever.
        let tmp = tempfile::tempdir().expect("temp dir");
        let cache = tmp.path().join("wt-cache-none.tsv");
        let pane = "%WT_CACHE_NONE";
        store_probe(&cache, pane, "/main/repo", &None);
        let hit = cached_probe(&cache, pane, "/main/repo").expect("cached none verdict");
        assert_eq!(hit.probe, None);
        assert!(cached_probe(&cache, pane, "/other/repo").is_none());
    }

    #[test]
    fn with_live_worktree_serves_a_cached_verdict_without_probing() {
        // Prime the runtime cache with a verdict for a NON-repo directory:
        // if the cache were ignored, git would report None here.
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().to_string_lossy().to_string();
        let pane = "%WT_CACHE_E2E";
        store_probe(
            &worktree_cache_path(),
            pane,
            &dir,
            &Some((format!("{dir}/feat-wt"), "feat/x".to_string())),
        );
        let wt = with_live_worktree(
            pane,
            &Some(WorktreeInfo {
                name: String::new(),
                path: dir.clone(),
                branch: String::new(),
                original_repo_dir: String::new(),
            }),
        )
        .expect("cached verdict served");
        assert!(wt.path.ends_with("/feat-wt"), "unexpected root {}", wt.path);
        assert_eq!(wt.branch, "feat/x");
        assert_eq!(wt.name, "feat-wt");
    }

    #[test]
    fn resolve_cwd_prefers_worktree_original_repo_dir() {
        let wt = WorktreeInfo {
            name: "feat".into(),
            path: "/tmp/wt".into(),
            branch: "feat".into(),
            original_repo_dir: "/home/user/repo".into(),
        };
        assert_eq!(resolve_cwd("/tmp/wt/src", &Some(wt)), "/home/user/repo");
    }

    #[test]
    fn resolve_cwd_falls_back_to_raw_cwd() {
        assert_eq!(resolve_cwd("/tmp/project", &None), "/tmp/project");
    }

    #[test]
    fn resolve_cwd_worktree_empty_original_falls_back() {
        let wt = WorktreeInfo {
            name: "feat".into(),
            path: "/tmp/wt".into(),
            branch: "feat".into(),
            original_repo_dir: "".into(),
        };
        assert_eq!(resolve_cwd("/tmp/wt/src", &Some(wt)), "/tmp/wt/src");
    }

    #[test]
    fn should_update_cwd_when_no_subagents() {
        assert!(should_update_cwd(""));
    }

    #[test]
    fn should_not_update_cwd_when_subagent_active() {
        assert!(!should_update_cwd("Explore:sub-1"));
    }

    #[test]
    fn should_not_update_cwd_when_multiple_subagents_active() {
        assert!(!should_update_cwd("Explore:sub-1,Plan:sub-2"));
    }

    #[test]
    fn sync_pane_location_skips_worktree_writes_while_subagents_active() {
        let _guard = tmux::test_mock::install();
        let _env = BIND_ENV_LOCK.lock().unwrap();
        let pane = "%PARENT";
        // Parent state: real worktree owned by the parent agent.
        tmux::test_mock::set(pane, tmux::PANE_SUBAGENTS, "Explore:sub-1");
        tmux::test_mock::set(pane, tmux::PANE_WORKTREE_NAME, "parent-feat");
        tmux::test_mock::set(pane, tmux::PANE_WORKTREE_BRANCH, "feat/parent");
        tmux::test_mock::set(pane, tmux::PANE_CWD, "/repo/parent");
        tmux::test_mock::set(pane, tmux::PANE_SESSION_ID, "parent-session");

        // Subagent fires a hook with its own (different) worktree.
        let child_wt = Some(WorktreeInfo {
            name: "child-feat".into(),
            path: "/wt/child".into(),
            branch: "feat/child".into(),
            original_repo_dir: "/repo/child".into(),
        });
        sync_pane_location(
            pane,
            "/repo/child",
            &child_wt,
            &Some("child-session".into()),
        );

        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_WORKTREE_NAME).as_deref(),
            Some("parent-feat"),
            "worktree name must not leak from subagent into parent"
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_WORKTREE_BRANCH).as_deref(),
            Some("feat/parent")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_CWD).as_deref(),
            Some("/repo/parent")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_SESSION_ID).as_deref(),
            Some("parent-session")
        );
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_LAUNCH_CWD),
            "subagent-guarded events must not seed the parent's launch anchor"
        );
    }

    #[test]
    fn sync_pane_location_writes_worktree_when_no_subagents() {
        let _guard = tmux::test_mock::install();
        let _env = BIND_ENV_LOCK.lock().unwrap();
        let pane = "%LONE";
        let wt = Some(WorktreeInfo {
            name: "feat-x".into(),
            path: "/wt/feat-x".into(),
            branch: "feat-x".into(),
            original_repo_dir: "/repo".into(),
        });

        sync_pane_location(pane, "/wt/feat-x", &wt, &Some("sess-1".into()));

        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_WORKTREE_NAME).as_deref(),
            Some("feat-x")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_WORKTREE_BRANCH).as_deref(),
            Some("feat-x")
        );
        // resolve_cwd routes the original_repo_dir into @pane_cwd.
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_CWD).as_deref(),
            Some("/repo")
        );
        // The same resolution seeds the launch anchor on first sight.
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_LAUNCH_CWD).as_deref(),
            Some("/repo")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_SESSION_ID).as_deref(),
            Some("sess-1")
        );
    }

    #[test]
    fn sync_launch_anchor_seeds_when_unset() {
        let _guard = tmux::test_mock::install();
        let pane = "%ANCHOR_SEED";
        sync_launch_anchor(pane, "/first/dir", &None, false);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_LAUNCH_CWD).as_deref(),
            Some("/first/dir")
        );
    }

    #[test]
    fn sync_launch_anchor_keeps_existing_unless_overwrite() {
        let _guard = tmux::test_mock::install();
        let pane = "%ANCHOR_STICKY";
        tmux::test_mock::set(pane, tmux::PANE_LAUNCH_CWD, "/launch/repo");

        // A mid-session cd must not move the anchor...
        sync_launch_anchor(pane, "/other/repo", &None, false);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_LAUNCH_CWD).as_deref(),
            Some("/launch/repo"),
            "non-overwrite events must keep the existing anchor"
        );

        // ...but a fresh startup launch is authoritative.
        sync_launch_anchor(pane, "/other/repo", &None, true);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_LAUNCH_CWD).as_deref(),
            Some("/other/repo"),
            "overwrite must replace a stale anchor"
        );
    }

    #[test]
    fn sync_launch_anchor_skips_empty_cwd() {
        let _guard = tmux::test_mock::install();
        let pane = "%ANCHOR_EMPTY";
        sync_launch_anchor(pane, "", &None, true);
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_LAUNCH_CWD),
            "empty cwd must not write an anchor"
        );
    }

    #[test]
    fn sync_launch_anchor_resolves_worktree_original_repo_dir() {
        let _guard = tmux::test_mock::install();
        let pane = "%ANCHOR_WT";
        let wt = Some(WorktreeInfo {
            name: "feat".into(),
            path: "/wt/feat".into(),
            branch: "feat".into(),
            original_repo_dir: "/main/repo".into(),
        });
        sync_launch_anchor(pane, "/wt/feat/src", &wt, false);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_LAUNCH_CWD).as_deref(),
            Some("/main/repo"),
            "anchor must group worktree spawns under the original repo"
        );
    }

    #[test]
    fn sync_worktree_meta_clears_when_worktree_is_none() {
        let _guard = tmux::test_mock::install();
        let pane = "%CLEAR";
        tmux::test_mock::set(pane, tmux::PANE_WORKTREE_NAME, "old");
        tmux::test_mock::set(pane, tmux::PANE_WORKTREE_BRANCH, "feat/old");

        let mut writes = Vec::new();
        push_worktree_writes(&None, &mut writes);
        tmux::apply_pane_options(pane, &writes);

        assert!(!tmux::test_mock::contains(pane, tmux::PANE_WORKTREE_NAME));
        assert!(!tmux::test_mock::contains(pane, tmux::PANE_WORKTREE_BRANCH));
    }

    #[test]
    fn sync_worktree_meta_clears_individual_empty_fields() {
        // Regression: a Some(worktree) payload with only `name`
        // populated must still drop a stale `@pane_worktree_branch`
        // that a previous run set — otherwise the sidebar keeps
        // rendering the old branch.
        let _guard = tmux::test_mock::install();
        let pane = "%PARTIAL";
        tmux::test_mock::set(pane, tmux::PANE_WORKTREE_NAME, "old-name");
        tmux::test_mock::set(pane, tmux::PANE_WORKTREE_BRANCH, "feat/old");

        let wt = Some(WorktreeInfo {
            name: "new-name".into(),
            path: "/wt/new".into(),
            branch: String::new(),
            original_repo_dir: "/repo".into(),
        });
        let mut writes = Vec::new();
        push_worktree_writes(&wt, &mut writes);
        tmux::apply_pane_options(pane, &writes);

        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_WORKTREE_NAME).as_deref(),
            Some("new-name"),
            "non-empty name must overwrite"
        );
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_WORKTREE_BRANCH),
            "empty branch must clear the stale option"
        );
    }

    #[test]
    fn pane_writes_allowed_tracks_subagent_presence() {
        let _guard = tmux::test_mock::install();
        let _env = BIND_ENV_LOCK.lock().unwrap();
        let pane = "%ALLOWED";
        assert!(pane_writes_allowed(pane));
        tmux::test_mock::set(pane, tmux::PANE_SUBAGENTS, "Explore:sub-1");
        assert!(!pane_writes_allowed(pane));
    }

    #[test]
    fn session_bind_allowed_defaults_to_true_without_the_bridge_flag() {
        let _env = BIND_ENV_LOCK.lock().unwrap();
        // SAFETY: single-threaded w.r.t. this variable — every test that
        // touches TMUX_AGENT_SIDEBAR_BIND_SESSION holds BIND_ENV_LOCK.
        unsafe { std::env::remove_var("TMUX_AGENT_SIDEBAR_BIND_SESSION") };
        assert!(session_bind_allowed());
    }

    #[test]
    fn session_bind_allowed_is_false_only_for_an_explicit_zero() {
        let _env = BIND_ENV_LOCK.lock().unwrap();
        // SAFETY: see session_bind_allowed_defaults_to_true_without_the_bridge_flag.
        unsafe { std::env::set_var("TMUX_AGENT_SIDEBAR_BIND_SESSION", "0") };
        assert!(!session_bind_allowed());
        unsafe { std::env::set_var("TMUX_AGENT_SIDEBAR_BIND_SESSION", "1") };
        assert!(session_bind_allowed());
        unsafe { std::env::remove_var("TMUX_AGENT_SIDEBAR_BIND_SESSION") };
    }

    // Regression: the bridge's last-resort pane guess flags the spawn with
    // TMUX_AGENT_SIDEBAR_BIND_SESSION=0. The guessed pane can belong to
    // another live session — recording the binding there would permanently
    // misroute that session's later events, so the write must be skipped
    // while the rest of the location sync (cwd, worktree) still applies.
    #[test]
    fn sync_pane_location_preserves_the_binding_on_a_guessed_pane() {
        let _guard = tmux::test_mock::install();
        let _env = BIND_ENV_LOCK.lock().unwrap();
        let pane = "%GUESSED";
        tmux::test_mock::set(pane, tmux::PANE_SESSION_ID, "live-session");
        tmux::test_mock::set(pane, tmux::PANE_SESSION_TITLE, "Live work");
        // SAFETY: see session_bind_allowed_defaults_to_true_without_the_bridge_flag.
        unsafe { std::env::set_var("TMUX_AGENT_SIDEBAR_BIND_SESSION", "0") };

        sync_pane_location(pane, "/repo", &None, &Some("guessed-session".into()));

        unsafe { std::env::remove_var("TMUX_AGENT_SIDEBAR_BIND_SESSION") };
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_SESSION_ID).as_deref(),
            Some("live-session"),
            "a guessed pane's binding must not be overwritten"
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_SESSION_TITLE).as_deref(),
            Some("Live work"),
            "the guessed session's title must not take over either"
        );
        // Non-identity fields still sync: the event has to land somewhere.
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_CWD).as_deref(),
            Some("/repo")
        );
    }
}
