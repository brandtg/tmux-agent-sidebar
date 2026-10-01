use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use crate::git::{self, GitData};
use crate::group::{PaneGitInfo, resolve_pane_git_info};
use crate::port::{self, PaneProcessSnapshot, PaneScanTarget};
use crate::session;
use crate::state::{AppState, BottomTab};
use crate::tmux;
use crate::version::{self, UpdateNotice};

/// How long a cached path→git-info entry is reused before the resolver
/// re-runs `git rev-parse` for it. Bounds both the subprocess rate (one
/// spawn per path per TTL instead of per tick) and the staleness of the
/// branch label shown for panes without hook-provided worktree metadata.
const GIT_INFO_TTL: Duration = Duration::from_secs(10);

/// Channels and shared flags produced by [`spawn`] that the main event loop
/// drains every tick.
pub(super) struct Workers {
    pub git_rx: Receiver<GitData>,
    pub session_rx: Receiver<HashMap<String, String>>,
    pub version_rx: Receiver<UpdateNotice>,
    pub git_tab_active: Arc<AtomicBool>,
    /// Requests the focus-git fetch worker: pane ids whose git data should
    /// be refetched right away (focus changes). Results arrive on `git_rx`.
    pub focus_git_tx: mpsc::Sender<String>,
    /// Requests the git-info resolver worker: the distinct pane working
    /// directories of the current snapshot. Results arrive on `git_info_rx`.
    pub git_info_tx: mpsc::Sender<Vec<String>>,
    pub git_info_rx: Receiver<HashMap<String, PaneGitInfo>>,
    /// Requests the port-scan worker: the panes to examine. Results arrive
    /// on `port_scan_rx`.
    pub port_scan_tx: mpsc::Sender<Vec<PaneScanTarget>>,
    pub port_scan_rx: Receiver<Option<PaneProcessSnapshot>>,
}

/// Spawn the background threads (git polling, session-name polling, version
/// notice fetch, focus-git fetch, git-info resolution, port scanning) that
/// feed the event loop.
pub(super) fn spawn(state: &AppState) -> Workers {
    let (git_tx, git_rx) = mpsc::channel::<GitData>();
    let (session_tx, session_rx) = mpsc::channel::<HashMap<String, String>>();
    let (version_tx, version_rx) = mpsc::channel::<UpdateNotice>();
    let (focus_git_tx, focus_git_rx) = mpsc::channel::<String>();
    let (git_info_tx, git_info_req_rx) = mpsc::channel::<Vec<String>>();
    let (git_info_res_tx, git_info_rx) = mpsc::channel::<HashMap<String, PaneGitInfo>>();
    let (port_scan_tx, port_scan_req_rx) = mpsc::channel::<Vec<PaneScanTarget>>();
    let (port_scan_res_tx, port_scan_rx) = mpsc::channel::<Option<PaneProcessSnapshot>>();
    let tmux_pane_clone = state.tmux_pane.clone();
    let git_tab_active = Arc::new(AtomicBool::new(state.bottom_tab == BottomTab::GitStatus));
    let git_tab_flag = Arc::clone(&git_tab_active);
    let focus_git_tx_thread = git_tx.clone();
    std::thread::spawn(move || {
        git_poll_loop(&tmux_pane_clone, &git_tx, &git_tab_flag);
    });
    std::thread::spawn(move || {
        session_poll_loop(&session_tx);
    });
    std::thread::spawn(move || {
        if let Some(notice) = version::fetch_update_notice() {
            let _ = version_tx.send(notice);
        }
    });
    std::thread::spawn(move || {
        focus_git_fetch_loop(&focus_git_rx, &focus_git_tx_thread);
    });
    std::thread::spawn(move || {
        git_info_poll_loop(&git_info_req_rx, &git_info_res_tx);
    });
    std::thread::spawn(move || {
        port_scan_loop(&port_scan_req_rx, &port_scan_res_tx);
    });

    Workers {
        git_rx,
        session_rx,
        version_rx,
        git_tab_active,
        focus_git_tx,
        git_info_tx,
        git_info_rx,
        port_scan_tx,
        port_scan_rx,
    }
}

/// Focus-change git fetch worker. Replaces the old synchronous
/// `refresh_git_for_focused_pane` call on the render thread, which blocked
/// the TUI for up to seven git subprocesses (5s deadline each) whenever
/// focus changed. The event loop sends the newly focused pane id; the
/// worker resolves its path and fetches the data, and the result rides the
/// same `git_tx` channel the git-tab poller uses.
pub(super) fn focus_git_fetch_loop(rx: &mpsc::Receiver<String>, git_tx: &mpsc::Sender<GitData>) {
    while let Ok(pane_id) = rx.recv() {
        let Some(path) = tmux::get_pane_path(&pane_id) else {
            continue;
        };
        let data = git::fetch_git_data(&path);
        if git_tx.send(data).is_err() {
            return;
        }
    }
}

