use std::collections::HashMap;
use std::time::Instant;

use crate::tmux;

use super::filter::{RepoFilter, StatusFilter};

/// State shared across all sidebar instances via tmux global variables.
/// Synced from tmux at startup and on pane focus change (SIGUSR1).
pub struct GlobalState {
    pub status_filter: StatusFilter,
    pub selected_pane_row: usize,
    pub repo_filter: RepoFilter,
    /// Last filter value successfully written to tmux.
    last_saved_filter: StatusFilter,
    /// Last cursor value successfully written to tmux.
    last_saved_cursor: usize,
    /// Last repo filter value successfully written to tmux.
    last_saved_repo_filter: RepoFilter,
    /// When the selected cursor was last changed and still needs persisting.
    pending_cursor_save_since: Option<Instant>,
}

impl Default for GlobalState {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalState {
    pub fn new() -> Self {
        Self {
            status_filter: StatusFilter::All,
            selected_pane_row: 0,
            repo_filter: RepoFilter::All,
            last_saved_filter: StatusFilter::All,
            last_saved_cursor: 0,
            last_saved_repo_filter: RepoFilter::All,
            pending_cursor_save_since: None,
        }
    }

    /// Save filter to tmux global variable.
    /// Only updates `last_saved_filter` on success so that a failed write
    /// does not cause sync to overwrite the user's choice.
    pub fn save_filter(&mut self) {
        if tmux::run_tmux(&[
            "set",
            "-g",
            tmux::SIDEBAR_FILTER,
            self.status_filter.as_str(),
        ])
        .is_some()
        {
            self.last_saved_filter = self.status_filter;
        }
    }

    /// Save cursor position to tmux global variable. Returns `true` when
    /// tmux accepted the write so callers can decide whether to clear a
    /// queued save or keep retrying.
    pub fn save_cursor(&mut self) -> bool {
        if tmux::run_tmux(&[
            "set",
            "-g",
            tmux::SIDEBAR_CURSOR,
            &self.selected_pane_row.to_string(),
        ])
        .is_some()
        {
            self.last_saved_cursor = self.selected_pane_row;
            true
        } else {
            false
        }
    }

    /// Mark the cursor as dirty so the main loop can persist it once the
    /// user pauses navigation.
    pub fn queue_cursor_save(&mut self) {
        self.pending_cursor_save_since = Some(Instant::now());
    }

    /// Persist a queued cursor update after it has been idle for at least the
    /// requested debounce duration. Returns true when the queue was consumed.
    pub fn flush_pending_cursor_save(&mut self, debounce: std::time::Duration) -> bool {
        let Some(queued_at) = self.pending_cursor_save_since else {
            return false;
        };
        if queued_at.elapsed() < debounce {
            return false;
        }
        // Only clear the pending marker on successful tmux write — otherwise
        // a transient failure would silently drop the queued save instead of
        // retrying on the next flush tick.
        if self.save_cursor() {
            self.pending_cursor_save_since = None;
            true
        } else {
            false
        }
    }

    /// Save repo filter to tmux global variable.
    pub fn save_repo_filter(&mut self) {
        if tmux::run_tmux(&[
            "set",
            "-g",
            tmux::SIDEBAR_REPO_FILTER,
            self.repo_filter.as_str(),
        ])
        .is_some()
        {
            self.last_saved_repo_filter = self.repo_filter.clone();
        }
    }

    /// Load all global state from tmux variables.
    /// Called at startup and on SIGUSR1 (pane focus change).
    pub fn load_from_tmux(&mut self) {
        let opts = tmux::get_all_global_options();
        self.apply_all(&opts);
    }

    /// Parse `@sidebar_default_view` into the status filter a newly opened
    /// sidebar lands on. Accepts the same labels as [`StatusFilter::as_str`],
    /// case-insensitively; unset or unrecognized values fall back to `all`,
    /// the first view.
    pub fn default_view_from_options(opts: &HashMap<String, String>) -> StatusFilter {
        opts.get(tmux::SIDEBAR_DEFAULT_VIEW)
            .map(|s| StatusFilter::from_label(s.trim().to_ascii_lowercase().as_str()))
            .unwrap_or(StatusFilter::All)
    }

    /// Land on the configured default view instead of the last-used filter.
    ///
    /// `@sidebar_filter` persists the most recent filter across sidebar
    /// instances, so a freshly opened sidebar would otherwise resume
    /// whatever filter some other window set earlier — a random-looking
    /// landing view. Called once at startup, after `load_from_tmux`.
    ///
    /// Only `status_filter` is overridden: `apply_all` has already pointed
    /// `last_saved_filter` at the persisted value, so the window-refocus
    /// reload in the main loop does not restore the stale filter over the
    /// default, while filter changes from other open sidebars still sync
    /// in. The default is never written back to tmux here — opening a
    /// sidebar must not yank the view out from under already-open ones.
    pub fn apply_default_view(&mut self, opts: &HashMap<String, String>) {
        self.status_filter = Self::default_view_from_options(opts);
    }

    /// Startup variant of [`GlobalState::apply_default_view`] that reads
    /// tmux directly. The refocus reload path deliberately keeps the plain
    /// shared-sync behavior, so this must only be called during setup.
    pub fn apply_default_view_from_tmux(&mut self) {
        let opts = tmux::get_all_global_options();
        self.apply_default_view(&opts);
    }

    /// Apply all global options from tmux (filter, cursor, repo filter).
    pub fn apply_all(&mut self, opts: &HashMap<String, String>) {
        if let Some(filter_str) = opts.get(tmux::SIDEBAR_FILTER) {
            let tmux_filter = StatusFilter::from_label(filter_str);
            if tmux_filter != self.last_saved_filter {
                self.status_filter = tmux_filter;
                self.last_saved_filter = tmux_filter;
            }
        }
        if let Some(cursor_str) = opts.get(tmux::SIDEBAR_CURSOR)
            && let Ok(n) = cursor_str.parse::<usize>()
            && n != self.last_saved_cursor
        {
            self.selected_pane_row = n;
            self.last_saved_cursor = n;
        }
        if let Some(repo_str) = opts.get(tmux::SIDEBAR_REPO_FILTER) {
            let tmux_repo = RepoFilter::from_label(repo_str);
            if tmux_repo != self.last_saved_repo_filter {
                self.repo_filter = tmux_repo.clone();
                self.last_saved_repo_filter = tmux_repo;
            }
        }
    }
}
