use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use super::{AppState, Focus, TodoScope};
use crate::todo::TodoItem;

/// How long a delete / clear-completed stays undoable. Matches the
/// clear-completed flash banner's own 4-second window so the stash and
/// the banner that names `u` as the undo key expire together (a plain
/// delete sets no banner — `?` documents the key).
const UNDO_WINDOW: Duration = Duration::from_secs(4);

/// A delete / clear-completed stashed for the undo window. `items` keeps
/// each removed item with its original index into `TodoState.items`;
/// `file_stamp` is the file state the deletion landed in, so a write by
/// another instance during the window invalidates the stash (restoring
/// over newer external state would resurrect stale data).
#[derive(Debug, Clone)]
pub struct TodoUndo {
    pub items: Vec<(usize, TodoItem)>,
    pub deadline: Instant,
    pub file_stamp: Option<(SystemTime, u64)>,
    /// Whether the removal actually reached the backing file. A
    /// failed-persistence delete / clear removes the items from memory
    /// only — the file still holds them — so the undo must restore
    /// memory-only, or `update_in` would insert a second copy of items
    /// the file never lost.
    pub landed: bool,
}

/// In-progress text edit for one item, opened with `e`. The buffer is
/// rendered in place of the item's own text; `cursor` is a char index
/// into `input`.
#[derive(Debug, Clone)]
pub struct TodoEdit {
    pub index: usize,
    pub input: String,
    pub cursor: usize,
}

/// Manual TODO list state for one sidebar instance.
///
/// The item list itself is shared across instances through the file
/// backing `dir`; `visible` follows the shared panel state when the
/// scope is global (the default), and `selected` plus the add-line
/// buffer stay per-instance. `dir` is `None` in unit-test builds (and
/// when no state directory is available), which keeps every mutation in
/// memory so the tests never touch the user's real list.
#[derive(Debug, Clone, Default)]
pub struct TodoState {
    pub items: Vec<TodoItem>,
    /// Selected item index into `items` (not into the visible subset).
    pub selected: usize,
    /// Panel visibility. Mirrors the shared `@sidebar_todo_visible` value
    /// when the panel scope is global (the default), so opening the panel
    /// in one tmux session shows it in every other one; with a local
    /// scope every sidebar starts hidden and owns its own state.
    pub visible: bool,
    /// `Some(input)` while the inline add line owns the keyboard.
    pub adding: Option<String>,
    /// `Some(edit)` while the selected item's text is being edited.
    pub editing: Option<TodoEdit>,
    /// Stamp of the file as of the last load, used to detect edits made
    /// by another sidebar instance.
    pub file_stamp: Option<(SystemTime, u64)>,
    /// Non-fatal persistence error, surfaced in the panel.
    pub error: Option<String>,
    /// Directory backing the list. `None` means in-memory only.
    pub dir: Option<PathBuf>,
    /// The last destructive action (delete / clear-completed) stashed for
    /// the undo window: `u` while this is `Some` and unexpired restores
    /// the items. A new destructive action replaces the stash.
    pub undo: Option<TodoUndo>,
}

impl TodoState {
    /// Point this instance at a state directory and load its list.
    /// Passing `None` keeps the list in memory.
    pub fn attach(&mut self, dir: Option<PathBuf>) {
        self.dir = dir;
        self.reload();
    }

    /// Load the on-disk list, recording its stamp. A missing file is an
    /// empty list, not an error.
    pub fn reload(&mut self) {
        let Some(dir) = self.dir.clone() else {
            return;
        };
        match crate::todo::load_in(&dir) {
            Ok(items) => {
                self.items = items;
                self.file_stamp = crate::todo::stamp_in(&dir);
                self.error = None;
            }
            Err(err) => {
                self.error = Some(format!("read failed: {err}"));
            }
        }
    }

    /// Reload when another instance changed the file. Cheap enough to run
    /// once per refresh tick: one `stat` per second.
    pub fn refresh_if_changed(&mut self, hide_completed: bool) {
        let Some(dir) = self.dir.clone() else {
            return;
        };
        let stamp = crate::todo::stamp_in(&dir);
        if stamp == self.file_stamp {
            return;
        }
        self.file_stamp = stamp;
        match crate::todo::load_in(&dir) {
            Ok(items) => {
                self.items = items;
                self.error = None;
                self.clamp_selection(hide_completed);
            }
            Err(err) => {
                self.error = Some(format!("read failed: {err}"));
            }
        }
    }

