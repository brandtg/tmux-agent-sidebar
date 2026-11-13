//! Main application orchestration: prime the [`AppState`], spawn background
//! workers, and run the crossterm event loop. Split out from `src/main.rs` so
//! the binary entry point only handles CLI arg parsing, signal wiring, and
//! TUI session setup.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{self};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::SPINNER_PULSE;

mod input;
mod render;
mod setup;
mod workers;

/// Minimum gap between signal-driven refresh requests. Hook events ride
/// SIGUSR1 from every agent in every window; without a gap a burst of
/// hooks would enqueue a spawn per signal. The gap only throttles the
/// signal path — the periodic 1s tick is unaffected — and a coalesced
/// signal is latched back onto `needs_refresh` until the gap elapses.
const MIN_REFRESH_GAP: Duration = Duration::from_millis(150);

/// Run the TUI event loop. Returns when the loop exits — on a fatal I/O
/// error, or when the user presses `q` (`state.quit_requested`), which
/// tears down a popup via `display-popup -E` and closes the pane-mode
/// sidebar's pane.
///
/// `needs_refresh` is the process-wide SIGUSR1 flag owned by `main.rs` — the
/// signal handler must reference a static visible at signal-handler time,
/// so the static stays with the `extern "C"` handler in the binary crate and
/// we just borrow it here.
pub fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    tmux_pane: String,
    needs_refresh: &'static AtomicBool,
) -> io::Result<()> {
    let mut state = setup::init_state(tmux_pane);
    // Worktree spawn/remove run on per-operation worker threads (they
    // can spend minutes in git under a hung repo); results land here
    // for the loop to apply — the same one-way-mpsc pattern as the
    // Workers feeds below. The render thread stays the sole owner of
    // AppState.
    let (popup_op_tx, popup_op_rx) = mpsc::channel();
    state.popup_op_tx = Some(popup_op_tx);
    let mut window_inactive_count: u32 = 0;

    let workers = workers::spawn(&state);
    let workers::Workers {
        snapshot_tx,
        snapshot_rx,
        session_rx,
        version_rx,
        git_info_tx,
        git_info_rx,
        port_scan_tx,
        port_scan_rx,
        approval_tx,
    } = workers;

    let mut last_refresh = std::time::Instant::now();
    let mut last_spinner = std::time::Instant::now();
    let refresh_interval = Duration::from_secs(1);
    let spinner_interval = Duration::from_millis(200);
    let mut needs_redraw = true;
    // A request is on the wire and has not been applied yet. Refresh
    // requests coalesce onto this flag: hook events can arrive in bursts
    // (every agent in every window), and each one used to cost its own
    // synchronous tmux spawn.
    let mut snapshot_in_flight = false;

    loop {
        if needs_redraw {
            render::render_frame(terminal, &mut state)?;
            needs_redraw = false;
        }

        // Refresh request: hook-event signals (SIGUSR1) and the periodic
        // tick both land here. The tmux spawns run on the snapshot worker —
        // `list-panes -a` used to run synchronously right here, and a
        // keystroke arriving mid-spawn queued behind the subprocess, which
        // is what made typing in the sidebar feel laggy. The raw listing is
        // parsed and applied further down when it comes back. Signal-driven
        // requests also keep a minimum gap so a hook storm cannot turn into
        // a spawn storm.
        let signal_pending = needs_refresh.swap(false, Ordering::Relaxed);
        let since_refresh = last_refresh.elapsed();
        let refresh_due = (signal_pending && since_refresh >= MIN_REFRESH_GAP)
            || since_refresh >= refresh_interval;
        if refresh_due && !snapshot_in_flight {
            state.refresh_now();
            state
                .todos
                .refresh_if_changed(state.global.todo_hide_completed);
            let request = workers::SnapshotRequest {
                include_globals: signal_pending,
            };
            if snapshot_tx.send(request).is_ok() {
                snapshot_in_flight = true;
            }
            last_refresh = std::time::Instant::now();
        } else if signal_pending {
            // Coalesced: latch the signal back so the next wake fires it.
            needs_refresh.store(true, Ordering::Relaxed);
        }

        let refresh_timeout = refresh_interval.saturating_sub(last_refresh.elapsed());
        let spinner_timeout = spinner_interval.saturating_sub(last_spinner.elapsed());
        let timeout = refresh_timeout
            .min(spinner_timeout)
            .min(Duration::from_millis(16));
        if event::poll(timeout)? {
            loop {
                let ev = event::read()?;
                if input::handle_event(ev, &mut state) {
                    needs_redraw = true;
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        if state.quit_requested {
            // Pane-mode sidebars close by process exit, so the layout
            // restore must be scheduled from a server-side job that
            // outlives the pane (see toggle::schedule_restore_after_pane_exit).
            // Popup mode never snapshots a layout and the helper no-ops
            // on a missing snapshot, but skipping it here keeps popup
            // teardown free of tmux round-trips.
            if !state.popup_mode {
                crate::cli::toggle::schedule_restore_after_pane_exit(&state.tmux_pane);
            }
            break Ok(());
        }

        if last_spinner.elapsed() >= spinner_interval {
            state.spinner_frame = (state.spinner_frame + 1) % SPINNER_PULSE.len();
            last_spinner = std::time::Instant::now();
            needs_redraw = true;
        }

        // Snapshot apply: the worker's raw listing landed — fold it into
        // state and run the per-refresh follow-ups (auto-close, focus
        // selection, worker feeds, shared-option reload). The same work
        // the synchronous refresh used to do inline, minus the spawn.
        if let Ok(raw) = snapshot_rx.try_recv() {
            snapshot_in_flight = false;
            let previous_sidebar_focused = state.focus_state.sidebar_focused;
            let is_window_active = match raw.panes_output {
                Some(output) => state.apply_session_refresh(&output),
                // A failed `list-panes` call is not evidence that no panes
                // exist (the sync twin holds the same line): apply nothing,
                // keep the last known-good snapshot, retry next tick.
                None => state.focus_state.window_active,
            };
            // A sidebar whose window has no panes left besides itself
            // would render as a full-window sidebar. The `auto-close`
            // hook cannot always repair that (its pane-exited
            // `#{window_id}` expands against the wrong window when the
            // exiting pane is removed before the hook's format runs, and
            // the last-window guard preserves a shared session), so the
            // instance closes itself the same way `q` does: exit the
            // process, which closes the pane and — when it was the last
            // one — the window with it. Sidebars in other windows and
            // sessions are never touched. The `quit_requested` check at
            // the top of the loop runs the normal teardown.
            if state.window_has_no_other_panes {
                state.quit_requested = true;
            }
            // The sidebar pane becoming the active pane (user jumped into
            // the sidebar) is the one transition this loop acts on: select
            // the agent matching the tmux context instead of re-anchoring
            // at the top. Popup mode forces `sidebar_focused` true from its
            // startup refresh on, so its context selection happens in
            // `setup::init_state` and this never fires there.
            let sidebar_gained_focus =
                !previous_sidebar_focused && state.focus_state.sidebar_focused;
            // Hand the queued port scan (if due) to its worker.
            if let Some(targets) = state.take_pending_port_scan() {
                let _ = port_scan_tx.send(targets);
            }
            // Hand the queued approval scan (if due) to its worker, which
            // writes the waiting state onto the pane options directly.
            if let Some(pane_ids) = state.take_pending_approval_scan() {
                let _ = approval_tx.send(pane_ids);
            }
            // Keep the git-info resolver fed with the current grouping
            // anchors (launch cwd when captured, else live cwd — the same
            // keys group_panes_by_repo looks up); it refreshes stale
            // entries off-thread and returns the cache.
            let mut paths: Vec<String> = state
                .repo_groups
                .iter()
                .flat_map(|group| group.panes.iter())
                .map(|(pane, _)| pane.grouping_anchor().to_string())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            paths.extend(
                state
                    .other_windows
                    .values()
                    .flatten()
                    .map(|window| window.path.clone())
                    .filter(|path| !path.is_empty()),
            );
            if !paths.is_empty() {
                let _ = git_info_tx.send(paths);
            }
            needs_redraw = true;
            // Reload shared global options (compact, filters, cursor) when
            // the tick was signal-driven — the request asked the worker to
            // fetch them, because a broadcast from another sidebar has to
            // land on *inactive* instances too (those are exactly the
            // stale ones) — and when an inactive window regains visibility
            // (≥2 inactive ticks) as the missed-signal safety net, falling
            // back to a direct load since no globals rode this snapshot.
            if raw.global_options.is_some() || (is_window_active && window_inactive_count >= 2) {
                let previous_group_by = state.global.group_by;
                match &raw.global_options {
                    Some(opts) => state.global.apply_all(opts),
                    None => state.global.load_from_tmux(),
                }
                // Group names are mode-specific (repo names vs session
                // names). A collapse set from the previous mode would
                // otherwise hide an unrelated group that happens to share
                // a name, so start the new mode fully expanded.
                if state.global.group_by != previous_group_by {
                    state.collapsed_groups.clear();
                }
                state.rebuild_row_targets();
                // The hide-completed setting may have changed in another
                // sidebar; keep the TODO selection on a visible item. The
                // panel's shared visibility may have changed too (another
                // sidebar opened or closed it), so adopt that first.
                state.sync_todo_visibility();
                state
                    .todos
                    .clamp_selection(state.global.todo_hide_completed);
                // A window/pane switch reloads the shared cursor, which
                // another window may have moved. Re-anchor this sidebar at
                // the top so the switch shows every agent instead of
                // auto-scrolling to the adopted row — but only while the
                // sidebar pane is unfocused. Hook events from agents in
                // this window ride the same SIGUSR1 (notify_sidebars_of_pane),
                // and while the user is navigating the list the selection
                // is theirs: resetting on every agent-state signal yanked
                // the cursor back to the first row mid-navigation. Leaving
                // the sidebar flips the focus flag, so the next signal
                // re-anchors as before; arriving re-anchors through the
                // context selection below.
                if !state.focus_state.sidebar_focused {
                    // A search is a transient navigation aid scoped to the
                    // sidebar's focus: leaving the sidebar (a jump, a
                    // manual pane switch) ends it, so returning shows the
                    // full list instead of a stale filter.
                    state.cancel_search();
                    state.reset_pane_scroll();
                }
            }
            if sidebar_gained_focus {
                // Focus normally arrives via the after-select-pane/window
                // hook, so this rides the same tick as the refresh that
                // observed it; the periodic tick is the safety net for a
                // missed signal. Runs after `rebuild_row_targets` so the
                // row index matches the filtered list this frame renders,
                // and `compute_scroll_offset` brings the row into view.
                if !state.select_focused_context_row() {
                    state.reset_pane_scroll();
                }
            }
            if is_window_active {
                window_inactive_count = 0;
            } else {
                window_inactive_count = window_inactive_count.saturating_add(1);
            }
        }

        if let Ok(scanned) = session_rx.try_recv() {
            state.sessions.names = scanned.names;
            state.sessions.derived = scanned.derived;
            needs_redraw = true;
        }

        if let Ok(notice) = version_rx.try_recv() {
            state.version_notice = Some(notice);
            needs_redraw = true;
        }

        if let Ok(git_info) = git_info_rx.try_recv() {
            state.git_info_cache = git_info;
            needs_redraw = true;
        }

        while let Some(scanned) = port_scan_rx.try_recv().ok().flatten() {
            state.apply_process_snapshot(scanned);
            needs_redraw = true;
        }

        while let Ok(result) = popup_op_rx.try_recv() {
            state.apply_popup_op_result(result);
            needs_redraw = true;
        }

        state
            .global
            .flush_pending_cursor_save(std::time::Duration::from_millis(120));
    }
}
