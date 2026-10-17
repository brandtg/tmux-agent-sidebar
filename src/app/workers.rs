use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use crate::group::{PaneGitInfo, resolve_pane_git_info};
use crate::port::{self, PaneProcessSnapshot, PaneScanTarget};
use crate::session;
use crate::version::{self, UpdateNotice};

/// How long a cached path→git-info entry is reused before the resolver
/// re-runs `git rev-parse` for it. Bounds both the subprocess rate (one
/// spawn per path per TTL instead of per tick) and the staleness of the
/// branch label shown for panes without hook-provided worktree metadata.
const GIT_INFO_TTL: Duration = Duration::from_secs(10);

/// Channels and shared flags produced by [`spawn`] that the main event loop
/// drains every tick.
pub(super) struct Workers {
    pub session_rx: Receiver<HashMap<String, String>>,
    pub version_rx: Receiver<UpdateNotice>,
    /// Requests the git-info resolver worker: the distinct pane working
    /// directories of the current snapshot. Results arrive on `git_info_rx`.
    pub git_info_tx: mpsc::Sender<Vec<String>>,
    pub git_info_rx: Receiver<HashMap<String, PaneGitInfo>>,
    /// Requests the port-scan worker: the panes to examine. Results arrive
    /// on `port_scan_rx`.
    pub port_scan_tx: mpsc::Sender<Vec<PaneScanTarget>>,
    pub port_scan_rx: Receiver<Option<PaneProcessSnapshot>>,
}

/// Spawn the background threads (session-name polling, version notice
/// fetch, git-info resolution, port scanning) that feed the event loop.
pub(super) fn spawn(_state: &crate::state::AppState) -> Workers {
    let (session_tx, session_rx) = mpsc::channel::<HashMap<String, String>>();
    let (version_tx, version_rx) = mpsc::channel::<UpdateNotice>();
    let (git_info_tx, git_info_req_rx) = mpsc::channel::<Vec<String>>();
    let (git_info_res_tx, git_info_rx) = mpsc::channel::<HashMap<String, PaneGitInfo>>();
    let (port_scan_tx, port_scan_req_rx) = mpsc::channel::<Vec<PaneScanTarget>>();
    let (port_scan_res_tx, port_scan_rx) = mpsc::channel::<Option<PaneProcessSnapshot>>();
    std::thread::spawn(move || {
        session_poll_loop(&session_tx);
    });
    std::thread::spawn(move || {
        if let Some(notice) = version::fetch_update_notice() {
            let _ = version_tx.send(notice);
        }
    });
    std::thread::spawn(move || {
        git_info_poll_loop(&git_info_req_rx, &git_info_res_tx);
    });
    std::thread::spawn(move || {
        port_scan_loop(&port_scan_req_rx, &port_scan_res_tx);
    });

    Workers {
        session_rx,
        version_rx,
        git_info_tx,
        git_info_rx,
        port_scan_tx,
        port_scan_rx,
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

/// Session name polling thread. Refreshes the `session_id → name` map every
/// 10 seconds — Claude `/rename` labels from `~/.claude/sessions/*.json` and
/// Hermes session titles from `~/.hermes/state.db` — so the main TUI thread
/// never performs blocking filesystem/subprocess I/O to refresh labels.
pub(super) fn session_poll_loop(tx: &mpsc::Sender<HashMap<String, String>>) {
    loop {
        std::thread::sleep(Duration::from_secs(10));
        let names = session::scan_session_names();
        if tx.send(names).is_err() {
            return;
        }
    }
}