    /// Indices into `items` in display order: every open item (in
    /// `items` order) first, then every completed item (in `items`
    /// order) below it. With `hide_completed` on the completed section
    /// is collapsed and only the open block is returned.
    pub fn visible_indices(&self, hide_completed: bool) -> Vec<usize> {
        let mut visible = Vec::with_capacity(self.items.len());
        let mut done = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            if item.done {
                done.push(i);
            } else {
                visible.push(i);
            }
        }
        if !hide_completed {
            visible.extend(done);
        }
        visible
    }

    pub fn open_count(&self) -> usize {
        self.items.iter().filter(|item| !item.done).count()
    }

    pub fn total_count(&self) -> usize {
        self.items.len()
    }

    /// Keep the selection on a visible item. When the selected item was
    /// hidden (completed while the completed section is collapsed,
    /// deleted, or removed by an external edit), move to the nearest
    /// visible item at or after the old position, falling back to the
    /// last one.
    pub fn clamp_selection(&mut self, hide_completed: bool) {
        let visible = self.visible_indices(hide_completed);
        if visible.is_empty() {
            self.selected = 0;
            return;
        }
        if visible.contains(&self.selected) {
            return;
        }
        self.selected = visible
            .iter()
            .copied()
            .find(|&index| index >= self.selected)
            .unwrap_or_else(|| *visible.last().expect("visible is non-empty"));
    }

    /// Move the selection by `delta` visible items. Returns whether it
    /// moved.
    pub fn move_selection(&mut self, delta: isize, hide_completed: bool) -> bool {
        let visible = self.visible_indices(hide_completed);
        if visible.is_empty() {
            return false;
        }
        let position = visible
            .iter()
            .position(|&index| index == self.selected)
            .unwrap_or(0);
        let next = (position as isize + delta).clamp(0, visible.len() as isize - 1) as usize;
        if next == position {
            return false;
        }
        self.selected = visible[next];
        true
    }

    /// Toggle the checkbox of the item at `index`, persisting first when
    /// a backing directory exists. On a persistence failure the change
    /// still lands in memory so the UI stays responsive, and the error is
    /// surfaced in the panel.
    pub fn toggle(&mut self, index: usize, hide_completed: bool) {
        if index >= self.items.len() {
            return;
        }
        match self.dir.clone() {
            None => {
                if let Some(item) = self.items.get_mut(index) {
                    item.done = !item.done;
                }
            }
            Some(dir) => match crate::todo::update_in(&dir, |items| {
                if let Some(item) = items.get_mut(index) {
                    item.done = !item.done;
                }
            }) {
                Ok(items) => {
                    self.items = items;
                    self.file_stamp = crate::todo::stamp_in(&dir);
                    self.error = None;
                }
                Err(err) => {
                    if let Some(item) = self.items.get_mut(index) {
                        item.done = !item.done;
                    }
                    self.error = Some(format!("save failed: {err}"));
                }
            },
        }
        self.clamp_selection(hide_completed);
    }

    /// Append an item and select it.
    pub fn add(&mut self, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let item = TodoItem { text, done: false };
        match self.dir.clone() {
            None => self.items.push(item),
            Some(dir) => match crate::todo::update_in(&dir, |items| items.push(item.clone())) {
                Ok(items) => {
                    self.items = items;
                    self.file_stamp = crate::todo::stamp_in(&dir);
                    self.error = None;
                }
                Err(err) => {
                    self.items.push(item);
                    self.error = Some(format!("save failed: {err}"));
                }
            },
        }
        self.selected = self.items.len().saturating_sub(1);
    }

    /// Delete every completed item (both visible and hidden), stashing
    /// them with their original indices for the undo window. Returns the
    /// removed items in file order; empty when there was nothing to clear
    /// (and no undo was stashed).
    pub fn clear_completed(&mut self, hide_completed: bool) -> Vec<(usize, TodoItem)> {
        if !self.items.iter().any(|item| item.done) {
            return Vec::new();
        }
        let mut landed = false;
        let removed = match self.dir.clone() {
            None => {
                landed = true;
                let removed = Self::completed_entries(&self.items);
                self.items.retain(|item| !item.done);
                removed
            }
            Some(dir) => {
                let mut disk = Vec::new();
                match crate::todo::update_in(&dir, |items| {
                    disk = Self::completed_entries(items);
                    items.retain(|item| !item.done);
                }) {
                    Ok(items) => {
                        self.items = items;
                        self.file_stamp = crate::todo::stamp_in(&dir);
                        self.error = None;
                        landed = true;
                        disk
                    }
                    Err(err) => {
                        // Persistence failed: apply the clear in memory
                        // only — the file still holds the completed
                        // items, so the undo must restore memory-only
                        // instead of writing a second copy back.
                        let removed = Self::completed_entries(&self.items);
                        self.items.retain(|item| !item.done);
                        self.error = Some(format!("save failed: {err}"));
                        removed
                    }
                }
            }
        };
        if !removed.is_empty() {
            self.stash(removed.clone(), landed);
        }
        self.clamp_selection(hide_completed);
        removed
    }

    /// Replace the text of the item at `index`, persisting first.
    /// Whitespace-only text is ignored.
    pub fn set_text(&mut self, index: usize, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() || index >= self.items.len() {
            return;
        }
        match self.dir.clone() {
            None => {
                if let Some(item) = self.items.get_mut(index) {
                    item.text = text;
                }
            }
            Some(dir) => match crate::todo::update_in(&dir, |items| {
                if let Some(item) = items.get_mut(index) {
                    item.text = text.clone();
                }
            }) {
                Ok(items) => {
                    self.items = items;
                    self.file_stamp = crate::todo::stamp_in(&dir);
                    self.error = None;
                }
                Err(err) => {
                    if let Some(item) = self.items.get_mut(index) {
                        item.text = text;
                    }
                    self.error = Some(format!("save failed: {err}"));
                }
            },
        }
    }

    /// Delete the item at `index`, stashing it for the undo window.
    /// Returns the deleted item, or `None` when nothing was deleted
    /// (bad index, or a concurrent edit that already removed the file
    /// slot) — no undo is stashed in that case.
    pub fn delete(&mut self, index: usize, hide_completed: bool) -> Option<TodoItem> {
        if index >= self.items.len() {
            return None;
        }
        let mut landed = false;
        let removed = match self.dir.clone() {
            None => {
                landed = true;
                Some((index, self.items.remove(index)))
            }
            Some(dir) => {
                let mut removed = None;
                match crate::todo::update_in(&dir, |items| {
                    if index < items.len() {
                        removed = Some((index, items.remove(index)));
                    }
                }) {
                    Ok(items) => {
                        // A concurrent edit that shrank the file skips
                        // the removal; the fresh file wins (no undo).
                        self.items = items;
                        self.file_stamp = crate::todo::stamp_in(&dir);
                        self.error = None;
                        landed = true;
                        removed
                    }
                    Err(err) => {
                        // Persistence failed: the delete lands in memory
                        // only — the file still holds the item, so the
                        // undo must restore memory-only instead of
                        // writing a second copy back (`TodoUndo::landed`).
                        let item = self.items.remove(index);
                        self.error = Some(format!("save failed: {err}"));
                        Some((index, item))
                    }
                }
            }
        };
        // A concurrent edit that shrank the file skipped the removal and
        // left a shorter `items` behind; keep the selection pinned before
        // either return — the `?` included.
        self.clamp_selection(hide_completed);
        let (index, item) = removed?;
        self.stash(vec![(index, item.clone())], landed);
        Some(item)
    }

    /// Record a destructive action's removals as the pending undo: the
    /// stash is relative to the file state after the write, so a later
    /// write by another instance invalidates it. `landed` says whether
    /// the removal reached the backing file (see [`TodoUndo::landed`]).
    /// A new destructive action replaces any still-pending stash.
    fn stash(&mut self, items: Vec<(usize, TodoItem)>, landed: bool) {
        self.undo = Some(TodoUndo {
            deadline: Instant::now() + UNDO_WINDOW,
            file_stamp: self.file_stamp,
            landed,
            items,
        });
    }

    /// Restore the stashed items if the undo window is still open: the
    /// deadline must not have passed and the file must not have changed
    /// since the deletion (a write by another instance would make the
    /// stashed indices stale — restoring over a newer state would
    /// resurrect stale data). An invalid stash is dropped silently.
    ///
    /// The freshness check stats the file fresh rather than trusting the
    /// per-tick `self.file_stamp` cache, which can lag the disk by up to
    /// one refresh tick — an external write inside that window must still
    /// refuse the restore.
    ///
    /// The items go back to their original positions: entries are
    /// applied in descending index order, each at its original index
    /// minus the count of still-missing earlier entries (every earlier
    /// removed item shifted the slot left by one), so the pre-delete
    /// order comes back. When the removal reached the file the restore
    /// persists through `update_in` like every other mutation; on a
    /// persistence failure the restore still lands in memory and the
    /// error is surfaced in the panel. When the removal never reached
    /// the file (failed persistence) the restore is memory-only — the
    /// file still holds the items, and a write would duplicate them.
    pub fn undo(&mut self, hide_completed: bool) -> bool {
        let Some(undo) = self.undo.take() else {
            return false;
        };
        if Instant::now() >= undo.deadline {
            return false;
        }
        if let Some(dir) = self.dir.clone()
            && crate::todo::stamp_in(&dir) != undo.file_stamp
        {
            return false;
        }
        let mut entries = undo.items;
        entries.sort_unstable_by_key(|(index, _)| *index);
        let restore = |items: &mut Vec<TodoItem>| {
            for (rank, (index, item)) in entries.iter().enumerate().rev() {
                // `rank` stashed entries sit earlier in the original
                // order and are still missing, each shifting this slot
                // left by one.
                let position = (index - rank).min(items.len());
                items.insert(position, item.clone());
            }
        };
        match self.dir.clone() {
            None => restore(&mut self.items),
            // The removal reached the file: the restore is a write like
            // any other mutation.
            Some(dir) if undo.landed => match crate::todo::update_in(&dir, restore) {
                Ok(items) => {
                    self.items = items;
                    self.file_stamp = crate::todo::stamp_in(&dir);
                    self.error = None;
                }
                Err(err) => {
                    restore(&mut self.items);
                    self.error = Some(format!("save failed: {err}"));
                }
            },
            // The removal never reached the file: memory is the only
            // place the items are missing, so restoring there lands back
            // in sync — which also retires the stale save error.
            Some(_) => {
                restore(&mut self.items);
                self.error = None;
            }
        }
        self.clamp_selection(hide_completed);
        true
    }

    /// The completed items with their indices into `items`.
    fn completed_entries(items: &[TodoItem]) -> Vec<(usize, TodoItem)> {
        items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.done)
            .map(|(index, item)| (index, item.clone()))
            .collect()
    }

    /// Move the item at `index` one position up or down **within the open
    /// block**: it swaps with its open neighbor in file order, so a
    /// completed item may sit between them, but the moved item never
    /// enters or crosses the completed block (done items are a
    /// graveyard, not reorderable work). A move of a completed item, a
    /// bad index, or a move at the block boundary is a no-op.
    ///
    /// Persists through `update_in` when a backing directory exists; on a
    /// persistence failure the swap still lands in memory and the error is
    /// surfaced in the panel (same policy as [`Self::delete`]). Returns
    /// the item's new index when it moved. Selection is index-based, so a
    /// concurrent edit from another instance can drift the mapping; that
    /// is accepted for now (no stable IDs in the file format).
    pub fn move_item(&mut self, index: usize, delta: isize) -> Option<usize> {
        let open: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| !item.done)
            .map(|(i, _)| i)
            .collect();
        // `pos` is the item's rank in the open block; `None` means the
        // index is bad or points at a completed item.
        let pos = open.iter().position(|&i| i == index)?;
        let target = pos as isize + delta;
        if target < 0 || target >= open.len() as isize {
            return None;
        }
        let partner = open[target as usize];
        let moved = match self.dir.clone() {
            None => {
                self.items.swap(index, partner);
                true
            }
            Some(dir) => {
                let swapped = &mut false;
                match crate::todo::update_in(&dir, |items| {
                    if index < items.len() && partner < items.len() {
                        items.swap(index, partner);
                        *swapped = true;
                    }
                }) {
                    Ok(items) => {
                        // A concurrent edit that shrank the file skips the
                        // swap; the fresh file then wins (the move is lost).
                        self.items = items;
                        self.file_stamp = crate::todo::stamp_in(&dir);
                        self.error = None;
                        *swapped
                    }
                    Err(err) => {
                        self.items.swap(index, partner);
                        self.error = Some(format!("save failed: {err}"));
                        true
                    }
                }
            }
        };
        moved.then_some(partner)
    }
}

