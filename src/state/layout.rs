use super::{AppState, Focus, RepoFilter};
use crate::tmux::PaneAttention;

/// Columns reserved at the header row's left edge for the `☰`
/// overview-grid button: the glyph itself is East-Asian-Wide (two cells
/// per unicode-width) plus one trailing space. Rendered — and
/// clickable — in every header state (toolbar, search input, chyron
/// banner). See [`AppState::spawn_overview_popup`].
pub const OVERVIEW_BUTTON_WIDTH: u16 = 3;

#[derive(Debug, Clone)]
pub struct RowTarget {
    pub pane_id: String,
    /// `true` when this row is a non-agent window rather than an agent
    /// pane. `pane_id` is the window's active pane either way. Used by
    /// activation to jump without summoning a sidebar into the window.
    pub is_window: bool,
}

/// One visible group header in the rendered list: the group's name and
/// the index of its first selectable row in `pane_row_targets`. Used by
/// `[` / `]` group jumps and by collapse (a collapsed group has no rows,
/// so it is not recorded here).
#[derive(Debug, Clone)]
pub struct GroupStart {
    pub name: String,
    pub row_start: usize,
}

/// Click target for the `+` button rendered at the right edge of each
/// repo-group header in the agents panel. Clicking it opens the spawn
/// modal prefilled for that repo. `session_name` is the tmux session
/// the group's panes live in (the focused pane's session, else the
/// first pane's) — the spawn must create its window in the group's
/// session, not the sidebar's.
#[derive(Debug, Clone)]
pub struct RepoSpawnTarget {
    pub rect: ratatui::layout::Rect,
    pub repo_name: String,
    pub repo_root: String,
    pub session_name: String,
}

/// Click target for the red `×` rendered next to the branch of a
/// sidebar-spawned pane. Clicking it opens the close-pane confirmation
/// for that specific pane.
#[derive(Debug, Clone)]
pub struct SpawnRemoveTarget {
    pub rect: ratatui::layout::Rect,
    pub pane_id: String,
}

/// Click target for a group header row in the agents panel. The whole
/// row folds/unfolds the group; the `+` spawn button and `×` remove
/// marker are matched first in `handle_mouse_click` and win the click
/// when their slice of the row is hit.
#[derive(Debug, Clone)]
pub struct GroupHeaderTarget {
    pub rect: ratatui::layout::Rect,
    pub group_name: String,
}

/// Click target for a window header row (`index:name`) rendered under
/// the group title when the session-mode `@sidebar_session_windows` or
/// its repo-mode counterpart `@sidebar_worktree_windows` is on. The
/// whole row jumps the tmux client to that window.
#[derive(Debug, Clone)]
pub struct WindowHeaderTarget {
    pub rect: ratatui::layout::Rect,
    pub session_name: String,
    pub window_index: i64,
}

/// Click target for one rendered TODO item. `rect` spans every wrapped
/// line of the item for hit-testing. A click on `checkbox_rect` (the
/// marker column of the item's first visible line) toggles the
/// checkbox; a click on the rest of `rect` selects the item and — when
/// it is already selected — opens the editor (Google Tasks'
/// click-the-text-to-edit convention).
#[derive(Debug, Clone)]
pub struct TodoRowTarget {
    pub rect: ratatui::layout::Rect,
    /// `None` when the item's first line is scrolled out of view, in
    /// which case every visible line is the text target.
    pub checkbox_rect: Option<ratatui::layout::Rect>,
    pub index: usize,
}

/// Screen-positioned hyperlink overlay for OSC 8 terminal hyperlinks.
#[derive(Debug, Clone)]
pub struct HyperlinkOverlay {
    pub x: u16,
    pub y: u16,
    pub text: String,
    pub url: String,
}

/// Ephemeral render output cached for click hit-testing.
///
/// Every field here is **rewritten on every frame** by the UI layer and
/// only read by event handlers (mouse/keyboard) before the next render.
/// Bundling them under `state.layout` makes the "frame-scoped vs
/// persistent state" boundary visible at a glance, since the rest of
/// `AppState` only holds data that survives across frames.
#[derive(Debug, Clone, Default)]
pub struct FrameLayout {
    /// Filtered pane list, in the order the UI rendered them. Index
    /// matches `GlobalState::selected_pane_row`.
    pub pane_row_targets: Vec<RowTarget>,
    /// Visible group headers in render order, with the first selectable
    /// row index of each. Collapsed groups contribute no rows and are
    /// therefore absent. Used by `[` / `]` and collapse.
    pub group_starts: Vec<GroupStart>,
    /// Maps each rendered text line in the agents panel back to a row in
    /// `pane_row_targets`. `None` for header/blank lines that should not
    /// route clicks to a pane.
    pub line_to_row: Vec<Option<usize>>,
    /// X column of the merged menu button in the header row. `None`
    /// before the first render. Used for click hit-testing.
    pub menu_button_col: Option<u16>,
    /// Screen row of the single header row (toolbar, or the chyron
    /// while it overlays the toolbar). Defaults to 0 for callers that
    /// have not rendered a frame yet.
    pub toolbar_row: u16,
    /// Width of the header row as of the last render. The chyron's
    /// ticker math needs it (the display window depends on how much
    /// room the banner has); `None` before the first frame.
    pub header_width: Option<u16>,
    /// Click regions for the `[+]` spawn button rendered at the right
    /// edge of each repo-group header. One entry per visible repo group.
    pub repo_spawn_targets: Vec<RepoSpawnTarget>,
    /// Click regions for the red `×` remove marker rendered next to the
    /// branch of each sidebar-spawned pane. One entry per visible row.
    pub spawn_remove_targets: Vec<SpawnRemoveTarget>,
    /// Click regions for group header rows: clicking anywhere on a
    /// header toggles that group's collapsed state. One entry per
    /// rendered header, expanded or collapsed.
    pub group_header_targets: Vec<GroupHeaderTarget>,
    /// Click regions for window header rows (`index:name` under a
    /// group title): clicking anywhere on one switches the tmux client
    /// to that window. One entry per rendered header.
    pub window_header_targets: Vec<WindowHeaderTarget>,
    /// OSC 8 hyperlink overlays the main loop writes after each frame so
    /// terminals can recognise PR numbers as clickable links.
    pub hyperlink_overlays: Vec<HyperlinkOverlay>,
    /// Click regions for rendered TODO items, one per visible item.
    pub todo_row_targets: Vec<TodoRowTarget>,
    /// Click region for the inline "add TODO" input line, when open.
    pub todo_input_rect: Option<ratatui::layout::Rect>,
    /// The TODO panel's on-screen area (zero-height when hidden). Used to
    /// route mouse-wheel scrolling to the panel instead of the agent list.
    pub todo_area: ratatui::layout::Rect,
    /// The flash banner's on-screen rect, recorded by the render pass on
    /// every frame where one is painted. The banner `Clear`s and repaints
    /// its row, so click targets underneath must go inert for the
    /// banner's lifetime (audit 2026-11-19 B2). `None` when the last
    /// frame painted no banner.
    pub flash_rect: Option<ratatui::layout::Rect>,
}

