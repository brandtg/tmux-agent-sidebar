use std::collections::HashMap;
use std::time::Instant;

use crate::tmux;

use super::filter::RepoFilter;

/// Default agent message preview line cap (used until tmux.conf says
/// otherwise). Two lines keep the panel compact; `@sidebar_preview_lines`
/// restores the older three-line preview.
pub const DEFAULT_PREVIEW_LINES: usize = 2;
/// Upper bound for `@sidebar_preview_lines`. Generous enough for a
/// "show me the whole message" config, tight enough that a typo like
/// `1000` cannot push every other row out of the panel.
pub const MAX_PREVIEW_LINES: usize = 10;
/// Lower bound for `@sidebar_preview_lines`: 0 would render no preview
/// at all, which no config ever wants.
pub const MIN_PREVIEW_LINES: usize = 1;

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
    /// `@sidebar_default_todo_hide_completed` (default off: completed
    /// items stay visible, struck through).
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
    /// Group names currently folded in the agents panel (`z`, a header
    /// click, `Z`). Persisted to `@sidebar_collapsed_groups` and synced
    /// across open sidebars like the other shared options, so folds
    /// survive restarts and follow the user everywhere. Cleared when the
    /// grouping mode changes: the names are mode-specific, and a session
    /// that shares a name with a repo would otherwise inherit its fold.
    pub collapsed_groups: std::collections::HashSet<String>,
    /// Session-mode window level (`@sidebar_session_windows`). When on
    /// and grouped by session, each window's rows render under a muted
    /// `index:name` header — the `Session > Window > agent|process`
    /// hierarchy. Set it in tmux.conf; like `group_by` it is read on
    /// every sync and has no runtime toggle.
    pub session_windows: bool,
    /// Repo-mode window level (`@sidebar_worktree_windows`). The
    /// worktree/repo counterpart of `session_windows`: when on and
    /// grouped by repo, each window's rows render under a muted
    /// `index:name` header — the `Repo > Window > agent|process`
    /// hierarchy. Set it in tmux.conf; like `group_by` it is read on
    /// every sync and has no runtime toggle.
    pub worktree_windows: bool,
    /// Worktree row titles (`@sidebar_worktree_titles`): an agent row
    /// without a user-given session name is titled by its linked worktree.
    /// Read on every sync; no runtime toggle.
    pub worktree_titles: bool,
    /// Line cap for the agent message preview (the prompt / response
    /// body under the status row). Read from `@sidebar_preview_lines` on
    /// every sync — set it in tmux.conf; there is no runtime toggle.
    /// Clamped to [`MIN_PREVIEW_LINES`]..=[`MAX_PREVIEW_LINES`] so a
    /// fat-fingered config cannot erase the preview or flood the panel.
    pub preview_lines: usize,
    /// Pane id of the sidebar instance owning this state. Used to skip
    /// self-signaling when broadcasting shared-option changes.
    owner_pane: String,
    /// Last cursor value successfully written to tmux.
    last_saved_cursor: usize,
    /// Last repo filter value successfully written to tmux.
    last_saved_repo_filter: RepoFilter,
    /// Last collapsed-group set successfully written to tmux.
    last_saved_collapsed_groups: Option<std::collections::HashSet<String>>,
    /// Last collapsed-group set parsed from tmux on a sync, whether or
    /// not it was adopted. Tells the group-by-change observer whether the
    /// shared option still carries the previous mode's names (the
    /// changer's clear did not land; a repair write is wanted) or is
    /// already cleared (stamp locally, no spawn — audit 2026-11-21 F-3).
    last_observed_collapsed_groups: Option<std::collections::HashSet<String>>,
    /// When the selected cursor was last changed and still needs persisting.
    pending_cursor_save_since: Option<Instant>,
    /// When the collapsed set was last changed and still needs persisting.
    /// The queue is what keeps a transient tmux failure from silently
    /// reverting a fold (audit 2026-11-21 F-4): the flush keeps the
    /// marker on a failed write and retries, while the revert anchor
    /// (`last_saved_collapsed_groups` stamped only on success) stays.
    pending_collapsed_save_since: Option<Instant>,
    /// A stale repo filter that `rebuild_row_targets` reset locally but
    /// whose shared write has not landed in tmux yet. The flush belongs
    /// to the shared-state owners — the per-snapshot fold and the event
    /// loop's shared-option reload — not to the target-list builder
    /// (audit 2026-11-21 F-2).
    repo_filter_reset_pending: bool,
}

