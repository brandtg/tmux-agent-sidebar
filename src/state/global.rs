use std::collections::HashMap;
use std::time::Instant;

use crate::tmux;

use super::filter::{RepoFilter, StatusFilter};

/// How the agents panel buckets panes into groups. Set via the
/// `@sidebar_group_by` tmux global; configurable in tmux.conf and
/// toggleable at runtime with `s`, which writes the shared option and
/// broadcasts the change like `c` / `w`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupBy {
    /// Bucket panes by resolved git repo root (the historical default).
    #[default]
    Repo,
    /// Bucket panes by tmux session name. With the windows section
    /// enabled this becomes an agent-forward session manager: sessions
    /// without agents render their plain windows too.
    Session,
}

impl GroupBy {
    /// Parse a tmux-option label into a [`GroupBy`]. Unknown values fall
    /// back to `Repo`.
    pub fn from_label(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "session" => Self::Session,
            _ => Self::Repo,
        }
    }

    pub fn as_label(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Session => "session",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Repo => Self::Session,
            Self::Session => Self::Repo,
        }
    }
}

/// How the manual TODO panel's open/closed state is scoped. Set via
/// `@sidebar_todo_scope`; the item list itself is always shared through
/// the backing file, so this only governs the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TodoScope {
    /// One panel state for every sidebar instance: opening or closing it
    /// in one session does the same everywhere (the default).
    #[default]
    Global,
    /// Each sidebar owns its panel visibility, as before the shared
    /// default: every instance starts hidden and `t` only affects it.
    Local,
}