pub(super) fn point_in_rect(row: u16, col: u16, rect: ratatui::layout::Rect) -> bool {
    rect.contains(ratatui::layout::Position { x: col, y: row })
}

impl AppState {
    pub fn rebuild_row_targets(&mut self) {
        // Reset stale repo filter if the repo no longer exists. The local
        // reset must happen here — the list this frame renders is wrong
        // otherwise — but the shared write is deferred to the snapshot
        // fold / reload branch (see `flush_pending_repo_filter_reset`):
        // this builder also runs from per-keystroke paths, and a tmux
        // write from here lets one instance's momentarily-empty grouping
        // flip the shared filter for everyone, per rebuild (audit
        // 2026-11-21 F-2). Fresh sidebar instances still land clean, one
        // tick later. Uses the same match rule as the renderer
        // (`matches_group`) so a bare name typed for a `⎇`-titled
        // worktree group is not mistaken for stale.
        let stale = matches!(self.global.repo_filter, RepoFilter::Repo(_))
            && !self
                .repo_groups
                .iter()
                .any(|g| self.global.repo_filter.matches_group(&g.name));
        if stale {
            self.global.repo_filter = RepoFilter::All;
            self.global.mark_repo_filter_reset_pending();
        }

        // Remember which pane the cursor was on before the target list is
        // rebuilt, so a rebuild that still contains it can keep the cursor
        // there instead of clamping to an unrelated index.
        let previous_pane_id = self
            .layout
            .pane_row_targets
            .get(self.global.selected_pane_row)
            .map(|target| target.pane_id.clone());
        // Also remember where in its group that pane sat: when the pane
        // is gone after the rebuild, the cursor re-anchors by position
        // within the same group instead of clamping to the global last
        // row — which can be a different group entirely and made the
        // selection drift (and later snap back down when the pane
        // returned) on every row removal outside a search.
        let previous_anchor = self.current_group_index().map(|index| {
            let group = &self.layout.group_starts[index];
            (
                group.name.clone(),
                self.global.selected_pane_row - group.row_start,
            )
        });

        self.layout.pane_row_targets.clear();
        self.layout.group_starts.clear();
        // Lowered once for the whole pass, mirroring row_collector::collect
        // (audit 2026-11-19 B6).
        let query_lower = self.search.lowercased_query();
        // Single source for the four row-index rules, shared with
        // `row_collector::collect` (audit 2026-11-21 F-10).
        let plan_ctx = crate::group::RowPlanContext {
            repo_filter: &self.global.repo_filter,
            collapsed_groups: &self.global.collapsed_groups,
            group_by: self.global.group_by,
            show_windows: self.global.show_windows,
            all_windows: &self.other_windows,
        };
        for group in &self.repo_groups {
            // The per-row matchers are the same `search_matches_*` the
            // renderer passes, over the same lowered query.
            let plan = crate::group::plan_group_rows(
                group,
                &plan_ctx,
                self.search_matches_group(&query_lower, group),
                |pane, git| self.search_matches_pane(&query_lower, pane, git),
                |window| self.search_matches_window(&query_lower, window),
            );
            if !plan.passes_repo_filter {
                continue;
            }
            // Collapsed groups render their header but no rows. They are
            // not selectable and contribute nothing to the target list or
            // the header counts.
            if plan.collapsed {
                continue;
            }
            // The agreed row order — `ordered_group_rows` over the
            // plan's filtered sets — is the coordinate space both this
            // builder and `row_collector::collect` use, so the selection
            // index and the rendered `row_index` cannot drift.
            let rows = plan.rows;
            if rows.is_empty() {
                continue;
            }
            self.layout.group_starts.push(GroupStart {
                name: group.name.clone(),
                row_start: self.layout.pane_row_targets.len(),
            });
            for row in rows {
                let target = match row {
                    crate::group::GroupRow::Agent((pane, _)) => RowTarget {
                        pane_id: pane.pane_id.clone(),
                        is_window: false,
                    },
                    crate::group::GroupRow::Window(window) => RowTarget {
                        pane_id: window.pane_id.clone(),
                        is_window: true,
                    },
                };
                self.layout.pane_row_targets.push(target);
            }
        }

        let len = self.layout.pane_row_targets.len();
        if len == 0 {
            // No rows to select — reset the cursor so a later, larger view
            // cannot inherit a stale index and auto-scroll down to it. The
            // parked group survives: with nothing visible it is the only
            // way `z` can still resolve a fold.
            self.global.selected_pane_row = 0;
            return;
        }
        // The cursor ends up on a visible row from here on, so a group
        // parked behind the folds is no longer needed.
        self.collapsed_cursor_group = None;
        // Keep the cursor on the same pane when it survives the rebuild
        // (search typing, status changes, refreshes). During an active
        // search, a cursor whose pane fell out of the result set moves to
        // the first match instead of clamping to the last row, which is
        // where the old clamp left it.
        if let Some(previous) = previous_pane_id
            && let Some(index) = self
                .layout
                .pane_row_targets
                .iter()
                .position(|target| target.pane_id == previous)
        {
            self.global.selected_pane_row = index;
            return;
        }
        if self.search.has_query() {
            self.global.selected_pane_row = 0;
        } else if let Some((group_name, offset)) = previous_anchor
            && let Some(start) = self
                .layout
                .group_starts
                .iter()
                .position(|group| group.name == group_name)
        {
            // The cursor's pane is gone but its group survived: hold the
            // cursor's old position within that group, clamped to the
            // group's new size. Clamping to the global last row instead
            // used to walk the cursor into a later group, and the pane's
            // later return then re-anchored the selection back down —
            // the very surprise the pane-id re-anchor was built to kill,
            // surviving via the row-removal path.
            let group_start = self.layout.group_starts[start].row_start;
            let group_len = self
                .layout
                .group_starts
                .get(start + 1)
                .map(|group| group.row_start)
                .unwrap_or(len)
                .saturating_sub(group_start)
                .max(1);
            self.global.selected_pane_row = group_start + offset.min(group_len - 1);
        } else if self.global.selected_pane_row >= len {
            self.global.selected_pane_row = len - 1;
        }
    }