impl AppState {
    /// Three-state `t` behavior:
    /// 1. Panel hidden → open it and focus the list.
    /// 2. Panel visible and focused → close it, returning focus to the
    ///    agent list.
    /// 3. Panel visible but the agent list (or filter bar) has focus →
    ///    focus the TODO list without closing it.
    pub fn toggle_todo_panel(&mut self) {
        if !self.todos.visible {
            self.set_todo_visible(true);
            self.todos.adding = None;
            self.todos.editing = None;
            self.focus_state.focus = Focus::Todos;
            self.todos.clamp_selection(self.global.todo_hide_completed);
        } else if self.focus_state.focus == Focus::Todos {
            self.set_todo_visible(false);
            self.todos.adding = None;
            self.todos.editing = None;
            self.focus_state.focus = Focus::Panes;
        } else {
            self.focus_state.focus = Focus::Todos;
            self.todos.clamp_selection(self.global.todo_hide_completed);
        }
    }

    /// Open or close the TODO panel, writing the shared visibility flag
    /// when the panel scope is global so every other sidebar follows.
    fn set_todo_visible(&mut self, visible: bool) {
        self.todos.visible = visible;
        if self.global.todo_scope == TodoScope::Global {
            self.global.set_todo_visible(visible);
        }
    }

    /// Adopt a panel-visibility change made by another sidebar (or the
    /// landing default at startup) when the scope is global. Closing the
    /// panel from elsewhere must also release a local focus/edit that
    /// pointed into it.
    pub fn sync_todo_visibility(&mut self) {
        if self.global.todo_scope != TodoScope::Global
            || self.todos.visible == self.global.todo_visible
        {
            return;
        }
        self.todos.visible = self.global.todo_visible;
        if !self.todos.visible {
            self.todos.adding = None;
            self.todos.editing = None;
            if self.focus_state.focus == Focus::Todos {
                self.focus_state.focus = Focus::Panes;
            }
        }
        self.todos.clamp_selection(self.global.todo_hide_completed);
    }

    /// Esc inside the panel: cancel the in-place edit, then the add
    /// line, otherwise hand focus back to the agent list and leave the
    /// panel visible.
    pub fn todo_escape(&mut self) {
        if self.todos.editing.is_some() {
            self.todos.editing = None;
        } else if self.todos.adding.is_some() {
            self.todos.adding = None;
        } else {
            self.focus_state.focus = Focus::Panes;
        }
    }

    pub fn move_todo_selection(&mut self, delta: isize) -> bool {
        self.todos
            .move_selection(delta, self.global.todo_hide_completed)
    }

    /// Move the selected item one position up or down within the open
    /// block; the selection follows it. A no-op for a completed
    /// selection, an empty list, or a boundary position.
    pub fn move_todo_selected(&mut self, delta: isize) -> bool {
        match self.todos.move_item(self.todos.selected, delta) {
            Some(new_index) => {
                self.todos.selected = new_index;
                true
            }
            None => {
                // No-op, or a concurrent edit invalidated the move: keep
                // the selection on a visible item either way.
                self.todos.clamp_selection(self.global.todo_hide_completed);
                false
            }
        }
    }

    /// Continue a downward walk from the agent list into the TODO panel:
    /// focus the panel landing on the first visible item. Only works when
    /// the panel is already visible and has items to show — a hidden
    /// panel stays hidden (the walk simply stops at the end of the list).
    pub fn focus_todo_from_panes(&mut self) -> bool {
        if !self.todos.visible {
            return false;
        }
        let Some(&first) = self
            .todos
            .visible_indices(self.global.todo_hide_completed)
            .first()
        else {
            return false;
        };
        self.todos.selected = first;
        self.todos.adding = None;
        self.todos.editing = None;
        self.focus_state.focus = Focus::Todos;
        true
    }

    /// Continue an upward walk from the top of the TODO list into the
    /// agent list: focus the list landing on the last agent row. No-op
    /// when the list has no rows.
    pub fn focus_panes_from_todos(&mut self) -> bool {
        let Some(last) = self.layout.pane_row_targets.len().checked_sub(1) else {
            return false;
        };
        self.global.selected_pane_row = last;
        self.focus_state.focus = Focus::Panes;
        true
    }

    pub fn page_todo_selection(&mut self, direction: isize) -> bool {
        let page = self.scrolls.todos.visible_height.max(1) as isize;
        self.todos
            .move_selection(direction * page, self.global.todo_hide_completed)
    }

    pub fn toggle_todo_selected(&mut self) {
        let index = self.todos.selected;
        self.todos.toggle(index, self.global.todo_hide_completed);
    }

    /// Delete the selected item. No banner: the list changing is the
    /// feedback, and the undo window is documented under `?` (`u` /
    /// Ctrl-z). The stash still expires after [`UNDO_WINDOW`].
    pub fn delete_todo_selected(&mut self) {
        let index = self.todos.selected;
        self.todos.delete(index, self.global.todo_hide_completed);
    }