impl TodoScope {
    /// Parse a tmux-option label into a [`TodoScope`]. Unknown values
    /// fall back to the shared default.
    pub fn from_label(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "local" | "session" | "window" | "per-session" => Self::Local,
            _ => Self::Global,
        }
    }

    pub fn as_label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Local => "local",
        }
    }
}

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
    /// Hide completed TODO items in the manual TODO panel. Toggleable at
    /// runtime with `H` while the panel has focus; shared across open
    /// sidebars via `@sidebar_todo_hide_completed` and seeded by
    /// `@sidebar_default_todo_hide_completed` (default on).
    pub todo_hide_completed: bool,
    /// Whether the manual TODO panel is open. Shared across open sidebars
    /// via `@sidebar_todo_visible` when [`Self::todo_scope`] is
    /// [`TodoScope::Global`] (the default), so a list opened in one tmux
    /// session is visible from every other one. Seeded by
    /// `@sidebar_default_todo_visible` (default off).
    pub todo_visible: bool,
    /// Scope of the TODO panel's open/closed state (`@sidebar_todo_scope`).
    pub todo_scope: TodoScope,
    /// Grouping mode for the agents panel: repo roots (default) or tmux
    /// session names. Read from `@sidebar_group_by` on every sync — set
    /// it in tmux.conf; there is no runtime toggle.
    pub group_by: GroupBy,
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
            todo_hide_completed: true,
            todo_visible: false,
            todo_scope: TodoScope::default(),
            group_by: GroupBy::default(),
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

    /// Flip the grouping mode (repo ↔ tmux session) and broadcast the
    /// new value to every open sidebar via `@sidebar_group_by`, matching
    /// the `c` / `w` toggle pattern. The caller re-groups from a fresh
    /// snapshot so the list changes immediately.
    pub fn toggle_group_by(&mut self) {
        self.group_by = self.group_by.toggled();
        if self.save_group_by() {
            self.broadcast_change();
        }
    }

    /// Save the grouping mode to tmux. Returns `true` when tmux accepted
    /// the write; on failure the next sync reverts this sidebar to the
    /// shared value.
    pub fn save_group_by(&mut self) -> bool {
        tmux::run_tmux(&[
            "set",
            "-g",
            tmux::SIDEBAR_GROUP_BY,
            self.group_by.as_label(),
        ])
        .is_some()
    }

    /// Flip the "hide completed TODOs" setting and broadcast the new
    /// value to every open sidebar via `@sidebar_todo_hide_completed`.
    pub fn toggle_todo_hide_completed(&mut self) {
        self.todo_hide_completed = !self.todo_hide_completed;
        if self.save_todo_hide_completed() {
            self.broadcast_change();
        }
    }

    /// Save the "hide completed TODOs" flag to tmux. Returns `true` when
    /// tmux accepted the write (and the caller should broadcast). On a
    /// failed write the next sync reverts this sidebar to the shared value.
    pub fn save_todo_hide_completed(&mut self) -> bool {
        tmux::run_tmux(&[
            "set",
            "-g",
            tmux::SIDEBAR_TODO_HIDE_COMPLETED,
            if self.todo_hide_completed { "1" } else { "0" },
        ])
        .is_some()
    }

    /// Persist the TODO panel's shared open state and broadcast it to
    /// every other sidebar. No-op while the scope is [`TodoScope::Local`]
    /// (the panel is per-instance then) and when the value is unchanged.
    pub fn set_todo_visible(&mut self, visible: bool) {
        if self.todo_scope != TodoScope::Global || self.todo_visible == visible {
            return;
        }
        self.todo_visible = visible;
        if self.save_todo_visible() {
            self.broadcast_change();
        }
    }

    /// Save the shared TODO panel visibility to tmux. Returns `true` when
    /// tmux accepted the write (and the caller should broadcast). On a
    /// failed write the next sync reverts this sidebar to the shared
    /// value, matching the other shared toggles.
    pub fn save_todo_visible(&mut self) -> bool {
        tmux::run_tmux(&[
            "set",
            "-g",
            tmux::SIDEBAR_TODO_VISIBLE,
            if self.todo_visible { "1" } else { "0" },
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

    /// Parse `@sidebar_default_todo_hide_completed` into the landing
    /// "hide completed TODOs" mode for a newly opened sidebar. Same
    /// accepted values as [`Self::default_compact_view_from_options`],
    /// but unset means `true`: the default is to hide finished items.
    pub fn default_todo_hide_completed_from_options(opts: &HashMap<String, String>) -> bool {
        opts.get(tmux::SIDEBAR_DEFAULT_TODO_HIDE_COMPLETED)
            .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "on" | "true" | "1"))
            .unwrap_or(true)
    }

    /// Parse `@sidebar_default_todo_visible` into the landing panel
    /// visibility for a newly opened sidebar. Same accepted values as
    /// [`Self::default_compact_view_from_options`], but unset means
    /// `false`: the panel starts hidden until a sidebar opens it.
    pub fn default_todo_visible_from_options(opts: &HashMap<String, String>) -> bool {
        opts.get(tmux::SIDEBAR_DEFAULT_TODO_VISIBLE)
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
        if !opts.contains_key(tmux::SIDEBAR_TODO_HIDE_COMPLETED) {
            self.todo_hide_completed = Self::default_todo_hide_completed_from_options(opts);
        }
        if self.todo_scope == TodoScope::Global && !opts.contains_key(tmux::SIDEBAR_TODO_VISIBLE) {
            self.todo_visible = Self::default_todo_visible_from_options(opts);
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
        if let Some(hide_completed_str) = opts.get(tmux::SIDEBAR_TODO_HIDE_COMPLETED) {
            self.todo_hide_completed = hide_completed_str.trim() == "1";
        }
        if let Some(scope_str) = opts.get(tmux::SIDEBAR_TODO_SCOPE) {
            self.todo_scope = TodoScope::from_label(scope_str);
        }
        if let Some(visible_str) = opts.get(tmux::SIDEBAR_TODO_VISIBLE) {
            self.todo_visible = visible_str.trim() == "1";
        }
        if let Some(group_by_str) = opts.get(tmux::SIDEBAR_GROUP_BY) {
            self.group_by = GroupBy::from_label(group_by_str);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn todo_scope_labels_round_trip() {
        assert_eq!(TodoScope::from_label("global"), TodoScope::Global);
        assert_eq!(TodoScope::from_label(" LOCAL "), TodoScope::Local);
        assert_eq!(
            TodoScope::from_label("nonsense"),
            TodoScope::Global,
            "unknown labels fall back to the shared default"
        );
        assert_eq!(TodoScope::Global.as_label(), "global");
        assert_eq!(TodoScope::Local.as_label(), "local");
    }

    #[test]
    fn apply_all_reads_shared_todo_visibility() {
        let mut state = GlobalState::default();
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_TODO_SCOPE.to_string(), "local".into());
        opts.insert(tmux::SIDEBAR_TODO_VISIBLE.to_string(), "1".into());

        state.apply_all(&opts);

        assert_eq!(state.todo_scope, TodoScope::Local);
        assert!(state.todo_visible);
    }

    #[test]
    fn default_todo_visibility_is_off_unless_configured() {
        assert!(!GlobalState::default_todo_visible_from_options(
            &HashMap::new()
        ));

        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_DEFAULT_TODO_VISIBLE.to_string(), "on".into());
        assert!(GlobalState::default_todo_visible_from_options(&opts));
    }

    #[test]
    fn apply_default_view_seeds_shared_visibility() {
        let mut state = GlobalState::default();
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_DEFAULT_TODO_VISIBLE.to_string(), "on".into());

        state.apply_default_view(&opts);

        assert!(state.todo_visible);
    }

    #[test]
    fn local_scope_ignores_the_landing_visibility_default() {
        let mut state = GlobalState {
            todo_scope: TodoScope::Local,
            ..GlobalState::default()
        };
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_DEFAULT_TODO_VISIBLE.to_string(), "on".into());

        state.apply_default_view(&opts);

        assert!(!state.todo_visible);
    }
}