    pub fn is_group_collapsed(&self, name: &str) -> bool {
        self.global.collapsed_groups.contains(name)
    }

    /// Index into `layout.group_starts` of the group containing the
    /// current selection, or `None` when the list is empty.
    fn current_group_index(&self) -> Option<usize> {
        self.layout
            .group_starts
            .iter()
            .rposition(|group| group.row_start <= self.global.selected_pane_row)
    }

    /// Name of the group containing the current selection. When every
    /// row is hidden behind collapsed headers the cursor has no visible
    /// group; the parked group (see `collapsed_cursor_group`) stands in
    /// so `z` keeps toggling the fold that swallowed the cursor.
    pub fn selected_group_name(&self) -> Option<String> {
        if let Some(index) = self.current_group_index() {
            return self
                .layout
                .group_starts
                .get(index)
                .map(|group| group.name.clone());
        }
        self.collapsed_cursor_group.clone()
    }

    /// Select the first row. Returns whether the selection moved.
    pub fn select_first_row(&mut self) -> bool {
        if self.layout.pane_row_targets.is_empty() || self.global.selected_pane_row == 0 {
            return false;
        }
        self.global.selected_pane_row = 0;
        true
    }

    /// Select the last row. Returns whether the selection moved.
    pub fn select_last_row(&mut self) -> bool {
        let Some(last) = self.layout.pane_row_targets.len().checked_sub(1) else {
            return false;
        };
        if self.global.selected_pane_row == last {
            return false;
        }
        self.global.selected_pane_row = last;
        true
    }

    /// Jump to the first row of the next visible group. No wrap: the last
    /// group is the end of the list.
    pub fn jump_to_next_group(&mut self) -> bool {
        let Some(current) = self.current_group_index() else {
            return false;
        };
        let Some(next) = self.layout.group_starts.get(current + 1) else {
            return false;
        };
        if self.global.selected_pane_row == next.row_start {
            return false;
        }
        self.global.selected_pane_row = next.row_start;
        true
    }

    /// Jump to the first row of the previous visible group. No wrap.
    pub fn jump_to_previous_group(&mut self) -> bool {
        let Some(current) = self.current_group_index() else {
            return false;
        };
        let Some(previous) = current
            .checked_sub(1)
            .and_then(|i| self.layout.group_starts.get(i))
        else {
            return false;
        };
        if self.global.selected_pane_row == previous.row_start {
            return false;
        }
        self.global.selected_pane_row = previous.row_start;
        true
    }