    /// Clear the completed section and offer the 4-second undo window
    /// through the flash banner.
    pub fn clear_completed_todos(&mut self) {
        let removed = self.todos.clear_completed(self.global.todo_hide_completed);
        if removed.is_empty() {
            return;
        }
        self.set_flash(format!("cleared {} completed · u to undo", removed.len()));
    }

    /// Restore the last delete / clear-completed while its undo window
    /// is open. A silent no-op once the window has passed, the file
    /// changed since the deletion, or there is no pending stash.
    pub fn undo_todo_last(&mut self) -> bool {
        self.todos.undo(self.global.todo_hide_completed)
    }

    /// Open the inline add line, focusing the panel first.
    pub fn begin_todo_add(&mut self) {
        self.set_todo_visible(true);
        self.focus_state.focus = Focus::Todos;
        self.todos.editing = None;
        self.todos.adding = Some(String::new());
    }

    /// Commit the add line and immediately start a fresh one so several
    /// items can be entered in a row (Google Tasks style). An empty
    /// input ends the streak instead — there is nothing to create, so
    /// Enter finishes and insert mode closes, exactly like Esc.
    pub fn commit_todo_add(&mut self) {
        let Some(input) = self.todos.adding.take() else {
            return;
        };
        if input.trim().is_empty() {
            return;
        }
        self.todos.add(input);
        self.todos.clamp_selection(self.global.todo_hide_completed);
        self.todos.adding = Some(String::new());
    }

    pub fn cancel_todo_add(&mut self) {
        self.todos.adding = None;
    }

    /// Enter in the panel — Google's Return: the selected task opens for
    /// editing; with no task under the selection (empty list, or a stale
    /// index after an external edit) it starts a new one instead.
    pub fn todo_enter(&mut self) {
        if self.todos.selected >= self.todos.items.len() {
            self.begin_todo_add();
        } else {
            self.begin_todo_edit();
        }
    }

    /// Start editing the selected item's text in place, prefilled with
    /// its current text and the cursor at the end. A no-op for a
    /// completed item: the done block is a graveyard (the rule the
    /// reorder lives by), so the editor never opens on struck-through
    /// text — uncheck the item first to re-work it. This guard also
    /// covers the mouse's click-to-edit path, which routes through here.
    pub fn begin_todo_edit(&mut self) {
        let index = self.todos.selected;
        let Some(item) = self.todos.items.get(index) else {
            return;
        };
        if item.done {
            return;
        }
        let input = item.text.clone();
        let cursor = input.chars().count();
        self.todos.adding = None;
        self.todos.editing = Some(TodoEdit {
            index,
            input,
            cursor,
        });
    }

    /// Save the edited text and return to normal panel keys. An empty
    /// edit is ignored (the original text is kept).
    pub fn commit_todo_edit(&mut self) {
        let Some(edit) = self.todos.editing.take() else {
            return;
        };
        if edit.input.trim().is_empty() {
            return;
        }
        self.todos.set_text(edit.index, edit.input);
        self.todos.clamp_selection(self.global.todo_hide_completed);
    }

    /// Enter while editing — the editor's newline: save the text and
    /// open a fresh insert line for the next task, Google Tasks style.
    /// The edit itself keeps the empty-means-unchanged rule.
    pub fn commit_todo_edit_and_next(&mut self) {
        self.commit_todo_edit();
        self.begin_todo_add();
    }

    pub fn cancel_todo_edit(&mut self) {
        self.todos.editing = None;
    }

    pub fn todo_input_push_char(&mut self, c: char) {
        if let Some(edit) = self.todos.editing.as_mut() {
            let mut chars: Vec<char> = edit.input.chars().collect();
            let cursor = edit.cursor.min(chars.len());
            chars.insert(cursor, c);
            edit.input = chars.into_iter().collect();
            edit.cursor = cursor + 1;
        } else if let Some(input) = self.todos.adding.as_mut() {
            input.push(c);
        }
    }

    /// Backspace: delete the char before the cursor.
    pub fn todo_input_pop_char(&mut self) {
        if let Some(edit) = self.todos.editing.as_mut() {
            let mut chars: Vec<char> = edit.input.chars().collect();
            let cursor = edit.cursor.min(chars.len());
            if cursor > 0 {
                chars.remove(cursor - 1);
                edit.input = chars.into_iter().collect();
                edit.cursor = cursor - 1;
            }
        } else if let Some(input) = self.todos.adding.as_mut() {
            input.pop();
        }
    }

    /// Delete: remove the char at the cursor. The add line has no
    /// cursor, so this only applies to an open edit.
    pub fn todo_input_delete_char(&mut self) {
        let Some(edit) = self.todos.editing.as_mut() else {
            return;
        };
        let mut chars: Vec<char> = edit.input.chars().collect();
        let cursor = edit.cursor.min(chars.len());
        if cursor < chars.len() {
            chars.remove(cursor);
            edit.input = chars.into_iter().collect();
        }
    }

    pub fn todo_input_clear(&mut self) {
        if let Some(edit) = self.todos.editing.as_mut() {
            edit.input.clear();
            edit.cursor = 0;
        } else if let Some(input) = self.todos.adding.as_mut() {
            input.clear();
        }
    }

    /// Delete the word before the cursor (Ctrl-w).
    pub fn todo_input_pop_word(&mut self) {
        if let Some(edit) = self.todos.editing.as_mut() {
            let mut chars: Vec<char> = edit.input.chars().collect();
            let cursor = edit.cursor.min(chars.len());
            let mut cut = cursor;
            while cut > 0 && chars[cut - 1].is_whitespace() {
                cut -= 1;
            }
            while cut > 0 && !chars[cut - 1].is_whitespace() {
                cut -= 1;
            }
            if cut < cursor {
                chars.drain(cut..cursor);
                edit.input = chars.into_iter().collect();
                edit.cursor = cut;
            }
        } else if let Some(input) = self.todos.adding.as_mut() {
            while input.ends_with(char::is_whitespace) {
                input.pop();
            }
            while let Some(last) = input.chars().last() {
                if last.is_whitespace() {
                    break;
                }
                input.pop();
            }
        }
    }

    /// Move the edit cursor by `delta` chars, clamped to the buffer.
    pub fn todo_edit_cursor_move(&mut self, delta: isize) {
        let Some(edit) = self.todos.editing.as_mut() else {
            return;
        };
        let len = edit.input.chars().count();
        edit.cursor = (edit.cursor as isize)
            .saturating_add(delta)
            .clamp(0, len as isize) as usize;
    }

    /// Ctrl-Left: back to the start of the previous word, skipping the
    /// whitespace and then the word's chars before it. The same
    /// whitespace/word split Ctrl-w deletes by.
    pub fn todo_edit_cursor_word_left(&mut self) {
        let Some(edit) = self.todos.editing.as_mut() else {
            return;
        };
        let chars: Vec<char> = edit.input.chars().collect();
        let mut cursor = edit.cursor.min(chars.len());
        while cursor > 0 && chars[cursor - 1].is_whitespace() {
            cursor -= 1;
        }
        while cursor > 0 && !chars[cursor - 1].is_whitespace() {
            cursor -= 1;
        }
        edit.cursor = cursor;
    }

