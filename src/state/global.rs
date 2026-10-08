use std::collections::HashMap;
use std::time::Instant;

use crate::tmux;

use super::filter::{RepoFilter, StatusFilter};

/// State shared across all sidebar instances via tmux global variables.
/// Synced from tmux at startup, on pane focus change (SIGUSR1), and
/// whenever another sidebar broadcasts a shared-option change.
pub struct GlobalState {
    pub status_filter: StatusFilter,
    pub selected_pane_row: usize,
    pub repo_filter: RepoFilter,
    /// Compact one-line-per-pane rendering. Toggleable at runtime with
    /// `c`; the live value is shared across open sidebars via
    /// `@sidebar_compact` so every sidebar renders the same density.
    /// `@sidebar_default_compact_view` only seeds the mode when no live
    /// value exists yet (fresh tmux server, before the first toggle).
    pub compact: bool,
    /// Show the "other windows" section beneath each repo's agent rows.
    /// Toggleable at runtime with `w`; the live value is shared across
    /// open sidebars via `@sidebar_show_windows`.
    /// `@sidebar_default_show_windows` only seeds the mode when no live
    /// value exists yet.
    pub show_windows: bool,
    /// Pane id of the sidebar instance owning this state. Used to skip
    /// self-signaling when broadcasting shared-option changes.
    owner_pane: String,
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
        Self::new(String::new())
    }
}

impl GlobalState {
    pub fn new(owner_pane: String) -> Self {
        Self {
            status_filter: StatusFilter::All,
            selected_pane_row: 0,
            repo_filter: RepoFilter::All,
            compact: false,
            show_windows: false,
            owner_pane,
            last_saved_filter: StatusFilter::All,
            last_saved_cursor: 0,
            last_saved_repo_filter: RepoFilter::All,
            pending_cursor_save_since: None,
        }
    }

    /// Signal every other open sidebar to reload the shared options.
    /// Called after this instance changes one, so the new value lands
    /// immediately everywhere — including sidebars whose window never
    /// leaves the "active" state of their own session, which the
    /// focus-transition reload cannot reach. Cursor updates do not
    /// broadcast: they fire on every navigation keystroke, and the
    /// focus-change reload keeps them converging cheaply.
    fn broadcast_change(&self) {
        tmux::broadcast_refresh(&self.owner_pane);
    }

    /// Save filter to tmux global variable, then broadcast the change to
    /// every other open sidebar.
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
            self.broadcast_change();
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

    /// Save repo filter to tmux global variable, then broadcast the
    /// change to every other open sidebar.
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
            self.broadcast_change();
        }
    }

    /// Flip compact mode and broadcast the new value to every open
    /// sidebar via the `@sidebar_compact` tmux global option.
    pub fn toggle_compact(&mut self) {
        self.compact = !self.compact;
        if self.save_compact() {
            self.broadcast_change();
        }
    }

    /// Save compact flag to tmux global variable. Returns `true` when
    /// tmux accepted the write (and the caller should broadcast). On a
    /// failed write the next sync reverts this sidebar to the shared
    /// density instead of leaving it diverged.
    pub fn save_compact(&mut self) -> bool {
        tmux::run_tmux(&[
            "set",
            "-g",
            tmux::SIDEBAR_COMPACT,
            if self.compact { "1" } else { "0" },
        ])
        .is_some()
    }

    /// Flip the "other windows" section and broadcast the new value to
    /// every open sidebar via the `@sidebar_show_windows` tmux global.
    pub fn toggle_show_windows(&mut self) {
        self.show_windows = !self.show_windows;
        if self.save_show_windows() {
            self.broadcast_change();
        }
    }

    /// Save the "other windows" flag to tmux. Returns `true` when tmux
    /// accepted the write (and the caller should broadcast). On a failed
    /// write the next sync reverts this sidebar to the shared value.
    pub fn save_show_windows(&mut self) -> bool {
        tmux::run_tmux(&[
            "set",
            "-g",
            tmux::SIDEBAR_SHOW_WINDOWS,
            if self.show_windows { "1" } else { "0" },
        ])
        .is_some()
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

    /// Parse `@sidebar_default_compact_view` into the landing compact
    /// mode for a newly opened sidebar. Accepts `on`/`true`/`1`
    /// (case-insensitive); unset or any other value means `off`.
    pub fn default_compact_view_from_options(opts: &HashMap<String, String>) -> bool {
        opts.get(tmux::SIDEBAR_DEFAULT_COMPACT_VIEW)
            .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "on" | "true" | "1"))
            .unwrap_or(false)
    }

    /// Parse `@sidebar_default_show_windows` into the landing "other
    /// windows" mode for a newly opened sidebar. Same accepted values as
    /// [`Self::default_compact_view_from_options`].
    pub fn default_show_windows_from_options(opts: &HashMap<String, String>) -> bool {
        opts.get(tmux::SIDEBAR_DEFAULT_SHOW_WINDOWS)
            .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "on" | "true" | "1"))
            .unwrap_or(false)
    }

    /// Land on the configured default view instead of the last-used
    /// filter/compact values.
    ///
    /// `@sidebar_filter` persists the most recent value across sidebar
    /// instances, so a freshly opened sidebar would otherwise resume
    /// whatever some other window set earlier — a random-looking landing
    /// state. Called once at startup, after `load_from_tmux`.
    ///
    /// Only `status_filter` is overridden unconditionally. For `compact`
    /// the landing default applies only while no live `@sidebar_compact`
    /// value exists: once any sidebar has toggled, the shared value wins
    /// so every sidebar renders the same density. The defaults are never
    /// written back to tmux here — opening a sidebar must not yank the
    /// view out from under already-open ones.
    pub fn apply_default_view(&mut self, opts: &HashMap<String, String>) {
        self.status_filter = Self::default_view_from_options(opts);
        if !opts.contains_key(tmux::SIDEBAR_COMPACT) {
            self.compact = Self::default_compact_view_from_options(opts);
        }
        if !opts.contains_key(tmux::SIDEBAR_SHOW_WINDOWS) {
            self.show_windows = Self::default_show_windows_from_options(opts);
        }
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
        if let Some(compact_str) = opts.get(tmux::SIDEBAR_COMPACT) {
            // Compact is adopted unconditionally: the live tmux value is
            // the shared density across sidebars, so it also overrides a
            // landing default from `@sidebar_default_compact_view` (which
            // only applies while this option is unset).
            let tmux_compact = compact_str.trim() == "1";
            self.compact = tmux_compact;
        }
        if let Some(show_windows_str) = opts.get(tmux::SIDEBAR_SHOW_WINDOWS) {
            self.show_windows = show_windows_str.trim() == "1";
        }
    }
}