    /// Cycle to the next row carrying an attention flag, wrapping around
    /// the list. Returns false when no other row needs attention.
    pub fn jump_to_next_attention(&mut self) -> bool {
        let len = self.layout.pane_row_targets.len();
        if len == 0 {
            return false;
        }
        // One lookup map instead of per-row scans: window rows resolved
        // their attention by linearly sweeping every `other_windows`
        // entry for *each* candidate row — O(rows × windows) per
        // keypress (audit 2026-11-16 J6). The agent half did the same
        // through `pane_by_id`'s linear sweep per candidate row —
        // O(rows × panes) (audit 2026-11-19 A7) — so both halves read
        // one map each.
        let window_attention: std::collections::HashMap<&str, PaneAttention> = self
            .other_windows
            .values()
            .flat_map(|windows| windows.iter())
            .map(|window| (window.pane_id.as_str(), window.attention))
            .collect();
        let agent_attention: std::collections::HashMap<&str, PaneAttention> = self
            .repo_groups
            .iter()
            .flat_map(|group| group.panes.iter())
            .map(|(pane, _)| (pane.pane_id.as_str(), pane.attention))
            .collect();
        let current = self.global.selected_pane_row.min(len - 1);
        for offset in 1..len {
            let index = (current + offset) % len;
            let Some(target) = self.layout.pane_row_targets.get(index) else {
                continue;
            };
            let attention = if target.is_window {
                window_attention
                    .get(target.pane_id.as_str())
                    .copied()
                    .unwrap_or(PaneAttention::None)
            } else {
                agent_attention
                    .get(target.pane_id.as_str())
                    .copied()
                    .unwrap_or(PaneAttention::None)
            };
            if attention != PaneAttention::None {
                self.global.selected_pane_row = index;
                return true;
            }
        }
        false
    }

    /// Fold or unfold every group from one key: with every visible group
    /// expanded, `Z` collapses them all; the moment any of them is
    /// collapsed — none or N-1 expanded — it expands everything back.
    /// Only visible groups count: stale persisted names and groups the
    /// repo filter hides are not part of the list. The cursor's group is
    /// parked on the collapse path so `z` reopens it from the
    /// fully-folded list, and the expand path lands the cursor on the
    /// parked group's first row (vim's za/zR behavior). Returns false
    /// when there is no visible group to act on.
    pub fn toggle_all_groups(&mut self) -> bool {
        let visible: std::collections::HashSet<String> = self
            .repo_groups
            .iter()
            .filter(|group| self.global.repo_filter.matches_group(&group.name))
            .map(|group| group.name.clone())
            .collect();
        if visible.is_empty() {
            return false;
        }
        let all_expanded = visible
            .iter()
            .all(|name| !self.global.collapsed_groups.contains(name));
        if all_expanded {
            if let Some(name) = self.selected_group_name() {
                self.collapsed_cursor_group = Some(name);
            }
            self.global.collapsed_groups.extend(visible);
            self.global.queue_collapsed_save();
            self.rebuild_row_targets();
            return true;
        }
        let parked = self.collapsed_cursor_group.clone();
        self.global.collapsed_groups.clear();
        self.global.queue_collapsed_save();
        self.rebuild_row_targets();
        if let Some(name) = parked
            && let Some(start) = self
                .layout
                .group_starts
                .iter()
                .find(|group| group.name == name)
        {
            self.global.selected_pane_row = start.row_start;
        }
        true
    }

    /// Collapse or expand the group containing the current selection.
    /// After collapsing, the old selection pane is gone, so the rebuild
    /// keeps the cursor at the same index — which is now the first row of
    /// the next group (or the last row when the collapsed group was last).
    pub fn toggle_selected_group_collapse(&mut self) -> bool {
        let Some(name) = self.selected_group_name() else {
            return false;
        };
        self.toggle_group_collapse(&name)
    }

    /// Collapse or expand a named group. The click path lands here
    /// straight from the header's hit rect (see
    /// [`GroupHeaderTarget`]); the keyboard path resolves the
    /// selection's group first, via
    /// [`Self::toggle_selected_group_collapse`]. The rebuild shifts the
    /// cursor the same way the keyboard path does.
    pub fn toggle_group_collapse(&mut self, name: &str) -> bool {
        let collapsing = !self.global.collapsed_groups.contains(name);
        // When the fold takes the cursor's own group, remember it: with
        // no visible row left, the cursor has no group to resolve
        // against and `z` would go dead (the pre-park bug behind
        // "collapse everything and the keyboard stops working").
        if collapsing && self.selected_group_name().as_deref() == Some(name) {
            self.collapsed_cursor_group = Some(name.to_string());
        }
        // Reopening the parked group moves the cursor onto the fold's
        // first line — vim's `za` on a closed fold — instead of the
        // rebuild's clamp to the last row of the list.
        let reopen_parked = !collapsing
            && self.current_group_index().is_none()
            && self.collapsed_cursor_group.as_deref() == Some(name);
        if !self.global.collapsed_groups.remove(name) {
            self.global.collapsed_groups.insert(name.to_string());
        }
        self.global.queue_collapsed_save();
        self.rebuild_row_targets();
        if reopen_parked
            && let Some(start) = self
                .layout
                .group_starts
                .iter()
                .find(|group| group.name == name)
        {
            self.global.selected_pane_row = start.row_start;
        }
        true
    }

    /// Handle a mouse scroll event. `delta` is expressed in full-size row
    /// lines (one wheel notch = 3 lines = one agent row). A compact row is
    /// a single line, so in compact mode a notch collapses to one line —
    /// otherwise each notch skips three panes. When the pointer sits over
    /// the TODO panel, the same delta scrolls that panel instead.
    pub fn handle_mouse_scroll(&mut self, delta: isize, row: u16) {
        if self.todos.visible && point_in_rect(row, self.layout.todo_area.x, self.layout.todo_area)
        {
            self.scrolls.todos.scroll(delta);
            return;
        }
        let lines = if self.global.compact {
            delta.signum()
        } else {
            delta
        };
        self.scrolls.panes.scroll(lines);
    }

