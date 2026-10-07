//! Main application orchestration: prime the [`AppState`], spawn background
//! workers, and run the crossterm event loop. Split out from `src/main.rs` so
//! the binary entry point only handles CLI arg parsing, signal wiring, and
//! TUI session setup.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{self};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::SPINNER_PULSE;
use crate::state::BottomTab;

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
    let mut window_inactive_count: u32 = 0;

    let workers = workers::spawn(&state);
    let workers::Workers {
        git_rx,
        session_rx,
        version_rx,
        git_tab_active,
        focus_git_tx,
        git_info_tx,
        git_info_rx,
        port_scan_tx,
        port_scan_rx,
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
                if input::handle_event(ev, &mut state, &git_tab_active, terminal) {
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
            if state.pet_enabled {
                let term_width = terminal.size().map(|s| s.width).unwrap_or(60);
                state.tick_pet(term_width);
            }
            last_spinner = std::time::Instant::now();
            needs_redraw = true;
        }

        let sigusr1 = needs_refresh.swap(false, Ordering::Relaxed);
        if sigusr1 || last_refresh.elapsed() >= refresh_interval {
            let previous_focused_pane_id = state.focus_state.focused_pane_id.clone();
            let is_window_active = state.refresh();
            if state.focus_state.focused_pane_id != previous_focused_pane_id {
                // The seven-git-call fetch for the newly focused pane runs
                // on the focus-fetch worker; the result arrives on `git_rx`
                // a frame later instead of stalling this tick.
                if let Some(pane_id) = state.focus_state.focused_pane_id.clone() {
                    let _ = focus_git_tx.send(pane_id);
                }
            }
            // Hand the queued port scan (if due) to its worker.
            if let Some(targets) = state.take_pending_port_scan() {
                let _ = port_scan_tx.send(targets);
            }
            // Keep the git-info resolver fed with the current grouping
            // anchors (launch cwd when captured, else live cwd — the same
            // keys group_panes_by_repo looks up); it refreshes stale
            // entries off-thread and returns the cache.
            let paths: Vec<String> = state
                .repo_groups
                .iter()
                .flat_map(|group| group.panes.iter())
                .map(|(pane, _)| pane.grouping_anchor().to_string())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
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
                // auto-scrolling to the adopted row.
                state.reset_pane_scroll();
            }
            if is_window_active {
                window_inactive_count = 0;
            } else {
                window_inactive_count = window_inactive_count.saturating_add(1);
            }
            git_tab_active.store(state.bottom_tab == BottomTab::GitStatus, Ordering::Relaxed);
            last_refresh = std::time::Instant::now();
        }

        while let Ok(data) = git_rx.try_recv() {
            state.apply_git_data(data);
            needs_redraw = true;
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

        state
            .global
            .flush_pending_cursor_save(std::time::Duration::from_millis(120));
    }
}