    /// Ctrl-Right: forward past the end of the next word, skipping the
    /// whitespace and then the word's chars after the cursor.
    pub fn todo_edit_cursor_word_right(&mut self) {
        let Some(edit) = self.todos.editing.as_mut() else {
            return;
        };
        let chars: Vec<char> = edit.input.chars().collect();
        let len = chars.len();
        let mut cursor = edit.cursor.min(len);
        while cursor < len && chars[cursor].is_whitespace() {
            cursor += 1;
        }
        while cursor < len && !chars[cursor].is_whitespace() {
            cursor += 1;
        }
        edit.cursor = cursor;
    }

    pub fn todo_edit_cursor_home(&mut self) {
        if let Some(edit) = self.todos.editing.as_mut() {
            edit.cursor = 0;
        }
    }

    pub fn todo_edit_cursor_end(&mut self) {
        if let Some(edit) = self.todos.editing.as_mut() {
            edit.cursor = edit.input.chars().count();
        }
    }

    /// Flip the shared hide-completed setting and keep the selection on a
    /// visible item.
    pub fn toggle_todo_hide_completed(&mut self) {
        self.global.toggle_todo_hide_completed();
        self.todos.clamp_selection(self.global.todo_hide_completed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(text: &str, done: bool) -> TodoItem {
        TodoItem {
            text: text.into(),
            done,
        }
    }

    fn state_with(items: Vec<TodoItem>) -> AppState {
        let mut state = AppState::new("%99".into());
        state.todos.items = items;
        state.todos.visible = true;
        state.focus_state.focus = Focus::Todos;
        state
    }

    #[test]
    fn panel_starts_hidden() {
        let state = AppState::new("%99".into());
        assert!(!state.todos.visible);
        assert!(!state.global.todo_visible);
        assert!(state.todos.items.is_empty());
        assert!(state.todos.dir.is_none());
    }

    #[test]
    fn opening_the_panel_shares_visibility_by_default() {
        let mut state = AppState::new("%99".into());
        state.toggle_todo_panel();
        assert!(state.todos.visible);
        assert!(
            state.global.todo_visible,
            "the default global scope publishes the open state"
        );

        state.toggle_todo_panel();
        assert!(!state.todos.visible);
        assert!(!state.global.todo_visible);
    }

    #[test]
    fn local_scope_keeps_visibility_per_instance() {
        let mut state = AppState::new("%99".into());
        state.global.todo_scope = TodoScope::Local;

        state.toggle_todo_panel();
        assert!(state.todos.visible);
        assert!(
            !state.global.todo_visible,
            "a local panel must not touch the shared flag"
        );
    }

    #[test]
    fn sync_adopts_a_shared_open_and_releases_focus_on_close() {
        let mut state = AppState::new("%99".into());

        state.global.todo_visible = true;
        state.sync_todo_visibility();
        assert!(state.todos.visible);
        assert_eq!(
            state.focus_state.focus,
            Focus::Panes,
            "another sidebar opening the panel must not steal focus"
        );

        state.focus_state.focus = Focus::Todos;
        state.todos.adding = Some("draft".into());
        state.global.todo_visible = false;
        state.sync_todo_visibility();
        assert!(!state.todos.visible);
        assert_eq!(state.focus_state.focus, Focus::Panes);
        assert!(state.todos.adding.is_none());
    }

    #[test]
    fn local_scope_ignores_the_shared_visibility_flag() {
        let mut state = AppState::new("%99".into());
        state.global.todo_scope = TodoScope::Local;
        state.global.todo_visible = true;

        state.sync_todo_visibility();
        assert!(!state.todos.visible);
    }

    #[test]
    fn t_shows_and_hides_the_panel_with_focus() {
        let mut state = AppState::new("%99".into());
        state.toggle_todo_panel();
        assert!(state.todos.visible);
        assert_eq!(state.focus_state.focus, Focus::Todos);

        state.toggle_todo_panel();
        assert!(!state.todos.visible);
        assert_eq!(state.focus_state.focus, Focus::Panes);
    }

    #[test]
    fn t_focuses_a_visible_panel_instead_of_closing_it() {
        let mut state = state_with(vec![item("a", false)]);
        state.todos.visible = false;
        state.focus_state.focus = Focus::Panes;

        // Hidden → open + focus.
        state.toggle_todo_panel();
        assert!(state.todos.visible);
        assert_eq!(state.focus_state.focus, Focus::Todos);

        // Visible but unfocused → focus, do not close.
        state.focus_state.focus = Focus::Panes;
        state.toggle_todo_panel();
        assert!(state.todos.visible);
        assert_eq!(state.focus_state.focus, Focus::Todos);

        // Focused → close + return focus to the list.
        state.toggle_todo_panel();
        assert!(!state.todos.visible);
        assert_eq!(state.focus_state.focus, Focus::Panes);
    }

    #[test]
    fn visible_indices_lists_open_items_before_completed() {
        let state = state_with(vec![item("a", false), item("b", true), item("c", false)]);
        assert_eq!(
            state.todos.visible_indices(false),
            vec![0, 2, 1],
            "expanded: open block first, then the completed block, each in items order"
        );
        assert_eq!(
            state.todos.visible_indices(true),
            vec![0, 2],
            "collapsed: only the open block"
        );
    }

    #[test]
    fn h_collapses_and_expands_the_completed_section() {
        let mut state = state_with(vec![item("a", false), item("b", true)]);
        state.global.todo_hide_completed = false;
        assert_eq!(
            state
                .todos
                .visible_indices(state.global.todo_hide_completed),
            vec![0, 1]
        );

        state.toggle_todo_hide_completed();

        assert!(state.global.todo_hide_completed, "the shared flag flips");
        assert_eq!(
            state
                .todos
                .visible_indices(state.global.todo_hide_completed),
            vec![0],
            "collapsed: only the open block is visible"
        );

        state.toggle_todo_hide_completed();
        assert!(!state.global.todo_hide_completed);
        assert_eq!(
            state
                .todos
                .visible_indices(state.global.todo_hide_completed),
            vec![0, 1]
        );
    }

    #[test]
    fn navigation_reaches_completed_items_when_expanded() {
        let mut state = state_with(vec![item("a", false), item("b", true)]);
        state.global.todo_hide_completed = false;
        state.todos.selected = 0;

        assert!(state.move_todo_selection(1));
        assert_eq!(
            state.todos.selected, 1,
            "j/k walks into the completed block"
        );
        assert!(!state.move_todo_selection(1), "clamped at the end");
    }

    #[test]
    fn unchecking_a_completed_item_keeps_selection_on_it() {
        let mut state = state_with(vec![item("a", false), item("b", true), item("c", true)]);
        state.global.todo_hide_completed = false; // section expanded
        state.todos.selected = 1;

        state.toggle_todo_selected();

        assert!(!state.todos.items[1].done);
        assert_eq!(
            state.todos.selected, 1,
            "the item stays selected, now inside the open block"
        );
        assert_eq!(
            state.todos.visible_indices(false),
            vec![0, 1, 2],
            "display order is derived: open block first, in items order"
        );
    }

    #[test]
    fn toggling_a_visible_item_persists_and_moves_selection() {
        let mut state = state_with(vec![item("a", false), item("b", false)]);
        state.global.todo_hide_completed = true;

        state.toggle_todo_selected();
        assert!(state.todos.items[0].done);
        assert_eq!(
            state.todos.selected, 1,
            "selection moves to the next open item"
        );
    }

    #[test]
    fn hide_completed_clamps_selection_to_a_visible_item() {
        let mut state = state_with(vec![item("a", false), item("b", true)]);
        state.todos.selected = 1;
        state.global.todo_hide_completed = false;

        state.toggle_todo_hide_completed();

        assert!(state.global.todo_hide_completed);
        assert_eq!(state.todos.selected, 0);
    }

    #[test]
    fn add_appends_and_selects_the_new_item() {
        let mut state = state_with(vec![item("a", false)]);
        state.begin_todo_add();
        for c in "new task".chars() {
            state.todo_input_push_char(c);
        }
        state.commit_todo_add();

        assert_eq!(state.todos.items.len(), 2);
        assert_eq!(state.todos.items[1].text, "new task");
        assert_eq!(state.todos.selected, 1);
        assert_eq!(
            state.todos.adding.as_deref(),
            Some(""),
            "the input stays open for the next item"
        );
    }

    #[test]
    fn enter_starts_a_fresh_line_for_rapid_entry() {
        let mut state = state_with(vec![]);
        state.begin_todo_add();

        for c in "first".chars() {
            state.todo_input_push_char(c);
        }
        state.commit_todo_add();
        for c in "second".chars() {
            state.todo_input_push_char(c);
        }
        state.commit_todo_add();

        assert_eq!(state.todos.items.len(), 2);
        assert_eq!(state.todos.items[0].text, "first");
        assert_eq!(state.todos.items[1].text, "second");
        assert_eq!(state.todos.selected, 1);
        assert_eq!(state.todos.adding.as_deref(), Some(""));

        // Esc ends the streak without adding the empty line.
        state.todo_escape();
        assert!(state.todos.adding.is_none());
        assert_eq!(state.todos.items.len(), 2);
    }

    #[test]
    fn blank_add_finishes_insert_mode() {
        let mut state = state_with(vec![]);
        state.begin_todo_add();
        state.todo_input_push_char(' ');
        state.commit_todo_add();
        assert!(state.todos.items.is_empty());
        assert_eq!(
            state.todos.adding, None,
            "Enter on an empty line ends the streak, like Google's empty Return"
        );
    }

    #[test]
    fn enter_edits_the_selected_task_or_starts_a_new_one() {
        // With a selection, Enter opens the editor on that item.
        let mut state = state_with(vec![item("a", false)]);
        state.todo_enter();
        assert_eq!(state.todos.editing.as_ref().unwrap().index, 0);

        // With no items, Enter starts a new task instead.
        let mut empty = state_with(vec![]);
        empty.todo_enter();
        assert!(empty.todos.editing.is_none());
        assert_eq!(empty.todos.adding.as_deref(), Some(""));
    }

    #[test]
    fn enter_while_editing_saves_and_opens_the_next_task() {
        let mut state = state_with(vec![item("old", false)]);
        state.todos.selected = 0;
        state.begin_todo_edit();
        state.todo_input_clear();
        for c in "renamed".chars() {
            state.todo_input_push_char(c);
        }

        state.commit_todo_edit_and_next();

        assert_eq!(state.todos.items[0].text, "renamed", "the edit saved");
        assert!(state.todos.editing.is_none());
        assert_eq!(
            state.todos.adding.as_deref(),
            Some(""),
            "insert mode opens for the next task"
        );
        assert_eq!(state.focus_state.focus, Focus::Todos);

        // The next Enter adds the task and keeps the streak going.
        for c in "next".chars() {
            state.todo_input_push_char(c);
        }
        state.commit_todo_add();
        assert_eq!(state.todos.items.len(), 2);
        assert_eq!(state.todos.items[1].text, "next");
        assert_eq!(state.todos.adding.as_deref(), Some(""));

        // Enter on the empty buffer finishes the streak.
        state.commit_todo_add();
        assert_eq!(state.todos.adding, None);
    }

    #[test]
    fn edit_prefills_and_saves_the_new_text() {
        let mut state = state_with(vec![item("old", false)]);
        state.todos.selected = 0;

        state.begin_todo_edit();
        assert_eq!(state.todos.editing.as_ref().unwrap().input, "old");
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 3);

        state.todo_input_clear();
        for c in "new text".chars() {
            state.todo_input_push_char(c);
        }
        state.commit_todo_edit();

        assert_eq!(state.todos.items[0].text, "new text");
        assert!(state.todos.editing.is_none());
    }

    #[test]
    fn edit_inserts_at_the_cursor_and_tracks_it() {
        let mut state = state_with(vec![item("ab", false)]);
        state.todos.selected = 0;

        state.begin_todo_edit();
        state.todo_edit_cursor_move(-1);
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 1);

        state.todo_input_push_char('X');
        assert_eq!(state.todos.editing.as_ref().unwrap().input, "aXb");
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 2);