/// Git-metadata resolver worker. Owns the path→[`PaneGitInfo`] cache that
/// `group_panes_by_repo` consumes: each tick the event loop sends the
/// distinct pane working directories, the worker refreshes entries that are
/// missing or older than [`GIT_INFO_TTL`] (one `git rev-parse` per stale
/// path — the spawn the render thread used to pay every tick) and returns
/// the full map. Entries for paths that dropped out of the snapshot are
/// pruned.
pub(super) fn git_info_poll_loop(
    rx: &mpsc::Receiver<Vec<String>>,
    tx: &mpsc::Sender<HashMap<String, PaneGitInfo>>,
) {
    let mut cache: HashMap<String, (PaneGitInfo, std::time::Instant)> = HashMap::new();
    while let Ok(paths) = rx.recv() {
        let now = std::time::Instant::now();
        let wanted: HashSet<&str> = paths.iter().map(|path| path.as_str()).collect();
        cache.retain(|path, _| wanted.contains(path.as_str()));
        for path in &paths {
            let fresh = cache
                .get(path)
                .is_some_and(|(_, resolved_at)| now.duration_since(*resolved_at) < GIT_INFO_TTL);
            if !fresh {
                let info = resolve_pane_git_info(path);
                cache.insert(path.clone(), (info, now));
            }
        }
        let snapshot: HashMap<String, PaneGitInfo> = cache
            .iter()
            .map(|(path, (info, _))| (path.clone(), info.clone()))
            .collect();
        if tx.send(snapshot).is_err() {
            return;
        }
    }
}

/// Port-scan worker. Runs the `ps` snapshot plus the `lsof` listening-port
/// probe off the render thread (the inline scan used to stall the TUI for
/// up to the subprocess timeout every 10 seconds). A failed scan sends
/// `None`; the event loop ignores it and the next due scan retries.
pub(super) fn port_scan_loop(
    rx: &mpsc::Receiver<Vec<PaneScanTarget>>,
    tx: &mpsc::Sender<Option<PaneProcessSnapshot>>,
) {
    while let Ok(targets) = rx.recv() {
        let scanned = port::scan_pane_processes(&targets);
        if tx.send(scanned).is_err() {
            return;
        }
    }
}

/// Session name polling thread. Scans `~/.claude/sessions/*.json` every 10
/// seconds so the main TUI thread never performs blocking filesystem I/O
/// to refresh `/rename`-assigned labels.
pub(super) fn session_poll_loop(tx: &mpsc::Sender<HashMap<String, String>>) {
    loop {
        std::thread::sleep(Duration::from_secs(10));
        let names = session::scan_session_names();
        if tx.send(names).is_err() {
            return;
        }
    }
}

/// Git data polling thread. Fetches git status every 2 seconds while the Git
/// tab is active. Skips fetching when the tab is not visible. PR numbers go
/// through an in-memory `(path, branch)`-keyed cache so `gh pr view` (the only
/// hop that costs GitHub API quota) runs at most once per `PR_CACHE_TTL`
/// instead of every tick.
pub(super) fn git_poll_loop(tmux_pane: &str, git_tx: &mpsc::Sender<GitData>, active: &AtomicBool) {
    let mut last_path: Option<String> = None;
    let mut pr_cache = git::PrCache::new();
    loop {
        std::thread::sleep(Duration::from_secs(2));

        if !active.load(Ordering::Relaxed) {
            continue;
        }

        // When the sidebar has focus, focused_pane_path returns None.
        // Reuse the last known path so git data keeps updating.
        if let Some(p) = tmux::focused_pane_path(tmux_pane) {
            last_path = Some(p);
        }
        if let Some(ref path) = last_path {
            let mut data = git::fetch_git_data(path);
            data.pr_number = pr_cache.get_or_fetch(
                path,
                &data.branch,
                std::time::Instant::now(),
                git::fetch_pr_number,
            );
            if git_tx.send(data).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_poll_skips_when_inactive() {
        let active = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<GitData>();

        let flag = Arc::clone(&active);
        let handle = std::thread::spawn(move || {
            // Simulate the poll loop check without actually sleeping 2s
            for _ in 0..3 {
                if !flag.load(Ordering::Relaxed) {
                    continue;
                }
                let _ = tx.send(GitData::default());
            }
        });

        handle.join().unwrap();
        // No data should have been sent since active=false
        assert!(
            rx.try_recv().is_err(),
            "should not poll when git tab is inactive"
        );
    }

    #[test]
    fn test_git_poll_sends_when_active() {
        let active = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::channel::<GitData>();

        let flag = Arc::clone(&active);
        let handle = std::thread::spawn(move || {
            // active=true, so it should send
            if flag.load(Ordering::Relaxed) {
                let _ = tx.send(GitData::default());
            }
        });

        handle.join().unwrap();
        assert!(rx.try_recv().is_ok(), "should poll when git tab is active");
    }

    #[test]
    fn test_git_poll_reacts_to_flag_change() {
        let active = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<GitData>();

        // Initially inactive
        assert!(!active.load(Ordering::Relaxed));

        // Switch to active
        active.store(true, Ordering::Relaxed);

        let flag = Arc::clone(&active);
        let handle = std::thread::spawn(move || {
            if flag.load(Ordering::Relaxed) {
                let _ = tx.send(GitData::default());
            }
        });

        handle.join().unwrap();
        assert!(
            rx.try_recv().is_ok(),
            "should poll after flag switches to active"
        );
    }

    #[test]
    fn test_git_poll_stops_on_sender_closed() {
        let active = AtomicBool::new(true);
        let (tx, rx) = mpsc::channel::<GitData>();
        drop(rx); // Close receiver

        let result = tx.send(GitData::default());
        assert!(result.is_err(), "send should fail when receiver is dropped");

        // Verify the flag check pattern used in git_poll_loop
        assert!(active.load(Ordering::Relaxed));
    }
}