    /// Handle a mouse click on the chyron row (the panel's top row).
    /// The event names a pane: the click selects that pane's row and
    /// the render's auto-scroll brings it into view. It does not jump
    /// into the pane — Enter/Right remain the explicit jump, matching
    /// list-row clicks. A blank chyron (nothing has fired) is inert.
    pub fn handle_chyron_click(&mut self) {
        let Some(event) = self.chyron.current.clone() else {
            return;
        };
        if let Some(row_index) = self
            .layout
            .pane_row_targets
            .iter()
            .position(|target| target.pane_id == event.pane_id)
        {
            self.global.selected_pane_row = row_index;
            self.focus_state.focus = Focus::Panes;
            self.global.queue_cursor_save();
        }
    }

    /// Handle a mouse click on the toolbar row. The merged menu button
    /// (`— ▾` / `ⓘ ▾`) is the row's only click target on the right edge;
    /// while the chyron shows, the whole row selects the event's pane
    /// instead (see [`Self::handle_mouse_click`]).
    pub fn handle_secondary_header_click(&mut self, col: u16) {
        if self
            .layout
            .menu_button_col
            .is_some_and(|menu_button_col| col >= menu_button_col)
        {
            self.toggle_repo_popup();
        }
    }

    /// Open the experimental overview grid popup (the header row's `☰`
    /// glyph and the sidebar's `o` key both land here). Fire-and-forget:
    /// a detached tmux client owns the popup, and failures surface in
    /// the flash banner.
    pub fn spawn_overview_popup(&mut self) {
        if let Err(e) = crate::cli::spawn_overview_popup_detached() {
            self.set_flash(format!("overview: {e}"));
        }
    }

