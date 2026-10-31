use std::path::PathBuf;
use std::time::SystemTime;

use super::{AppState, Focus};
use crate::todo::TodoItem;

/// In-progress text edit for one item, opened with `e`.
#[derive(Debug, Clone)]
pub struct TodoEdit {
    pub index: usize,
    pub input: String,
}

/// Manual TODO list state for one sidebar instance.
///
/// The item list itself is shared across instances through the file
/// backing `dir`; `visible`, `selected`, and the add-line buffer are
/// per-instance. `dir` is `None` in unit-test builds (and when no state
/// directory is available), which keeps every mutation in memory so the
/// tests never touch the user's real list.
#[derive(Debug, Clone, Default)]
pub struct TodoState {
    pub items: Vec<TodoItem>,
    /// Selected item index into `items` (not into the visible subset).
    pub selected: usize,
    /// Panel visibility. Never persisted: every sidebar starts hidden.
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

    /// Indices into `items` that should render, honoring hide-completed.
    pub fn visible_indices(&self, hide_completed: bool) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| !(hide_completed && item.done))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn open_count(&self) -> usize {
        self.items.iter().filter(|item| !item.done).count()
    }

    pub fn total_count(&self) -> usize {
        self.items.len()
    }

    /// Keep the selection on a visible item. When the selected item was
    /// hidden (completed under hide-completed, deleted, or removed by an
    /// external edit), move to the nearest visible item at or after the
    /// old position, falling back to the last one.
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

    /// Delete every completed item (both visible and hidden).
    pub fn clear_completed(&mut self, hide_completed: bool) {
        if !self.items.iter().any(|item| item.done) {
            return;
        }
        match self.dir.clone() {
            None => self.items.retain(|item| !item.done),
            Some(dir) => match crate::todo::update_in(&dir, |items| {
                items.retain(|item| !item.done);
            }) {
                Ok(items) => {
                    self.items = items;
                    self.file_stamp = crate::todo::stamp_in(&dir);
                    self.error = None;
                }
                Err(err) => {
                    self.items.retain(|item| !item.done);
                    self.error = Some(format!("save failed: {err}"));
                }
            },
        }
        self.clamp_selection(hide_completed);
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

    /// Delete the item at `index`.
    pub fn delete(&mut self, index: usize, hide_completed: bool) {
        if index >= self.items.len() {
            return;
        }
        match self.dir.clone() {
            None => {
                self.items.remove(index);
            }
            Some(dir) => match crate::todo::update_in(&dir, |items| {
                if index < items.len() {
                    items.remove(index);
                }
            }) {
                Ok(items) => {
                    self.items = items;
                    self.file_stamp = crate::todo::stamp_in(&dir);
                    self.error = None;
                }
                Err(err) => {
                    self.items.remove(index);
                    self.error = Some(format!("save failed: {err}"));
                }
            },
        }
        self.clamp_selection(hide_completed);
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
            self.todos.visible = true;
            self.todos.adding = None;
            self.todos.editing = None;
            self.focus_state.focus = Focus::Todos;
            self.todos.clamp_selection(self.global.todo_hide_completed);
        } else if self.focus_state.focus == Focus::Todos {
            self.todos.visible = false;
            self.todos.adding = None;
            self.todos.editing = None;
            self.focus_state.focus = Focus::Panes;
        } else {
            self.focus_state.focus = Focus::Todos;
            self.todos.clamp_selection(self.global.todo_hide_completed);
        }
    }

    /// Esc inside the panel: cancel the edit line, then the add line,
    /// otherwise hand focus back to the agent list and leave the panel
    /// visible.
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

    pub fn page_todo_selection(&mut self, direction: isize) -> bool {
        let page = self.scrolls.todos.visible_height.max(1) as isize;
        self.todos
            .move_selection(direction * page, self.global.todo_hide_completed)
    }

    pub fn toggle_todo_selected(&mut self) {
        let index = self.todos.selected;
        self.todos.toggle(index, self.global.todo_hide_completed);
    }

    pub fn delete_todo_selected(&mut self) {
        let index = self.todos.selected;
        self.todos.delete(index, self.global.todo_hide_completed);
    }

    pub fn clear_completed_todos(&mut self) {
        self.todos.clear_completed(self.global.todo_hide_completed);
    }

    /// Open the inline add line, focusing the panel first.
    pub fn begin_todo_add(&mut self) {
        self.todos.visible = true;
        self.focus_state.focus = Focus::Todos;
        self.todos.editing = None;
        self.todos.adding = Some(String::new());
    }

    /// Commit the add line and immediately start a fresh one so several
    /// items can be entered in a row (Google Tasks style). A
    /// whitespace-only line adds nothing but keeps the input open; Esc
    /// ends the streak.
    pub fn commit_todo_add(&mut self) {
        let Some(input) = self.todos.adding.take() else {
            return;
        };
        if !input.trim().is_empty() {
            self.todos.add(input);
            self.todos.clamp_selection(self.global.todo_hide_completed);
        }
        self.todos.adding = Some(String::new());
    }

    pub fn cancel_todo_add(&mut self) {
        self.todos.adding = None;
    }

    /// Open the edit line for the selected item, prefilled with its text.
    pub fn begin_todo_edit(&mut self) {
        let index = self.todos.selected;
        let Some(item) = self.todos.items.get(index) else {
            return;
        };
        let input = item.text.clone();
        self.todos.adding = None;
        self.todos.editing = Some(TodoEdit { index, input });
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

    pub fn cancel_todo_edit(&mut self) {
        self.todos.editing = None;
    }

    /// The buffer owning printable keys right now: the edit line when an
    /// item is being edited, otherwise the add line.
    fn todo_input_mut(&mut self) -> Option<&mut String> {
        if let Some(edit) = &mut self.todos.editing {
            Some(&mut edit.input)
        } else {
            self.todos.adding.as_mut()
        }
    }

    pub fn todo_input_push_char(&mut self, c: char) {
        if let Some(input) = self.todo_input_mut() {
            input.push(c);
        }
    }

    pub fn todo_input_pop_char(&mut self) {
        if let Some(input) = self.todo_input_mut() {
            input.pop();
        }
    }

    pub fn todo_input_clear(&mut self) {
        if let Some(input) = self.todo_input_mut() {
            input.clear();
        }
    }

    /// Delete the word before the cursor (Ctrl-w).
    pub fn todo_input_pop_word(&mut self) {
        let Some(input) = self.todo_input_mut() else {
            return;
        };
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
        assert!(state.todos.items.is_empty());
        assert!(state.todos.dir.is_none());
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
    fn visible_indices_hide_completed() {
        let state = state_with(vec![item("a", false), item("b", true), item("c", false)]);
        assert_eq!(state.todos.visible_indices(false), vec![0, 1, 2]);
        assert_eq!(state.todos.visible_indices(true), vec![0, 2]);
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
    fn blank_add_is_ignored() {
        let mut state = state_with(vec![]);
        state.begin_todo_add();
        state.todo_input_push_char(' ');
        state.commit_todo_add();
        assert!(state.todos.items.is_empty());
        assert_eq!(
            state.todos.adding.as_deref(),
            Some(""),
            "a blank Enter keeps the input open"
        );
    }

    #[test]
    fn edit_prefills_and_saves_the_new_text() {
        let mut state = state_with(vec![item("old", false)]);
        state.todos.selected = 0;

        state.begin_todo_edit();
        assert_eq!(state.todos.editing.as_ref().unwrap().input, "old");

        state.todo_input_clear();
        for c in "new text".chars() {
            state.todo_input_push_char(c);
        }
        state.commit_todo_edit();

        assert_eq!(state.todos.items[0].text, "new text");
        assert!(state.todos.editing.is_none());
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