        state.todo_input_pop_char();
        assert_eq!(state.todos.editing.as_ref().unwrap().input, "ab");
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 1);
    }

    #[test]
    fn delete_removes_the_char_under_the_cursor() {
        let mut state = state_with(vec![item("abc", false)]);
        state.todos.selected = 0;

        state.begin_todo_edit();
        state.todo_edit_cursor_home();
        state.todo_input_delete_char();

        assert_eq!(state.todos.editing.as_ref().unwrap().input, "bc");
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 0);
    }

    #[test]
    fn edit_cursor_stays_clamped_to_the_buffer() {
        let mut state = state_with(vec![item("ab", false)]);
        state.todos.selected = 0;

        state.begin_todo_edit();
        state.todo_edit_cursor_move(-10);
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 0);
        state.todo_edit_cursor_move(10);
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 2);
        state.todo_edit_cursor_home();
        state.todo_edit_cursor_end();
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 2);
    }

    #[test]
    fn ctrl_left_walks_to_the_start_of_the_previous_word() {
        let mut state = state_with(vec![item("one two  three", false)]);
        state.todos.selected = 0;

        state.begin_todo_edit();
        // From the end: "three" starts at 9, the previous word at 4.
        state.todo_edit_cursor_word_left();
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 9);
        state.todo_edit_cursor_word_left();
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 4);
        state.todo_edit_cursor_word_left();
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 0);
        // Stays clamped at the buffer start.
        state.todo_edit_cursor_word_left();
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 0);
    }

    #[test]
    fn ctrl_right_walks_past_the_end_of_the_next_word() {
        let mut state = state_with(vec![item("one two  three", false)]);
        state.todos.selected = 0;

        state.begin_todo_edit();
        state.todo_edit_cursor_home();
        state.todo_edit_cursor_word_right();
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 3);
        state.todo_edit_cursor_word_right();
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 7);
        state.todo_edit_cursor_word_right();
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 14);
        // Stays clamped at the buffer end.
        state.todo_edit_cursor_word_right();
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 14);
    }

    #[test]
    fn ctrl_w_deletes_the_word_before_the_cursor_mid_text() {
        let mut state = state_with(vec![item("write docs now", false)]);
        state.todos.selected = 0;

        state.begin_todo_edit();
        // Cursor lands after "now"; step back to the end of "docs".
        state.todo_edit_cursor_move(-4);
        state.todo_input_pop_word();

        assert_eq!(state.todos.editing.as_ref().unwrap().input, "write  now");
        assert_eq!(state.todos.editing.as_ref().unwrap().cursor, 6);
    }

    #[test]
    fn empty_edit_keeps_the_original_text() {
        let mut state = state_with(vec![item("keep me", false)]);
        state.todos.selected = 0;

        state.begin_todo_edit();
        state.todo_input_clear();
        state.commit_todo_edit();

        assert_eq!(state.todos.items[0].text, "keep me");
        assert!(state.todos.editing.is_none());
    }

    #[test]
    fn editing_a_done_item_is_a_no_op() {
        let mut state = state_with(vec![item("a", false), item("b", true)]);
        state.global.todo_hide_completed = false;
        state.todos.selected = 1; // the done item

        state.begin_todo_edit();

        assert!(
            state.todos.editing.is_none(),
            "the editor never opens on struck-through text"
        );
        assert_eq!(state.todos.items[1].text, "b");
        assert!(state.todos.items[1].done);
    }

    #[test]
    fn escape_cancels_the_edit_without_changing_the_item() {
        let mut state = state_with(vec![item("original", false)]);
        state.todos.selected = 0;

        state.begin_todo_edit();
        state.todo_input_push_char('!');
        state.todo_escape();

        assert!(state.todos.editing.is_none());
        assert_eq!(state.todos.items[0].text, "original");
        assert_eq!(state.focus_state.focus, Focus::Todos);
    }

    #[test]
    fn move_item_swaps_with_the_open_neighbor_in_file_order() {
        let mut state = state_with(vec![
            item("a", false),
            item("b", true),
            item("c", false),
            item("d", false),
        ]);
        state.todos.selected = 2; // c

        assert!(state.move_todo_selected(-1));

        assert_eq!(
            state
                .todos
                .items
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>(),
            vec!["c", "b", "a", "d"],
            "c swaps with a across the done b; done items stay put"
        );
        assert!(state.todos.items[1].done);
        assert_eq!(state.todos.selected, 0, "selection follows the moved item");
    }

    #[test]
    fn move_item_is_a_no_op_at_the_open_block_boundaries() {
        let mut state = state_with(vec![item("a", false), item("b", true), item("c", false)]);
        state.global.todo_hide_completed = false;

        state.todos.selected = 0;
        assert!(!state.move_todo_selected(-1), "top of the open block");

        state.todos.selected = 2;
        assert!(!state.move_todo_selected(1), "bottom of the open block");
        assert_eq!(
            state
                .todos
                .items
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"],
            "nothing moved"
        );
        assert_eq!(state.todos.selected, 2);
    }

    #[test]
    fn moving_a_completed_item_is_a_no_op() {
        let mut state = state_with(vec![item("a", false), item("b", true)]);
        state.global.todo_hide_completed = false;
        state.todos.selected = 1;

        assert!(!state.move_todo_selected(1));
        assert!(!state.move_todo_selected(-1));
        assert_eq!(state.todos.selected, 1);
        assert_eq!(
            state
                .todos
                .items
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn move_item_rejects_bad_indexes_and_empty_lists() {
        let mut todos = TodoState::default();
        assert!(todos.move_item(0, 1).is_none(), "empty list");
        todos.items = vec![item("a", false), item("b", true)];
        assert!(todos.move_item(5, 1).is_none(), "bad index");
        assert!(
            todos.move_item(1, 1).is_none(),
            "completed items never reorder"
        );
    }

    #[test]
    fn move_item_persists_through_update_in() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = AppState::new("%99".into());
        state.todos.attach(Some(tmp.path().to_path_buf()));
        state.todos.add("a".into());
        state.todos.add("b".into());
        state.todos.add("c".into());
        state.todos.toggle(1, false); // b done, now interleaved between a and c
        state.todos.selected = 0;

        assert!(state.move_todo_selected(1));

        assert_eq!(
            state
                .todos
                .items
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>(),
            vec!["c", "b", "a"],
            "a moves below c, around the done b"
        );
        assert_eq!(state.todos.selected, 2);
        assert_eq!(
            crate::todo::load_in(tmp.path()).unwrap(),
            state.todos.items,
            "the on-disk file agrees with memory"
        );
    }

    #[test]
    fn clear_completed_removes_done_items_and_keeps_open_ones() {
        let mut state = state_with(vec![
            item("a", false),
            item("b", true),
            item("c", false),
            item("d", true),
        ]);
        state.todos.selected = 2;
        state.global.todo_hide_completed = true;

        state.clear_completed_todos();

        assert_eq!(state.todos.items.len(), 2);
        assert_eq!(state.todos.items[0].text, "a");
        assert_eq!(state.todos.items[1].text, "c");
        assert_eq!(state.todos.selected, 1);
    }

    #[test]
    fn delete_removes_the_selected_item_and_clamps() {
        let mut state = state_with(vec![item("a", false), item("b", false)]);
        state.todos.selected = 1;
        state.delete_todo_selected();
        assert_eq!(state.todos.items.len(), 1);
        assert_eq!(state.todos.selected, 0);
    }

    // ─── undo (delete / clear-completed) ────────────────────────────

    #[test]
    fn undo_restores_a_deleted_item_at_its_original_index() {
        let mut state = state_with(vec![item("a", false), item("b", false), item("c", false)]);
        state.todos.selected = 1;

        state.delete_todo_selected();
        assert_eq!(state.todos.items.len(), 2);
        assert!(state.todos.undo.is_some(), "the delete stashes the item");

        state.undo_todo_last();

        assert_eq!(
            state
                .todos
                .items
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"],
            "b is back in its original slot"
        );
        assert!(state.todos.undo.is_none(), "the stash is consumed");
        assert_eq!(state.todos.selected, 1, "selection lands back on b");
    }

    #[test]
    fn undo_restores_cleared_completed_items_at_their_original_indices() {
        let mut state = state_with(vec![
            item("a", true),
            item("b", false),
            item("c", true),
            item("d", false),
        ]);

        state.clear_completed_todos();
        assert_eq!(state.todos.items.len(), 2);

        state.undo_todo_last();

        assert_eq!(
            state.todos.items.len(),
            4,
            "the file order comes back whole"
        );
        assert!(state.todos.items[0].done && state.todos.items[0].text == "a");
        assert!(!state.todos.items[1].done && state.todos.items[1].text == "b");
        assert!(state.todos.items[2].done && state.todos.items[2].text == "c");
        assert!(!state.todos.items[3].done && state.todos.items[3].text == "d");
        assert!(state.todos.undo.is_none());
    }

    #[test]
    fn undo_is_a_no_op_after_the_deadline() {
        let mut state = state_with(vec![item("a", false), item("b", false)]);
        state.todos.selected = 0;
        state.delete_todo_selected();

        // Rewind the deadline into the past: the window has passed.
        state.todos.undo.as_mut().unwrap().deadline = Instant::now() - Duration::from_secs(1);

        assert!(!state.undo_todo_last());
        assert_eq!(
            state.todos.items.len(),
            1,
            "the expired stash is dropped with the item gone"
        );
        assert!(state.todos.undo.is_none());
    }

    #[test]
    fn undo_with_no_pending_stash_is_a_no_op() {
        let mut state = state_with(vec![item("a", false)]);
        assert!(!state.undo_todo_last());
        assert_eq!(state.todos.items.len(), 1);
        assert!(state.todos.undo.is_none());
    }

    #[test]
    fn a_new_destructive_action_replaces_the_undo_stash() {
        let mut state = state_with(vec![item("a", false), item("b", false), item("c", false)]);
        state.todos.selected = 1;
        state.delete_todo_selected(); // stash b
        state.todos.selected = 0;
        state.delete_todo_selected(); // stash a, replacing b

        state.undo_todo_last();

        assert_eq!(
            state
                .todos
                .items
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "c"],
            "only the newest deletion is undoable"
        );
    }

    #[test]
    fn delete_sets_no_flash_banner() {
        let mut state = state_with(vec![item("old", false), item("keep", false)]);
        state.todos.selected = 0;

        state.delete_todo_selected();

        assert_eq!(state.todos.items.len(), 1);
        assert!(
            state.flash.is_none(),
            "the list changing is the feedback; no banner"
        );
        assert!(state.todos.undo.is_some(), "the undo stash still arms");
    }

    #[test]
    fn clear_completed_flashes_the_undo_banner() {
        let mut state = state_with(vec![item("a", true), item("b", true), item("c", false)]);

        state.clear_completed_todos();

        assert_eq!(
            state.flash.as_ref().map(|(text, _)| text.as_str()),
            Some("cleared 2 completed · u to undo")
        );
    }

    #[test]
    fn a_changeless_delete_or_clear_stashes_and_flashes_nothing() {
        let mut state = state_with(Vec::new());
        state.delete_todo_selected();
        assert!(state.todos.undo.is_none());
        assert!(state.flash.is_none(), "an empty-list delete is a no-op");

        state.todos.items = vec![item("open", false)];
        state.clear_completed_todos();
        assert!(state.todos.undo.is_none());
        assert!(state.flash.is_none(), "a no-completed clear is a no-op");
    }

    #[test]
    fn undo_persists_the_restore_through_update_in() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = AppState::new("%99".into());
        state.todos.attach(Some(tmp.path().to_path_buf()));
        state.todos.add("a".into());
        state.todos.add("b".into());
        state.todos.add("c".into());
        state.todos.selected = 1;
        state.delete_todo_selected();
        assert_eq!(crate::todo::load_in(tmp.path()).unwrap().len(), 2);

        state.undo_todo_last();

        assert_eq!(
            state
                .todos
                .items
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert_eq!(
            crate::todo::load_in(tmp.path()).unwrap(),
            state.todos.items,
            "the on-disk file agrees with memory after the restore"
        );
    }

    #[test]
    fn undo_of_cleared_completed_persists_through_update_in() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = AppState::new("%99".into());
        state.todos.attach(Some(tmp.path().to_path_buf()));
        state.todos.add("a".into());
        state.todos.toggle(0, false);
        state.todos.add("b".into());
        state.todos.toggle(1, false);

        state.clear_completed_todos();
        assert!(state.todos.items.is_empty());

        state.undo_todo_last();

        assert_eq!(state.todos.items.len(), 2);
        assert!(state.todos.items[0].done && state.todos.items[0].text == "a");
        assert!(state.todos.items[1].done && state.todos.items[1].text == "b");
        assert_eq!(crate::todo::load_in(tmp.path()).unwrap(), state.todos.items,);
    }

    #[test]
    fn undo_after_a_failed_persistence_delete_does_not_duplicate_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = AppState::new("%99".into());
        state.todos.attach(Some(tmp.path().to_path_buf()));
        state.todos.add("a".into());
        state.todos.add("b".into());
        state.todos.add("c".into());
        state.todos.selected = 1;

        // Break writes only: swapping the lock file for a directory makes
        // update_in fail at the lock (O_RDWR on a directory) while the
        // list file — and its stamp — stay readable.
        std::fs::remove_file(tmp.path().join("todos.lock")).unwrap();
        std::fs::create_dir(tmp.path().join("todos.lock")).unwrap();

        state.delete_todo_selected();
        assert_eq!(state.todos.items.len(), 2, "the delete applied in memory");
        assert!(state.todos.error.is_some(), "the save failure is surfaced");

        state.undo_todo_last();

        assert_eq!(
            state
                .todos
                .items
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"],
            "memory is restored"
        );
        assert_eq!(
            crate::todo::load_in(tmp.path()).unwrap(),
            vec![item("a", false), item("b", false), item("c", false)],
            "the file never lost b, so the restore must not write a second copy"
        );
        assert!(
            state.todos.error.is_none(),
            "memory is back in sync with the file, so the error retires"
        );
    }

    #[test]
    fn undo_drops_the_stash_when_the_file_changed_since_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = AppState::new("%99".into());
        state.todos.attach(Some(tmp.path().to_path_buf()));
        state.todos.add("a".into());
        state.todos.add("b".into());
        state.todos.selected = 0;
        state.delete_todo_selected();

        // Another instance edits the list while the window is open; the
        // per-tick refresh picks it up and moves the stamp.
        crate::todo::update_in(tmp.path(), |items| {
            items.push(TodoItem {
                text: "c".into(),
                done: false,
            });
        })
        .unwrap();
        state
            .todos
            .refresh_if_changed(state.global.todo_hide_completed);

        assert!(!state.undo_todo_last(), "the stale stash is refused");

        assert_eq!(
            state
                .todos
                .items
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "c"],
            "the external state stands; the deleted item stays gone"
        );
        assert!(state.todos.undo.is_none());
    }

    #[test]
    fn undo_refuses_an_external_write_without_waiting_for_the_refresh_tick() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = AppState::new("%99".into());
        state.todos.attach(Some(tmp.path().to_path_buf()));
        state.todos.add("a".into());
        state.todos.add("b".into());
        state.todos.selected = 0;
        state.delete_todo_selected();

        // Another instance writes while the per-tick stamp cache is still
        // stale: no refresh_if_changed runs, so the cached stamp still
        // matches the stash — the fresh stat at undo time must catch it.
        crate::todo::update_in(tmp.path(), |items| {
            items.push(TodoItem {
                text: "c".into(),
                done: false,
            });
        })
        .unwrap();

        assert!(!state.undo_todo_last(), "the restore is refused");

        assert_eq!(
            state
                .todos
                .items
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>(),
            vec!["b"],
            "nothing is resurrected over the newer external state"
        );
        assert_eq!(
            crate::todo::load_in(tmp.path()).unwrap().len(),
            2,
            "the external write stands untouched"
        );
        assert!(state.todos.undo.is_none());
    }

    #[test]
    fn navigation_skips_hidden_completed_items() {
        let mut state = state_with(vec![item("a", false), item("b", true), item("c", false)]);
        state.global.todo_hide_completed = true;
        state.todos.selected = 0;

        assert!(state.move_todo_selection(1));
        assert_eq!(state.todos.selected, 2);
        assert!(!state.move_todo_selection(1), "clamped at the end");
    }

    #[test]
    fn escape_cancels_add_before_leaving_focus() {
        let mut state = state_with(vec![]);
        state.begin_todo_add();
        state.todo_escape();
        assert!(state.todos.adding.is_none());
        assert_eq!(state.focus_state.focus, Focus::Todos);

        state.todo_escape();
        assert_eq!(state.focus_state.focus, Focus::Panes);
        assert!(state.todos.visible, "panel stays visible after Esc");
    }

    #[test]
    fn ctrl_w_deletes_the_word_before_the_cursor() {
        let mut state = state_with(vec![]);
        state.begin_todo_add();
        for c in "write docs now".chars() {
            state.todo_input_push_char(c);
        }
        state.todo_input_pop_word();
        assert_eq!(state.todos.adding.as_deref(), Some("write docs "));
        state.todo_input_pop_word();
        assert_eq!(state.todos.adding.as_deref(), Some("write "));
    }
}