    /// Handle mouse click in agents panel. Maps screen row to agent row
    /// via line_to_row (adjusted for scroll offset) and activates that pane.
    /// Row 0 is the single header row: while the chyron shows it selects
    /// the event's pane, otherwise it routes toolbar clicks (the merged
    /// menu button). Rows below map into the scrollable list.
    pub fn handle_mouse_click(&mut self, row: u16, col: u16) {
        if self.is_repo_popup_open() {
            if let Some(area) = self.repo_popup_area()
                && point_in_rect(row, col, area)
            {
                // Skip clicks on the popup chrome (top border). Rows in
                // the notices section are click-to-copy via the copy
                // targets; rows in the repo list select + confirm.
                if let Some(agent) = self.notices_copy_target_at(row, col).map(str::to_string) {
                    self.copy_notices_prompt(&agent);
                    return;
                }
                if row > area.y {
                    let repo_start = self.repo_popup_repo_start() as usize;
                    let inner_row = (row - area.y - 1) as usize;
                    if inner_row >= repo_start {
                        let item_index = inner_row - repo_start;
                        if item_index < self.repo_names().len() {
                            self.set_repo_popup_selected(item_index);
                            self.confirm_repo_popup();
                        }
                    }
                }
                return;
            }
            self.close_repo_popup();
            return;
        }
        if self.is_help_open() {
            if let Some(area) = self.help_popup_area()
                && point_in_rect(row, col, area)
            {
                return;
            }
            self.close_help_popup();
            return;
        }
        if self.is_spawn_input_open() {
            if let Some(area) = self.spawn_input_popup_area()
                && point_in_rect(row, col, area)
            {
                return;
            }
            self.close_spawn_input();
            return;
        }
        if self.is_remove_confirm_open() {
            if let Some(area) = self.remove_confirm_popup_area()
                && point_in_rect(row, col, area)
            {
                return;
            }
            self.close_remove_confirm();
            return;
        }

        // The flash banner Clear-paints over the panel's bottom row for
        // its ~4s lifetime; the targets under it must not fire. Clicking
        // the banner text used to toggle the invisible todo item (or
        // activate the hidden agent row) beneath it (audit 2026-11-19
        // B2). Checked before the toolbar row so a banner on a
        // one-row-tall panel still wins.
        if self.flash_showing()
            && self
                .layout
                .flash_rect
                .is_some_and(|rect| point_in_rect(row, col, rect))
        {
            return;
        }

        // The single header row hosts the toolbar, or the chyron while
        // one is showing. Row defaults match the normal layout for
        // callers (tests) that have not rendered a frame yet. The `☰`
        // overview button owns the row's first columns in every state —
        // clicking it opens the grid. Over the banner, the button's
        // corner columns still route to the toolbar handler — the menu
        // button never stops being clickable; anything else on the row
        // selects the event's pane.
        let toolbar_row = self.layout.toolbar_row;
        if toolbar_row == row {
            let over_button = self
                .layout
                .menu_button_col
                .is_some_and(|menu_button_col| col >= menu_button_col);
            if col < OVERVIEW_BUTTON_WIDTH {
                self.spawn_overview_popup();
            } else if self.chyron.is_showing(self) && !over_button {
                self.handle_chyron_click();
            } else {
                self.handle_secondary_header_click(col);
            }
            return;
        }

        // Check the `+` spawn buttons before the pane-row fallback so a
        // click on the button doesn't also shift the pane selection.
        if let Some((repo_name, repo_root, session_name, anchor_y)) = self
            .layout
            .repo_spawn_targets
            .iter()
            .find(|t| point_in_rect(row, col, t.rect))
            .map(|t| {
                (
                    t.repo_name.clone(),
                    t.repo_root.clone(),
                    t.session_name.clone(),
                    t.rect.y,
                )
            })
        {
            self.open_spawn_input_for_repo(repo_name, repo_root, session_name, Some(anchor_y));
            return;
        }

        // Check the red `×` remove markers next to spawn-created branches.
        if let Some(pane_id) = self
            .layout
            .spawn_remove_targets
            .iter()
            .find(|t| point_in_rect(row, col, t.rect))
            .map(|t| t.pane_id.clone())
        {
            self.open_remove_confirm_for_pane(pane_id);
            return;
        }

        // Check group header rows next: a click anywhere on a header
        // toggles that group's fold (same state the `z` key writes). The
        // `+` / `×` targets above are matched first and keep their slice
        // of the header row.
        if let Some(name) = self
            .layout
            .group_header_targets
            .iter()
            .find(|t| point_in_rect(row, col, t.rect))
            .map(|t| t.group_name.clone())
        {
            self.toggle_group_collapse(&name);
            // The header lives in the agents panel, so the click acts
            // from Panes focus — a recovery click must not leave the
            // fold keys dead one press later. The TODO panel keeps the
            // keyboard while an edit or add is open, though: silently
            // abandoning it would be worse than the extra Esc.
            if self.todos.editing.is_none() && self.todos.adding.is_none() {
                self.focus_state.focus = Focus::Panes;
            }
            self.global.queue_cursor_save();
            return;
        }

        // Window header rows next (`index:name` under a group title):
        // a click jumps the tmux client to the window the header names,
        // same full-row convention as the fold target above. They never
        // share a row with the `+` / `×` markers, so ordering against
        // those is only about keeping the precedence readable.
        if let Some((session_name, window_index)) = self
            .layout
            .window_header_targets
            .iter()
            .find(|t| point_in_rect(row, col, t.rect))
            .map(|t| (t.session_name.clone(), t.window_index))
        {
            self.jump_to_window(&session_name, window_index);
            // Same Panes-focus rule as the group header above: the
            // click acted inside the agents panel.
            if self.todos.editing.is_none() && self.todos.adding.is_none() {
                self.focus_state.focus = Focus::Panes;
            }
            return;
        }

        // The TODO panel sits below the list area, so its screen rows would
        // otherwise fall through to the pane-row mapping. Claim every click
        // inside the panel: the checkbox column toggles, the item text
        // selects (a second click on the selection opens the editor), the
        // open input line re-opens insert mode, and header/empty space is
        // inert.
        if self.todos.visible && point_in_rect(row, col, self.layout.todo_area) {
            if let Some(rect) = self.layout.todo_input_rect
                && point_in_rect(row, col, rect)
            {
                // Insert mode already owns the line: a click inside it
                // must not reset the buffer.
                if self.todos.editing.is_none() && self.todos.adding.is_none() {
                    self.begin_todo_add();
                }
                return;
            }
            // An edit owns the keyboard; clicks elsewhere in the panel
            // must not toggle a different item underneath it.
            if self.todos.editing.is_some() {
                return;
            }
            if let Some(target) = self
                .layout
                .todo_row_targets
                .iter()
                .find(|t| point_in_rect(row, col, t.rect))
            {
                // The marker column is a checklist click; the text only
                // selects, and a click on the already-selected item opens
                // the editor — two-step so a stray click never edits by
                // surprise (the first click also focuses the panel, which
                // is the second click's precondition).
                let on_checkbox = target
                    .checkbox_rect
                    .is_some_and(|rect| point_in_rect(row, col, rect));
                let already_selected = self.todos.selected == target.index;
                self.todos.selected = target.index;
                self.focus_state.focus = Focus::Todos;
                if on_checkbox {
                    self.toggle_todo_selected();
                } else if already_selected {
                    self.begin_todo_edit();
                }
            }
            return;
        }

        // The list starts below the single header row.
        let line_index = row.saturating_sub(1) as usize + self.scrolls.panes.offset;
        if let Some(Some(agent_row)) = self.layout.line_to_row.get(line_index) {
            self.global.selected_pane_row = *agent_row;
            self.global.queue_cursor_save();
            self.activate_selected_pane();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::{PaneGitInfo, RepoGroup};
    use crate::tmux::{
        AgentType, PaneAttention, PaneInfo, PaneStatus, PermissionMode, WorktreeMetadata,
    };
    use crate::todo::TodoItem;
    use ratatui::layout::Rect;

    /// A minimal idle agent pane for group fixtures.
    fn idle_pane(id: &str) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status: PaneStatus::Idle,
            attention: PaneAttention::None,
            agent: AgentType::Claude,
            path: "/tmp/repo".into(),
            launch_cwd: String::new(),
            current_command: String::new(),
            prompt: String::new(),
            prompt_is_response: false,
            started_at: None,
            wait_reason: String::new(),
            permission_mode: PermissionMode::Default,
            subagents: vec![],
            pane_pid: None,
            worktree: WorktreeMetadata::default(),
            session_id: None,
            session_name: String::new(),
            tmux_session: String::new(),
            window_index: 0,
            window_name: String::new(),
            sidebar_spawned: false,
            bg_shell_cmd: None,
            bg_shell_pid: None,
            pane_title: String::new(),
            flagged: false,
        }
    }

    /// Two groups with one agent pane each, targets rebuilt — the
    /// minimal state for header-click routing tests.
    fn two_group_state() -> AppState {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![
            RepoGroup {
                name: "backend".into(),
                has_focus: false,
                panes: vec![(idle_pane("%1"), PaneGitInfo::default())],
            },
            RepoGroup {
                name: "frontend".into(),
                has_focus: false,
                panes: vec![(idle_pane("%2"), PaneGitInfo::default())],
            },
        ];
        state.rebuild_row_targets();
        state
    }