impl Default for GlobalState {
    fn default() -> Self {
        Self::new(String::new())
    }
}

impl GlobalState {
    pub fn new(owner_pane: String) -> Self {
        Self {
            selected_pane_row: 0,
            repo_filter: RepoFilter::All,
            compact: false,
            show_windows: false,
            // Google Tasks shows completed items at the bottom, struck
            // through; `H` (or `@sidebar_default_todo_hide_completed`)
            // opts into the collapsed view.
            todo_hide_completed: false,
            todo_visible: false,
            todo_scope: TodoScope::default(),
            group_by: GroupBy::default(),
            collapsed_groups: std::collections::HashSet::new(),
            session_windows: false,
            worktree_windows: false,
            worktree_titles: false,
            preview_lines: DEFAULT_PREVIEW_LINES,
            owner_pane,
            last_saved_cursor: 0,
            last_saved_repo_filter: RepoFilter::All,
            last_saved_collapsed_groups: None,
            last_observed_collapsed_groups: None,
            pending_cursor_save_since: None,
            pending_collapsed_save_since: None,
            repo_filter_reset_pending: false,
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

    /// Record that a stale repo filter was reset locally by
    /// [`AppState::rebuild_row_targets`]; the shared write is deferred to
    /// the next [`Self::flush_pending_repo_filter_reset`] on a tick owned
    /// by the shared-state paths.
    pub fn mark_repo_filter_reset_pending(&mut self) {
        self.repo_filter_reset_pending = true;
    }

    /// Whether a stale repo-filter reset is awaiting its shared write.
    pub fn has_pending_repo_filter_reset(&self) -> bool {
        self.repo_filter_reset_pending
    }

    /// Persist a stale repo filter reset recorded by
    /// [`Self::mark_repo_filter_reset_pending`]. Called from the snapshot
    /// fold and the event loop's shared-option reload — the two paths
    /// that own shared state — so the write runs at most once per
    /// reset-detecting rebuild rather than once per rebuild, and one
    /// instance's momentarily-empty grouping cannot stampede every other
    /// sidebar with per-tick `set -g` spawns (audit 2026-11-21 F-2).
    pub fn flush_pending_repo_filter_reset(&mut self) {
        if !self.repo_filter_reset_pending {
            return;
        }
        self.repo_filter_reset_pending = false;
        self.save_repo_filter();
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

    /// Save the collapsed-group set to tmux as a JSON array, then
    /// broadcast the change to every open sidebar. Returns `true` when
    /// tmux accepted the write so callers can decide whether to clear a
    /// queued save or keep retrying. A failed write leaves
    /// `last_saved_collapsed_groups` untouched so the next sync reverts
    /// this sidebar to the shared value instead of diverging.
    pub fn save_collapsed_groups(&mut self) -> bool {
        let Ok(json) = serde_json::to_string(&self.collapsed_groups) else {
            return false;
        };
        if tmux::run_tmux(&["set", "-g", tmux::SIDEBAR_COLLAPSED_GROUPS, &json]).is_some() {
            self.last_saved_collapsed_groups = Some(self.collapsed_groups.clone());
            self.broadcast_change();
            true
        } else {
            false
        }
    }

    /// Mark the collapsed set as dirty so the main loop persists it once
    /// the user pauses folding — the same debounce-and-retry policy the
    /// cursor queue uses (audit 2026-11-21 F-4).
    pub fn queue_collapsed_save(&mut self) {
        self.pending_collapsed_save_since = Some(Instant::now());
    }

    /// Persist a queued collapsed-set update after it has been idle for
    /// at least the requested debounce duration. Returns true when the
    /// queue was consumed.
    pub fn flush_pending_collapsed_save(&mut self, debounce: std::time::Duration) -> bool {
        let Some(queued_at) = self.pending_collapsed_save_since else {
            return false;
        };
        if queued_at.elapsed() < debounce {
            return false;
        }
        // A sync between the toggle and this flush may have reconciled
        // the set — adopting the shared value also stamps the marker — in
        // which case there is nothing left to write and the retry must
        // not echo the shared set straight back at it.
        if self.last_saved_collapsed_groups.as_ref() == Some(&self.collapsed_groups) {
            self.pending_collapsed_save_since = None;
            return true;
        }
        // Only clear the pending marker on successful tmux write —
        // otherwise a transient failure would silently drop the queued
        // save instead of retrying on the next flush tick.
        if self.save_collapsed_groups() {
            self.pending_collapsed_save_since = None;
            true
        } else {
            false
        }
    }

    /// Clear the collapsed set locally *and* persist the cleared set to
    /// the shared option. Used by the instance that *changes* the grouping
    /// mode (the `s` key), and by an observer that saw the mode change but
    /// found the shared option still carrying the previous mode's names —
    /// the changer's clear did not land, and the first observer whose
    /// write succeeds repairs it. Without that repair the echo guard would
    /// compare tmux's (stale-mode) value against a set this sidebar no
    /// longer reconciles with, letting the old names fold back in later.
    /// An observer whose sync already shows the cleared set uses
    /// [`Self::clear_collapsed_groups_local`] instead — every instance
    /// writing the same empty set is the per-press stampede that audit
    /// 2026-11-21 F-3 calls out.
    pub fn persist_cleared_collapsed_groups(&mut self) {
        self.collapsed_groups.clear();
        self.save_collapsed_groups();
    }

    /// Clear the collapsed set locally and stamp the write marker (and
    /// the observed-value record) to match the already-cleared shared
    /// option — no spawn, no broadcast. This is the observer's half of a
    /// group-by change when tmux already holds the cleared set the
    /// changer wrote: the echo guard now matches the shared value, so the
    /// old names cannot fold back in, and nothing needs to be written.
    pub fn clear_collapsed_groups_local(&mut self) {
        self.collapsed_groups.clear();
        self.last_saved_collapsed_groups = Some(std::collections::HashSet::new());
        self.last_observed_collapsed_groups = Some(std::collections::HashSet::new());
    }

    /// Whether the last shared collapsed set seen on a sync was a
    /// non-empty (previous-mode) fold — the condition under which an
    /// observer of a group-by change must write the clear rather than
    /// just stamp it locally.
    pub fn shared_collapsed_still_folded(&self) -> bool {
        self.last_observed_collapsed_groups
            .as_ref()
            .is_some_and(|names| !names.is_empty())
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

    /// Load all global state from tmux variables, including the shared
    /// cursor row. Startup-only: restores where the user last navigated.
    /// The runtime fold must use
    /// [`Self::apply_shared_options_without_cursor`] /
    /// [`Self::reload_shared_options_without_cursor`] instead — see there.
    pub fn load_from_tmux(&mut self) {
        let opts = tmux::get_all_global_options();
        self.apply_all(&opts);
    }

    /// Runtime reload of the shared options straight from tmux — the
    /// direct-spawn twin of the worker-fetched fold in
    /// [`Self::apply_shared_options_without_cursor`], used when a window
    /// regains visibility without a globals-bearing snapshot in hand.
    /// Never adopts the shared cursor (see there for why). Returns
    /// whether tmux actually answered: a failed `show -g` reports false
    /// so the caller does not restart the resync floor on a sync that
    /// synced nothing (audit 2026-11-21 F-7).
    pub fn reload_shared_options_without_cursor(&mut self) -> bool {
        let Some(opts) = tmux::try_get_all_global_options() else {
            return false;
        };
        self.apply_shared_options_without_cursor(&opts);
        true
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
            // Google Tasks shows completed items struck at the bottom;
            // collapsing is the opt-in.
            .unwrap_or(false)
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

    /// Land on the configured defaults instead of restoring whatever a
    /// previous sidebar persisted.
    ///
    /// A freshly opened sidebar must not resume a value some other
    /// window set earlier — a random-looking landing state. Called once
    /// at startup, after `load_from_tmux`.
    ///
    /// For `compact` the landing default applies only while no live
    /// `@sidebar_compact` value exists: once any sidebar has toggled,
    /// the shared value wins so every sidebar renders the same density.
    /// The defaults are never written back to tmux here — opening a
    /// sidebar must not yank the view out from under already-open ones.
    pub fn apply_default_view(&mut self, opts: &HashMap<String, String>) {
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

    /// Apply all global options from tmux (cursor, repo filter, etc.).
    ///
    /// The startup/full variant: the shared cursor row rides along so a
    /// fresh sidebar lands where the user last navigated, except that a
    /// startup `select_focused_context_row` pass (`app::setup`) overrides
    /// it with this window's context pane when that pane is in the list.
    /// The per-tick
    /// snapshot fold must use
    /// [`Self::apply_shared_options_without_cursor`] instead — at runtime
    /// the cursor is owned by whoever holds focus, and an index written
    /// by another window's sidebar is meaningless in this one's list.
    pub fn apply_all(&mut self, opts: &HashMap<String, String>) {
        self.apply_options(opts, true);
    }

    /// Runtime variant of [`Self::apply_all`] used by the event loop's
    /// snapshot fold: syncs every shared view setting but leaves the
    /// cursor row alone. An unfocused sidebar zeroes its cursor right
    /// after the fold (the re-anchor rule), and a focused one is being
    /// navigated — adoption in either state is at best erased and at
    /// worst drags the visible selection highlight onto an unrelated
    /// agent while the focus flag is momentarily stale.
    pub fn apply_shared_options_without_cursor(&mut self, opts: &HashMap<String, String>) {
        self.apply_options(opts, false);
    }

    fn apply_options(&mut self, opts: &HashMap<String, String>, adopt_cursor: bool) {
        if adopt_cursor
            && let Some(cursor_str) = opts.get(tmux::SIDEBAR_CURSOR)
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
        if let Some(session_windows_str) = opts.get(tmux::SIDEBAR_SESSION_WINDOWS) {
            // Same accepted labels as the landing defaults (`on`/`true`/
            // `1`, case-insensitive); unset means off, so the seeding
            // `if -F` in agent-sidebar.conf is the only writer of `off`.
            self.session_windows = matches!(
                session_windows_str.trim().to_ascii_lowercase().as_str(),
                "on" | "true" | "1"
            );
        }
        if let Some(worktree_windows_str) = opts.get(tmux::SIDEBAR_WORKTREE_WINDOWS) {
            // Same accepted labels as `session_windows`; unset means off,
            // so the seeding `if -F` in agent-sidebar.conf is the only
            // writer of `off`.
            self.worktree_windows = matches!(
                worktree_windows_str.trim().to_ascii_lowercase().as_str(),
                "on" | "true" | "1"
            );
        }
        if let Some(worktree_titles_str) = opts.get(tmux::SIDEBAR_WORKTREE_TITLES) {
            self.worktree_titles = matches!(
                worktree_titles_str.trim().to_ascii_lowercase().as_str(),
                "on" | "true" | "1"
            );
        }
        if let Some(preview_str) = opts.get(tmux::SIDEBAR_PREVIEW_LINES)
            && let Some(n) = parse_preview_lines(preview_str)
        {
            self.preview_lines = n;
        }
        if let Some(collapsed_str) = opts.get(tmux::SIDEBAR_COLLAPSED_GROUPS)
            && let Ok(names) =
                serde_json::from_str::<std::collections::HashSet<String>>(collapsed_str)
        {
            // Recorded before the echo guard: the group-by-change branch
            // in the event loop asks whether the *shared* option still
            // holds the previous mode's names, regardless of what this
            // instance adopted.
            self.last_observed_collapsed_groups = Some(names.clone());
            if Some(&names) != self.last_saved_collapsed_groups.as_ref() {
                // A set this sidebar itself just wrote equals its
                // `last_saved` copy and is skipped (the same echo-guard
                // pattern `last_saved_cursor` uses); anything else
                // (another sidebar's fold, a restored value at startup)
                // is adopted wholesale, and the marker follows so the
                // next sync treats the shared value as known.
                self.collapsed_groups = names.clone();
                self.last_saved_collapsed_groups = Some(names);
            }
        }
    }
}

/// Parse `@sidebar_preview_lines`. Accepts a plain integer and clamps it
/// to [`MIN_PREVIEW_LINES`]..=[`MAX_PREVIEW_LINES`]; `None` for unset or
/// unparseable values, which keeps the current/default cap.
fn parse_preview_lines(s: &str) -> Option<usize> {
    let n = s.trim().parse::<usize>().ok()?;
    Some(n.clamp(MIN_PREVIEW_LINES, MAX_PREVIEW_LINES))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn persist_cleared_collapsed_groups_unfolds_locally() {
        let mut global = GlobalState::new("%99".into());
        let mut opts = HashMap::new();
        opts.insert(
            tmux::SIDEBAR_COLLAPSED_GROUPS.to_string(),
            r#"["backend"]"#.to_string(),
        );
        global.apply_all(&opts);
        assert!(global.collapsed_groups.contains("backend"));

        // The tmux write cannot succeed under test (the IPC gate refuses
        // every spawn), so this pins the local half of the contract: the
        // fold is dropped, and a failed write must not touch the echo
        // guard — the next sync still reverts to the shared value rather
        // than silently claiming the clear.
        global.persist_cleared_collapsed_groups();
        assert!(global.collapsed_groups.is_empty());
        assert_eq!(
            global.last_saved_collapsed_groups.as_ref(),
            Some(&["backend".to_string()].into_iter().collect()),
            "a failed write leaves last_saved as the revert anchor"
        );
    }

    #[test]
    fn clear_collapsed_groups_local_stamps_the_echo_guard() {
        // Audit 2026-11-21 F-3: the observer of a group-by change clears
        // folds and stamps the marker to match the already-cleared shared
        // option — no write, and the old fold does not come back.
        let mut global = GlobalState::new("%99".into());
        let mut opts = HashMap::new();
        opts.insert(
            tmux::SIDEBAR_COLLAPSED_GROUPS.to_string(),
            r#"["backend"]"#.to_string(),
        );
        global.apply_all(&opts);
        assert!(global.collapsed_groups.contains("backend"));
        assert!(global.shared_collapsed_still_folded());

        global.clear_collapsed_groups_local();
        assert!(global.collapsed_groups.is_empty());
        assert_eq!(
            global.last_saved_collapsed_groups,
            Some(std::collections::HashSet::new()),
            "the marker is stamped as if the clear had been written"
        );
        assert!(!global.shared_collapsed_still_folded());

        // The shared cleared set now matches the marker: the echo guard
        // skips it and nothing re-adopts the fold.
        let mut cleared = HashMap::new();
        cleared.insert(tmux::SIDEBAR_COLLAPSED_GROUPS.to_string(), "[]".to_string());
        global.apply_all(&cleared);
        assert!(global.collapsed_groups.is_empty());
    }

    #[test]
    fn queued_collapsed_save_retries_after_a_failed_write() {
        // Audit 2026-11-21 F-4: a fold toggle queues its write, and the
        // flush keeps the pending marker when tmux refuses — the retry
        // policy that stops a transient failure from silently dropping
        // the fold (the cursor queue's contract, now shared).
        let mut global = GlobalState::new("%99".into());
        global.collapsed_groups.insert("backend".to_string());
        global.queue_collapsed_save();

        // The tmux write cannot succeed under test (the IPC gate refuses
        // every spawn), so the marker must survive the flush attempt.
        assert!(!global.flush_pending_collapsed_save(std::time::Duration::ZERO));
        assert!(global.pending_collapsed_save_since.is_some());
    }

    #[test]
    fn queued_collapsed_save_is_dropped_when_a_sync_reconciles_it() {
        let mut global = GlobalState::new("%99".into());
        global.collapsed_groups.insert("backend".to_string());
        global.queue_collapsed_save();

        // The same set adopted from the shared option stamps the marker,
        // so the queued toggle has nothing left to persist — and the
        // retry must not fire an echo write (and broadcast) back.
        let mut opts = HashMap::new();
        opts.insert(
            tmux::SIDEBAR_COLLAPSED_GROUPS.to_string(),
            r#"["backend"]"#.to_string(),
        );
        global.apply_all(&opts);

        assert!(global.flush_pending_collapsed_save(std::time::Duration::ZERO));
        assert!(global.pending_collapsed_save_since.is_none());
    }

    #[test]
    fn shared_collapsed_still_folded_tracks_the_last_observed_set() {
        // The observer's repair-write condition: only a shared option that
        // still carries names is "folded"; unset or empty means the
        // changer's clear landed and a local stamp is enough.
        let mut global = GlobalState::new("%99".into());
        assert!(
            !global.shared_collapsed_still_folded(),
            "never having observed the option reads as cleared"
        );

        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_COLLAPSED_GROUPS.to_string(), "[]".to_string());
        global.apply_all(&opts);
        assert!(!global.shared_collapsed_still_folded());

        opts.insert(
            tmux::SIDEBAR_COLLAPSED_GROUPS.to_string(),
            r#"["stale-mode"]"#.to_string(),
        );
        global.apply_all(&opts);
        assert!(
            global.shared_collapsed_still_folded(),
            "a shared set the changer failed to clear is detected for the repair write"
        );
    }

    #[test]
    fn collapsed_groups_round_trip_through_the_shared_option() {
        let mut global = GlobalState::new("%99".into());
        let mut opts = HashMap::new();
        opts.insert(
            tmux::SIDEBAR_COLLAPSED_GROUPS.to_string(),
            r#"["backend","⎇ fix (repo)"]"#.to_string(),
        );
        global.apply_all(&opts);
        assert_eq!(
            global.collapsed_groups,
            ["backend", "⎇ fix (repo)"]
                .map(str::to_string)
                .into_iter()
                .collect::<std::collections::HashSet<_>>(),
            "the persisted JSON set is adopted wholesale"
        );

        // A value that fails to parse leaves the adopted set alone
        // instead of silently unfolding everything.
        opts.insert(
            tmux::SIDEBAR_COLLAPSED_GROUPS.to_string(),
            "not json".to_string(),
        );
        global.apply_all(&opts);
        assert_eq!(global.collapsed_groups.len(), 2);
    }

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
    fn preview_lines_default_is_two() {
        assert_eq!(GlobalState::default().preview_lines, 2);
        assert_eq!(GlobalState::new(String::new()).preview_lines, 2);
    }

    #[test]
    fn apply_all_adopts_the_shared_cursor() {
        let mut state = GlobalState::default();
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_CURSOR.to_string(), "5".into());

        state.apply_all(&opts);

        assert_eq!(state.selected_pane_row, 5);
        assert_eq!(state.last_saved_cursor, 5);
    }

    #[test]
    fn apply_shared_options_without_cursor_leaves_the_cursor_row_alone() {
        // The user is mid-navigation in this sidebar (cursor on row 2);
        // another window's sidebar flushed its own row 5 to the shared
        // option. The runtime fold must not drag the highlight there —
        // an index is only meaningful in the list it was written against.
        let mut state = GlobalState {
            selected_pane_row: 2,
            last_saved_cursor: 2,
            ..Default::default()
        };
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_CURSOR.to_string(), "5".into());
        opts.insert(tmux::SIDEBAR_PREVIEW_LINES.to_string(), "7".into());

        state.apply_shared_options_without_cursor(&opts);

        assert_eq!(
            state.selected_pane_row, 2,
            "the foreign index must not move the local cursor"
        );
        assert_eq!(
            state.last_saved_cursor, 2,
            "the skipped adoption must not poison the echo guard"
        );
        assert_eq!(
            state.preview_lines, 7,
            "the other shared settings still fold through"
        );
    }

    #[test]
    fn apply_all_reads_preview_lines() {
        let mut state = GlobalState::default();
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_PREVIEW_LINES.to_string(), "3".into());

        state.apply_all(&opts);

        assert_eq!(state.preview_lines, 3);
    }

    #[test]
    fn preview_lines_clamps_to_sane_bounds() {
        let mut state = GlobalState::default();
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_PREVIEW_LINES.to_string(), "0".into());
        state.apply_all(&opts);
        assert_eq!(state.preview_lines, MIN_PREVIEW_LINES);

        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_PREVIEW_LINES.to_string(), "1000".into());
        state.apply_all(&opts);
        assert_eq!(state.preview_lines, MAX_PREVIEW_LINES);
    }

