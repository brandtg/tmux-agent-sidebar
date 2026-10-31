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

    loop {
        if needs_redraw {
            render::render_frame(terminal, &mut state)?;
            needs_redraw = false;
        }

        let refresh_timeout = refresh_interval.saturating_sub(last_refresh.elapsed());
        let spinner_timeout = spinner_interval.saturating_sub(last_spinner.elapsed());
        let timeout = if needs_refresh.load(Ordering::Relaxed) {
            Duration::ZERO
        } else {
            refresh_timeout
                .min(spinner_timeout)
                .min(Duration::from_millis(16))
        };
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

        let sigusr1 = needs_refresh.swap(false, Ordering::Relaxed);
        if sigusr1 || last_refresh.elapsed() >= refresh_interval {
            let previous_sidebar_focused = state.focus_state.sidebar_focused;
            let is_window_active = state.refresh();
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
            // Reload shared global options (compact, filters, cursor) on
            // every SIGUSR1-driven refresh, and when an inactive window
            // regains visibility (≥2 inactive ticks). The signal must
            // reload regardless of window-active state: a broadcast from
            // another sidebar has to land on *inactive* instances too —
            // those are exactly the stale ones — and `#{window_active}`
            // is per-session, so a sidebar whose window leads its own
            // (unfocused) session never registers a visible→hidden
            // transition for the second branch to catch.
            if sigusr1 || (is_window_active && window_inactive_count >= 2) {
                state.global.load_from_tmux();
                state.rebuild_row_targets();
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
            last_refresh = std::time::Instant::now();
        }

        if let Ok(names) = session_rx.try_recv() {
            state.sessions.names = names;
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