    #[test]
    fn a_click_on_the_flash_banner_row_is_inert() {
        // Audit 2026-11-19 B2: the banner Clear-paints over the panel's
        // bottom row for ~4s while the targets under it stay registered —
        // the click used to reach the invisible item underneath.
        let mut state = two_group_state();
        state.set_flash("worktree created");
        state.layout.flash_rect = Some(Rect::new(0, 2, 40, 1));
        state.layout.group_header_targets = vec![GroupHeaderTarget {
            rect: Rect::new(0, 2, 40, 1),
            group_name: "backend".into(),
        }];

        state.handle_mouse_click(2, 7);

        assert!(
            !state.is_group_collapsed("backend"),
            "a click on the banner row must not reach the covered target"
        );

        // Once the banner's window passes, the same row routes again —
        // the deadline check, not the stale rect, gates the routing.
        state.flash = None;
        state.handle_mouse_click(2, 7);
        assert!(state.is_group_collapsed("backend"));
    }

    #[test]
    fn clicking_a_group_header_toggles_its_fold() {
        let mut state = two_group_state();
        assert_eq!(state.layout.pane_row_targets.len(), 2);
        // Simulate the frame's hit rect for the first header row (as
        // `click_targets::materialize` writes it): full-width, one row.
        state.layout.group_header_targets = vec![GroupHeaderTarget {
            rect: Rect::new(0, 2, 40, 1),
            group_name: "backend".into(),
        }];

        state.handle_mouse_click(2, 7);

        assert!(state.is_group_collapsed("backend"));
        assert_eq!(
            state
                .layout
                .pane_row_targets
                .iter()
                .map(|t| t.pane_id.as_str())
                .collect::<Vec<_>>(),
            vec!["%2"],
            "the fold rebuilds the target list"
        );

        // The collapsed header re-registers as a target on the next
        // frame; clicking it again expands the group.
        state.handle_mouse_click(2, 7);
        assert!(!state.is_group_collapsed("backend"));
        assert_eq!(state.layout.pane_row_targets.len(), 2);
    }

    #[test]
    fn a_stale_repo_filter_reset_defers_its_shared_write() {
        // Audit 2026-11-21 F-2: the rebuild resets the filter locally so
        // this frame's list is right, but the tmux write is deferred to
        // the flush on the shared-state owners' ticks — a rebuild from a
        // keystroke or a snapshot fold must not spawn a `set -g` on the
        // spot, and repeated rebuilds of one reset must not queue a
        // write each.
        let mut state = two_group_state();
        state.global.repo_filter = RepoFilter::Repo("ghost".into());

        state.rebuild_row_targets();
        assert_eq!(state.global.repo_filter, RepoFilter::All);
        assert!(state.global.has_pending_repo_filter_reset());

        // Neighboring rebuilds (collapse toggles, search edits) re-run
        // the builder, but the already-reset filter is not stale again,
        // so the pending marker rides unchanged until an owner flushes.
        state.rebuild_row_targets();
        state.rebuild_row_targets();
        assert!(state.global.has_pending_repo_filter_reset());

        state.global.flush_pending_repo_filter_reset();
        assert!(!state.global.has_pending_repo_filter_reset());
    }

    #[test]
    fn a_matching_repo_filter_is_left_alone_by_the_rebuild() {
        let mut state = two_group_state();
        state.global.repo_filter = RepoFilter::Repo("backend".into());

        state.rebuild_row_targets();

        assert_eq!(state.global.repo_filter, RepoFilter::Repo("backend".into()));
        assert!(!state.global.has_pending_repo_filter_reset());
        // A flush with nothing pending is inert.
        state.global.flush_pending_repo_filter_reset();
        assert!(!state.global.has_pending_repo_filter_reset());
    }

    #[test]
    fn header_click_folds_session_groups_too() {
        // The fold state is keyed by group name, and session grouping
        // names groups by tmux session — the same click path must fold
        // there, leaving the other sessions' rows untouched.
        let mut state = two_group_state();
        state.global.group_by = crate::state::GroupBy::Session;
        state.rebuild_row_targets();
        state.layout.group_header_targets = vec![GroupHeaderTarget {
            rect: Rect::new(0, 2, 40, 1),
            group_name: "frontend".into(),
        }];

        state.handle_mouse_click(2, 0);

        assert!(state.is_group_collapsed("frontend"));
        assert_eq!(
            state
                .layout
                .pane_row_targets
                .iter()
                .map(|t| t.pane_id.as_str())
                .collect::<Vec<_>>(),
            vec!["%1"],
            "the backend session keeps its row"
        );
    }

    #[test]
    fn header_click_leaves_the_spawn_button_in_charge_of_its_slice() {
        // The `+` spawn button is matched before header targets; when
        // both rects cover the click, the button wins and the fold
        // state is untouched.
        let mut state = two_group_state();
        state.layout.group_header_targets = vec![GroupHeaderTarget {
            rect: Rect::new(0, 2, 40, 1),
            group_name: "backend".into(),
        }];
        state.layout.repo_spawn_targets = vec![RepoSpawnTarget {
            rect: Rect::new(38, 2, 1, 1),
            repo_name: "backend".into(),
            repo_root: "/tmp/repo".into(),
            session_name: "sess".into(),
        }];

        state.handle_mouse_click(2, 38);

        assert!(
            state.is_spawn_input_open(),
            "the spawn modal opened instead of folding"
        );
        assert!(!state.is_group_collapsed("backend"));
    }