    #[test]
    fn preview_lines_ignores_unparseable_values() {
        let mut state = GlobalState::default();
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_PREVIEW_LINES.to_string(), "many".into());

        state.apply_all(&opts);

        assert_eq!(state.preview_lines, DEFAULT_PREVIEW_LINES);
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
    fn apply_all_reads_session_window_level() {
        // The session-mode window level is a config option like
        // `group_by`: read on every sync, no runtime toggle. Unset
        // keeps the default off; `on`/`true`/`1` enable it.
        let mut state = GlobalState::default();
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_SESSION_WINDOWS.to_string(), "on".into());
        state.apply_all(&opts);
        assert!(state.session_windows);

        let mut state = GlobalState::default();
        let opts = HashMap::new();
        state.apply_all(&opts);
        assert!(!state.session_windows, "unset falls back to off");

        let mut state = GlobalState::default();
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_SESSION_WINDOWS.to_string(), "off".into());
        state.apply_all(&opts);
        assert!(!state.session_windows);
    }

    #[test]
    fn apply_all_reads_worktree_window_level() {
        // The repo-mode window level mirrors `session_windows`: a config
        // option read on every sync, no runtime toggle. Unset keeps the
        // default off; `on`/`true`/`1` enable it.
        let mut state = GlobalState::default();
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_WORKTREE_WINDOWS.to_string(), "on".into());
        state.apply_all(&opts);
        assert!(state.worktree_windows);

        let mut state = GlobalState::default();
        let opts = HashMap::new();
        state.apply_all(&opts);
        assert!(!state.worktree_windows, "unset falls back to off");

        let mut state = GlobalState::default();
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_WORKTREE_WINDOWS.to_string(), "off".into());
        state.apply_all(&opts);
        assert!(!state.worktree_windows);
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