    /// A visible TODO panel with pre-registered click targets, as
    /// `render_todo_panel` would write them: two open items on rows 2-3,
    /// all inside `todo_area` (rows 2-5). Row 5 is left for the input
    /// rect.
    fn todo_click_state() -> AppState {
        let mut state = AppState::new("%99".into());
        state.todos.items = vec![
            TodoItem {
                text: "Buy milk".into(),
                done: false,
            },
            TodoItem {
                text: "Write the report".into(),
                done: false,
            },
        ];
        state.todos.visible = true;
        state.layout.todo_area = Rect::new(0, 2, 30, 4);
        state.layout.todo_row_targets = vec![
            TodoRowTarget {
                rect: Rect::new(0, 2, 30, 1),
                checkbox_rect: Some(Rect::new(0, 2, 4, 1)),
                index: 0,
            },
            TodoRowTarget {
                rect: Rect::new(0, 3, 30, 1),
                checkbox_rect: Some(Rect::new(0, 3, 4, 1)),
                index: 1,
            },
        ];
        state
    }

    #[test]
    fn todo_checkbox_click_toggles_the_item() {
        let mut state = todo_click_state();

        state.handle_mouse_click(2, 1); // the marker column of item 0

        assert!(state.todos.items[0].done, "the checkbox click toggles");
        assert_eq!(state.focus_state.focus, Focus::Todos);
        assert!(
            state.todos.editing.is_none(),
            "a checkbox click must not open the editor"
        );
        assert_eq!(
            state.todos.selected, 0,
            "the completed section is visible by default, so the item \
            stays put and keeps the selection (Space behaves the same)"
        );
    }

    #[test]
    fn todo_text_click_selects_without_toggling() {
        let mut state = todo_click_state();

        state.handle_mouse_click(3, 5); // the text of item 1

        assert_eq!(state.todos.selected, 1);
        assert_eq!(state.focus_state.focus, Focus::Todos);
        assert!(!state.todos.items[1].done, "a text click must not toggle");
        assert!(
            state.todos.editing.is_none(),
            "the first click only selects"
        );
    }

    #[test]
    fn todo_second_text_click_on_the_selection_opens_the_editor() {
        let mut state = todo_click_state();

        state.handle_mouse_click(3, 5); // select item 1
        state.handle_mouse_click(3, 5); // click it again

        let edit = state
            .todos
            .editing
            .as_ref()
            .expect("the second click opens the editor");
        assert_eq!(edit.index, 1);
        assert_eq!(
            edit.input, "Write the report",
            "the editor prefills the item's text"
        );
    }

    #[test]
    fn todo_checkbox_click_unchecks_a_done_item() {
        let mut state = todo_click_state();
        state.global.todo_hide_completed = false; // section expanded
        state.todos.items[0].done = true;

        state.handle_mouse_click(2, 3);

        assert!(
            !state.todos.items[0].done,
            "the done block toggles from the mouse"
        );
        assert_eq!(
            state.todos.selected, 0,
            "unchecking keeps the selection on the item, now in the open block"
        );
    }

    #[test]
    fn todo_second_text_click_on_a_done_item_does_not_edit() {
        let mut state = todo_click_state();
        state.global.todo_hide_completed = false; // done block clickable
        state.todos.items[0].done = true;
        state.todos.selected = 1;

        state.handle_mouse_click(2, 5); // select the done item's text
        state.handle_mouse_click(2, 5); // click it again

        assert_eq!(state.todos.selected, 0, "the click still selects");
        assert!(
            state.todos.editing.is_none(),
            "the done block is a graveyard: no editor on struck-through text"
        );
    }

    #[test]
    fn todo_clicks_stay_inert_while_an_edit_is_open() {
        let mut state = todo_click_state();
        state.todos.selected = 0;
        state.focus_state.focus = Focus::Todos;
        state.begin_todo_edit();

        state.handle_mouse_click(3, 5); // another item's text
        state.handle_mouse_click(2, 0); // even the edited item's checkbox

        assert_eq!(state.todos.selected, 0, "the selection must not move");
        assert_eq!(
            state.todos.editing.as_ref().map(|e| e.index),
            Some(0),
            "the open edit must stay"
        );
        assert!(
            !state.todos.items[0].done,
            "nothing toggles underneath the edit"
        );
        assert!(!state.todos.items[1].done);
    }

    #[test]
    fn todo_input_line_click_begins_adding() {
        let mut state = todo_click_state();
        state.layout.todo_input_rect = Some(Rect::new(0, 5, 30, 1));

        state.handle_mouse_click(5, 1);

        assert_eq!(
            state.todos.adding.as_deref(),
            Some(""),
            "clicking the input line opens insert mode"
        );
        assert_eq!(state.focus_state.focus, Focus::Todos);
    }

    #[test]
    fn todo_input_line_click_keeps_the_buffer_while_adding() {
        let mut state = todo_click_state();
        state.begin_todo_add();
        for c in "draft".chars() {
            state.todo_input_push_char(c);
        }
        state.layout.todo_input_rect = Some(Rect::new(0, 5, 30, 1));

        state.handle_mouse_click(5, 1);

        assert_eq!(
            state.todos.adding.as_deref(),
            Some("draft"),
            "a click inside the input line must not reset the buffer"
        );
    }

    #[test]
    fn todo_input_line_click_stays_inert_while_an_edit_is_open() {
        let mut state = todo_click_state();
        state.todos.selected = 0;
        state.begin_todo_edit();
        state.layout.todo_input_rect = Some(Rect::new(0, 5, 30, 1));

        state.handle_mouse_click(5, 1);

        assert!(state.todos.editing.is_some(), "the edit survives the click");
        assert!(
            state.todos.adding.is_none(),
            "the input must not open underneath the edit"
        );
    }
}
