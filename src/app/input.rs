use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};

use crate::state::{AppState, Focus};
use crate::worktree::RemoveMode;

/// Dispatch a single crossterm [`Event`] into the [`AppState`], returning
/// `true` when a redraw should be scheduled.
pub(super) fn handle_event(ev: Event, state: &mut AppState) -> bool {
    match ev {
        Event::Key(key) => handle_key_event(key, state),
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                state.handle_mouse_click(mouse.row, mouse.column);
                true
            }
            MouseEventKind::ScrollDown => {
                state.handle_mouse_scroll(3, mouse.row);
                true
            }
            MouseEventKind::ScrollUp => {
                state.handle_mouse_scroll(-3, mouse.row);
                true
            }
            // Motion, drag, and button-release events mutate nothing; only
            // the kinds above change state, and only they may schedule a
            // redraw. Returning `true` for every mouse event used to force
            // a full frame render on each movement burst.
            _ => false,
        },
        _ => false,
    }
}

/// Dispatch a single [`KeyEvent`]. Split out from [`handle_event`] so that
/// unit tests can drive the keyboard path without constructing a real
/// terminal handle.
pub(super) fn handle_key_event(key: KeyEvent, state: &mut AppState) -> bool {
    if state.is_notices_popup_open() {
        if key.code == KeyCode::Esc {
            state.close_notices_popup();
        }
        return true;
    }
    if state.is_help_open() {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') => state.close_help_popup(),
            // The overlay is a full-sidebar modal: it scrolls like any
            // narrow column of text.
            KeyCode::Char('j') | KeyCode::Down => state.scroll_help(1),
            KeyCode::Char('k') | KeyCode::Up => state.scroll_help(-1),
            KeyCode::PageDown => state.scroll_help_page(1),
            KeyCode::PageUp => state.scroll_help_page(-1),
            _ => {}
        }
        return true;
    }
    if state.is_spawn_input_open() {
        // While the spawn runs on its worker thread the modal must not
        // accept input: no double-submit, no edits the result would
        // clobber, no dismissal that strands the feedback.
        if state.is_spawn_input_working() {
            return true;
        }
        match key.code {
            KeyCode::Esc => state.close_spawn_input(),
            KeyCode::Enter => state.confirm_spawn_input(),
            KeyCode::Tab | KeyCode::Down => state.spawn_input_next_field(),
            KeyCode::BackTab | KeyCode::Up => state.spawn_input_prev_field(),
            KeyCode::Left => state.spawn_input_cycle(-1),
            KeyCode::Right => state.spawn_input_cycle(1),
            KeyCode::Backspace => state.spawn_input_pop_char(),
            KeyCode::Char(c) => state.spawn_input_push_char(c),
            _ => {}
        }
        return true;
    }
    if state.is_remove_confirm_open() {
        if state.is_remove_confirm_working() {
            return true;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') => state.close_remove_confirm(),
            KeyCode::Char('c') => state.confirm_remove(RemoveMode::WindowOnly),
            KeyCode::Enter | KeyCode::Char('y') => {
                state.confirm_remove(RemoveMode::WindowAndWorktree)
            }
            _ => {}
        }
        return true;
    }
    if state.is_repo_popup_open() {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => state.close_repo_popup(),
            KeyCode::Char('j') | KeyCode::Down => repo_popup_nav_down(state),
            KeyCode::Char('n') if ctrl => repo_popup_nav_down(state),
            KeyCode::Char('k') | KeyCode::Up => repo_popup_nav_up(state),
            KeyCode::Char('p') if ctrl => repo_popup_nav_up(state),
            KeyCode::Enter => state.confirm_repo_popup(),
            _ => {}
        }
        return true;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // Undo window: while the last delete / clear-completed is still
    // fresh, a plain `u` (Google's ⌘Z, terminal-flavored) restores it
    // from any focus that is not a text input. The TODO add/edit line
    // and the search line type `u` / `z` into their buffers instead
    // (they own the keyboard while open), and the popups above own
    // theirs. Outside the window the arm is inert — `u` is not bound
    // anywhere else, so this is a transient claim, not a global
    // rebinding.
    let undo_key = (key.code == KeyCode::Char('u') && key.modifiers == KeyModifiers::NONE)
        || (key.code == KeyCode::Char('z') && ctrl);
    if undo_key {
        let input_open = state.todos.adding.is_some()
            || state.todos.editing.is_some()
            || state.search.is_active();
        if !input_open && state.undo_todo_last() {
            return true;
        }
    }

    // The TODO panel owns its keys while it has focus: j/k navigate items,
    // Ctrl-↑/↓ (and Ctrl-j/Ctrl-k) reorder the selected item within the
    // open block, Enter edits the selected task, Space toggles its
    // checkbox, a adds, x/d delete, D clears completed, H flips
    // hide-completed. Global keys it does not claim (q, c, w, ?, /) still
    // work. The add line, when open, owns printable keys; an open edit
    // owns them on the item's own rows.
    if state.focus_state.focus == Focus::Todos {
        return handle_todo_key(key, state);
    }

    // The `/` input line owns the keyboard while open: printable keys edit
    // the query (including `q`, `?`, and `/`), Up/Down still navigate the
    // filtered list, Esc clears the query and closes, Enter closes but
    // keeps the query applied, and Ctrl-u clears the query in place.
    if state.search.is_active() {
        match key.code {
            KeyCode::Esc => state.cancel_search(),
            KeyCode::Enter => state.finish_search(),
            KeyCode::Backspace => state.search_pop_char(),
            // Navigate the filtered list without leaving the input line.
            // Use the raw selection move (not `pane_nav_up`), which would
            // hand focus back to the status row at the top of the list.
            KeyCode::Up => {
                state.move_pane_selection(-1);
            }
            KeyCode::Down => {
                state.move_pane_selection(1);
            }
            KeyCode::Char('n') if ctrl => {
                state.move_pane_selection(1);
            }
            KeyCode::Char('p') if ctrl => {
                state.move_pane_selection(-1);
            }
            KeyCode::Char('u') if ctrl => state.clear_search_query(),
            KeyCode::Char('w') if ctrl => state.search_pop_word(),
            KeyCode::Char(c) if !ctrl => state.search_push_char(c),
            _ => {}
        }
        return true;
    }

    match key.code {
        KeyCode::Esc => {
            // Esc cancels an applied search first; only when the list is
            // unfiltered does it fall back to returning focus from the
            // status row.
            if state.search.has_query() {
                state.cancel_search();
            } else if state.focus_state.focus == Focus::Filter {
                state.focus_state.focus = Focus::Panes;
            }
        }
        KeyCode::Char('/') => state.open_search(),
        KeyCode::Char('q') => state.quit_requested = true,
        KeyCode::Char('j') | KeyCode::Down => pane_nav_down(state),
        KeyCode::Char('n') if ctrl => pane_nav_down(state),
        KeyCode::Char('k') | KeyCode::Up => pane_nav_up(state),
        KeyCode::Char('p') if ctrl => pane_nav_up(state),
        KeyCode::Char('g') => {
            if state.focus_state.focus == Focus::Panes && state.select_first_row() {
                state.global.queue_cursor_save();
            }
        }
        KeyCode::Char('G') => {
            if state.focus_state.focus == Focus::Panes && state.select_last_row() {
                state.global.queue_cursor_save();
            }
        }
        KeyCode::Char('a') => {
            if state.focus_state.focus == Focus::Panes && state.jump_to_next_attention() {
                state.global.queue_cursor_save();
            }
        }
        KeyCode::Char(']') => {
            if state.focus_state.focus == Focus::Panes && state.jump_to_next_group() {
                state.global.queue_cursor_save();
            }
        }
        KeyCode::Char('[') => {
            if state.focus_state.focus == Focus::Panes && state.jump_to_previous_group() {
                state.global.queue_cursor_save();
            }
        }
        // Ctrl-z is the undo alias above; a fall-through here (no
        // pending undo) must not collapse a group.
        KeyCode::Char('z') if !ctrl => {
            if state.focus_state.focus == Focus::Panes && state.toggle_selected_group_collapse() {
                state.global.queue_cursor_save();
            }
        }
        KeyCode::Char('Z') => {
            if state.focus_state.focus == Focus::Panes && state.expand_all_groups() {
                state.global.queue_cursor_save();
            }
        }
        // Grouping is a shared view setting, like `c` / `w`: write the
        // tmux global and let every open sidebar reload it. Re-query the
        // snapshot here so this sidebar regroups immediately instead of
        // waiting for the next tick.
        KeyCode::Char('s') => {
            state.global.toggle_group_by();
            state.collapsed_groups.clear();
            state.refresh();
        }
        KeyCode::PageDown => page_scroll(state, 1),
        KeyCode::PageUp => page_scroll(state, -1),
        KeyCode::Char('h') | KeyCode::Left => {
            if state.focus_state.focus == Focus::Filter {
                state.global.status_filter = state.global.status_filter.prev();
                state.global.save_filter();
                state.rebuild_row_targets();
            }
        }
        KeyCode::Char('l') | KeyCode::Right => {
            if state.focus_state.focus == Focus::Filter {
                state.global.status_filter = state.global.status_filter.next();
                state.global.save_filter();
                state.rebuild_row_targets();
            } else if state.focus_state.focus == Focus::Panes {
                // "Go into" the agent: Right (and its vim pair `l`) mirrors
                // Enter's jump-to-pane from the agent list.
                state.activate_selected_pane();
            }
        }
        KeyCode::Char('r') => {
            if state.focus_state.focus == Focus::Filter {
                state.toggle_repo_popup();
            }
        }
        KeyCode::Char('n') => {
            if state.focus_state.focus == Focus::Panes {
                state.open_spawn_input_from_selection();
            }
        }
        KeyCode::Char('x') => {
            if state.focus_state.focus == Focus::Panes {
                state.open_remove_confirm();
            }
        }
        // Flag the selected row as an "inbox pin" (orange gutter marker).
        // Popups own their keys before this match, and `f` is only bound
        // under Panes focus, so it never swallows other input.
        KeyCode::Char('f') => {
            if state.focus_state.focus == Focus::Panes {
                state.toggle_selected_flag();
            }
        }
        // Toggles are self-evident in the pane list; `?` explains them.
        KeyCode::Char('c') => {
            state.global.toggle_compact();
        }
        KeyCode::Char('w') => {
            state.global.toggle_show_windows();
            state.rebuild_row_targets();
        }
        KeyCode::Char('t') => {
            state.toggle_todo_panel();
        }
        KeyCode::Char('?') => {
            state.toggle_help_popup();
        }
        KeyCode::Enter => {
            if state.focus_state.focus == Focus::Panes {
                state.activate_selected_pane();
            }
        }
        KeyCode::Tab => {
            state.global.status_filter = state.global.status_filter.next();
            state.global.save_filter();
            state.rebuild_row_targets();
        }
        _ => {}
    }
    true
}

/// Keys owned by the manual TODO panel. See [`Focus::Todos`].
///
/// The flow follows Google Tasks: Enter edits the selected task, Enter
/// again (or in the add line) saves and moves on to the next task, Space
/// completes, Esc backs out without creating anything.
fn handle_todo_key(key: KeyEvent, state: &mut AppState) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    if state.todos.editing.is_some() {
        match key.code {
            KeyCode::Esc => state.todo_escape(),
            // The editor's newline: save and land on a fresh task.
            KeyCode::Enter => state.commit_todo_edit_and_next(),
            KeyCode::Backspace => state.todo_input_pop_char(),
            KeyCode::Delete => state.todo_input_delete_char(),
            // Plain Home/End and their readline equivalents (Ctrl-a/e).
            // The Ctrl aliases must be separate arms: a guard on an
            // or-pattern applies to every alternative, which once left
            // plain Home/End matching nothing.
            KeyCode::Home => state.todo_edit_cursor_home(),
            KeyCode::Char('a') if ctrl => state.todo_edit_cursor_home(),
            KeyCode::End => state.todo_edit_cursor_end(),
            KeyCode::Char('e') if ctrl => state.todo_edit_cursor_end(),
            // Word-wise navigation, readline style.
            KeyCode::Left if ctrl => state.todo_edit_cursor_word_left(),
            KeyCode::Right if ctrl => state.todo_edit_cursor_word_right(),
            KeyCode::Left => state.todo_edit_cursor_move(-1),
            KeyCode::Right => state.todo_edit_cursor_move(1),
            KeyCode::Char('u') if ctrl => state.todo_input_clear(),
            KeyCode::Char('w') if ctrl => state.todo_input_pop_word(),
            KeyCode::Char(c) if !ctrl => state.todo_input_push_char(c),
            _ => {}
        }
        return true;
    }

    if state.todos.adding.is_some() {
        match key.code {
            KeyCode::Esc => state.todo_escape(),
            // Commits the task and keeps a fresh line open; an empty
            // buffer ends the streak instead (see `commit_todo_add`).
            KeyCode::Enter => state.commit_todo_add(),
            // Google Tasks' backspace: on the empty new-task line the
            // caret returns to the previous task, insert mode intact.
            KeyCode::Backspace => state.todo_input_backspace(),
            KeyCode::Char('u') if ctrl => state.todo_input_clear(),
            KeyCode::Char('w') if ctrl => state.todo_input_pop_word(),
            KeyCode::Char(c) if !ctrl => state.todo_input_push_char(c),
            _ => {}
        }
        return true;
    }

    match key.code {
        KeyCode::Esc => state.todo_escape(),
        KeyCode::Char('t') => state.toggle_todo_panel(),
        KeyCode::Char('q') => state.quit_requested = true,
        // Google's Return: edit the selected task; with nothing selected
        // it starts a new one. Space is the complete toggle.
        KeyCode::Enter => state.todo_enter(),
        KeyCode::Char(' ') => state.toggle_todo_selected(),
        // Reorder the selected item within the open block. Google maps
        // this to Ctrl-↑/↓; the vim-ish Ctrl-j/Ctrl-k do the same.
        //
        // Ctrl-j is byte 0x0A, which only means Ctrl-j because the TUI
        // runs in raw mode: crossterm maps cooked-mode 0x0A to Enter but
        // raw-mode 0x0A to Char('j') + CONTROL (Enter itself is 0x0D), so
        // reorder can never collide with the Enter edit above.
        KeyCode::Up | KeyCode::Char('k') if ctrl => {
            state.move_todo_selected(-1);
        }
        KeyCode::Down | KeyCode::Char('j') if ctrl => {
            state.move_todo_selected(1);
        }
        KeyCode::Char('j') | KeyCode::Down => {
            state.move_todo_selection(1);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if !state.move_todo_selection(-1) && state.focus_panes_from_todos() {
                // Off the top of the TODO list and onto the last agent,
                // as one continuous list.
                state.global.queue_cursor_save();
            }
        }
        KeyCode::PageDown => {
            state.page_todo_selection(1);
        }
        KeyCode::PageUp => {
            state.page_todo_selection(-1);
        }
        KeyCode::Char('e') => state.begin_todo_edit(),
        KeyCode::Char('a') => state.begin_todo_add(),
        KeyCode::Char('x') | KeyCode::Char('d') => state.delete_todo_selected(),
        KeyCode::Char('D') => state.clear_completed_todos(),
        KeyCode::Char('H') => state.toggle_todo_hide_completed(),
        // Global keys stay available while the panel has focus.
        KeyCode::Char('c') => state.global.toggle_compact(),
        KeyCode::Char('w') => {
            state.global.toggle_show_windows();
            state.rebuild_row_targets();
        }
        KeyCode::Char('?') => state.toggle_help_popup(),
        KeyCode::Char('/') => state.open_search(),
        _ => {}
    }
    true
}

fn pane_nav_down(state: &mut AppState) {
    match state.focus_state.focus {
        Focus::Filter => {
            state.focus_state.focus = Focus::Panes;
        }
        Focus::Panes => {
            if state.move_pane_selection(1) {
                state.global.queue_cursor_save();
            } else if state.focus_todo_from_panes() {
                // Off the bottom of the agent list and into the visible
                // TODO panel, as one continuous list.
                state.global.queue_cursor_save();
            }
        }
        Focus::Todos => {}
    }
}

/// PageUp/PageDown: a viewport-sized jump in whatever has focus. The
/// agent list moves the selection (the render's auto-scroll keeps it in
/// view, so the viewport follows). No-op from the filter row, where h/l
/// own horizontal movement.
fn page_scroll(state: &mut AppState, direction: isize) {
    match state.focus_state.focus {
        Focus::Panes => {
            if state.page_pane_selection(direction) {
                state.global.queue_cursor_save();
            }
        }
        Focus::Filter | Focus::Todos => {}
    }
}

fn pane_nav_up(state: &mut AppState) {
    match state.focus_state.focus {
        Focus::Filter => {}
        Focus::Panes => {
            if state.move_pane_selection(-1) {
                state.global.queue_cursor_save();
            } else {
                state.focus_state.focus = Focus::Filter;
            }
        }
        Focus::Todos => {}
    }
}

fn repo_popup_nav_down(state: &mut AppState) {
    let count = state.repo_names().len();
    let current = state.repo_popup_selected();
    if current + 1 < count {
        state.set_repo_popup_selected(current + 1);
    }
}

fn repo_popup_nav_up(state: &mut AppState) {
    let current = state.repo_popup_selected();
    if current > 0 {
        state.set_repo_popup_selected(current - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::{PaneGitInfo, RepoGroup};
    use crate::state::{PopupState, RowTarget, SpawnField};
    use crate::tmux::{
        AgentType, PANE_FLAGGED, PaneAttention, PaneInfo, PaneStatus, PermissionMode,
        WorktreeMetadata, test_mock,
    };

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// Build an AppState with three navigable pane rows and Panes focus,
    /// which is the precondition the navigation arms operate against.
    fn state_with_three_panes() -> AppState {
        let mut state = AppState::new("%99".into());
        state.layout.pane_row_targets = vec![
            RowTarget {
                pane_id: "%1".into(),
                is_window: false,
            },
            RowTarget {
                pane_id: "%2".into(),
                is_window: false,
            },
            RowTarget {
                pane_id: "%3".into(),
                is_window: false,
            },
        ];
        state.global.selected_pane_row = 0;
        state.focus_state.focus = Focus::Panes;
        state
    }

    fn state_with_repo_popup_open() -> AppState {
        let mut state = AppState::new("%99".into());
        // toggle_repo_popup uses repo_names(), which always includes the
        // "All" sentinel — pad with two named groups so the selection has
        // somewhere to move.
        state.repo_groups = vec![
            RepoGroup {
                name: "repo-a".into(),
                has_focus: false,
                panes: vec![],
            },
            RepoGroup {
                name: "repo-b".into(),
                has_focus: false,
                panes: vec![],
            },
        ];
        state.toggle_repo_popup();
        state.set_repo_popup_selected(0);
        state
    }

    #[test]
    fn ctrl_n_moves_pane_selection_down() {
        let mut state = state_with_three_panes();
        handle_key_event(ctrl_key('n'), &mut state);
        assert_eq!(state.global.selected_pane_row, 1);
        handle_key_event(ctrl_key('n'), &mut state);
        assert_eq!(state.global.selected_pane_row, 2);
    }

    #[test]
    fn ctrl_p_moves_pane_selection_up() {
        let mut state = state_with_three_panes();
        state.global.selected_pane_row = 2;
        handle_key_event(ctrl_key('p'), &mut state);
        assert_eq!(state.global.selected_pane_row, 1);
        handle_key_event(ctrl_key('p'), &mut state);
        assert_eq!(state.global.selected_pane_row, 0);
    }

    #[test]
    fn bare_j_and_k_still_navigate_panes() {
        let mut state = state_with_three_panes();
        handle_key_event(key(KeyCode::Char('j')), &mut state);
        assert_eq!(state.global.selected_pane_row, 1);
        handle_key_event(key(KeyCode::Char('k')), &mut state);
        assert_eq!(state.global.selected_pane_row, 0);
    }

    #[test]
    fn right_arrow_jumps_to_selected_pane_from_panes_focus() {
        // Under `cargo test` tmux IPC is disabled at the choke point, so
        // activate_selected_pane's select-pane/switch-client calls are
        // inert; the observable effect is the focused_pane_id it records.
        let mut state = state_with_three_panes();
        assert_eq!(state.focus_state.focused_pane_id, None);

        handle_key_event(key(KeyCode::Right), &mut state);
        assert_eq!(state.focus_state.focused_pane_id, Some("%1".into()));

        // `l` mirrors Right, matching the j/k ↔ Down/Up pairing.
        handle_key_event(key(KeyCode::Char('l')), &mut state);
        assert_eq!(state.focus_state.focused_pane_id, Some("%1".into()));
    }

    #[test]
    fn bare_n_does_not_move_selection() {
        // The bare `n` arm is wired to the spawn input flow, not navigation.
        // We don't assert the popup opens (that requires repo_groups +
        // git metadata, exercised elsewhere) — only that it does NOT
        // shadow the Ctrl-N navigation arm.
        let mut state = state_with_three_panes();
        handle_key_event(key(KeyCode::Char('n')), &mut state);
        assert_eq!(state.global.selected_pane_row, 0);
    }

    #[test]
    fn bare_p_is_unbound_in_panes_focus() {
        let mut state = state_with_three_panes();
        state.global.selected_pane_row = 1;
        handle_key_event(key(KeyCode::Char('p')), &mut state);
        assert_eq!(state.global.selected_pane_row, 1);
    }

    // ─── agent list ↔ TODO list continuous navigation ───────────────

    fn todo_items(texts: &[&str]) -> Vec<crate::todo::TodoItem> {
        texts
            .iter()
            .map(|text| crate::todo::TodoItem {
                text: (*text).into(),
                done: false,
            })
            .collect()
    }

    fn todo_texts(state: &AppState) -> Vec<&str> {
        state.todos.items.iter().map(|i| i.text.as_str()).collect()
    }

    #[test]
    fn j_at_last_agent_walks_into_the_visible_todo_panel() {
        let mut state = state_with_three_panes();
        state.global.selected_pane_row = 2;
        state.todos.visible = true;
        state.todos.items = todo_items(&["a", "b"]);

        handle_key_event(key(KeyCode::Char('j')), &mut state);

        assert_eq!(state.focus_state.focus, Focus::Todos);
        assert_eq!(state.todos.selected, 0, "lands on the first todo");
        assert_eq!(state.global.selected_pane_row, 2);
    }

    #[test]
    fn j_at_last_agent_does_not_open_a_hidden_todo_panel() {
        let mut state = state_with_three_panes();
        state.global.selected_pane_row = 2;

        handle_key_event(key(KeyCode::Char('j')), &mut state);

        assert_eq!(state.focus_state.focus, Focus::Panes);
        assert!(!state.todos.visible, "the walk must not open the panel");
        assert_eq!(state.global.selected_pane_row, 2);
    }

    #[test]
    fn k_at_top_todo_walks_to_the_last_agent() {
        let mut state = state_with_three_panes();
        state.global.selected_pane_row = 0; // stale position from earlier
        state.todos.visible = true;
        state.todos.items = todo_items(&["a", "b"]);
        state.todos.selected = 0;
        state.focus_state.focus = Focus::Todos;

        handle_key_event(key(KeyCode::Char('k')), &mut state);

        assert_eq!(state.focus_state.focus, Focus::Panes);
        assert_eq!(state.global.selected_pane_row, 2, "lands on the last agent");
    }

    #[test]
    fn k_on_a_middle_todo_still_moves_between_todos() {
        let mut state = state_with_three_panes();
        state.todos.visible = true;
        state.todos.items = todo_items(&["a", "b"]);
        state.todos.selected = 1;
        state.focus_state.focus = Focus::Todos;

        handle_key_event(key(KeyCode::Char('k')), &mut state);

        assert_eq!(state.focus_state.focus, Focus::Todos);
        assert_eq!(state.todos.selected, 0);
        assert_eq!(state.global.selected_pane_row, 0, "pane row untouched");
    }

    #[test]
    fn agent_todo_walk_round_trip() {
        // Down through the whole agent list into the todos, back up
        // through the todos into the agents, ending where it began.
        let mut state = state_with_three_panes();
        state.todos.visible = true;
        state.todos.items = todo_items(&["a", "b"]);

        for _ in 0..4 {
            handle_key_event(key(KeyCode::Char('j')), &mut state);
        }
        assert_eq!(state.focus_state.focus, Focus::Todos);
        assert_eq!(state.todos.selected, 1);

        for _ in 0..5 {
            handle_key_event(key(KeyCode::Char('k')), &mut state);
        }
        assert_eq!(state.focus_state.focus, Focus::Filter, "all the way up");
        for _ in 0..5 {
            handle_key_event(key(KeyCode::Char('j')), &mut state);
        }
        assert_eq!(state.focus_state.focus, Focus::Todos);
        assert_eq!(state.todos.selected, 1);
    }

    #[test]
    fn page_down_moves_selection_by_a_page() {
        let mut state = state_with_three_panes();
        state.scrolls.panes.visible_height = 2;

        handle_key_event(key(KeyCode::PageDown), &mut state);

        assert_eq!(state.global.selected_pane_row, 2);
    }

    #[test]
    fn page_keys_clamp_at_the_list_edges() {
        let mut state = state_with_three_panes();
        state.global.selected_pane_row = 2;
        state.scrolls.panes.visible_height = 10;
        handle_key_event(key(KeyCode::PageDown), &mut state);
        assert_eq!(state.global.selected_pane_row, 2, "clamped to last row");

        state.global.selected_pane_row = 0;
        handle_key_event(key(KeyCode::PageUp), &mut state);
        assert_eq!(state.global.selected_pane_row, 0, "clamped to first row");
    }

    #[test]
    fn page_up_moves_selection_up_by_a_page() {
        let mut state = state_with_three_panes();
        state.global.selected_pane_row = 2;
        state.scrolls.panes.visible_height = 2;

        handle_key_event(key(KeyCode::PageUp), &mut state);

        assert_eq!(state.global.selected_pane_row, 0);
    }

    // ─── group / attention navigation ──────────────────────────────

    fn set_pane_attention(state: &mut AppState, pane_id: &str, attention: PaneAttention) {
        for group in &mut state.repo_groups {
            for (pane, _) in &mut group.panes {
                if pane.pane_id == pane_id {
                    pane.attention = attention;
                }
            }
        }
    }

    #[test]
    fn g_and_shift_g_jump_to_the_list_edges() {
        let mut state = state_with_three_panes();
        state.global.selected_pane_row = 1;

        handle_key_event(key(KeyCode::Char('g')), &mut state);
        assert_eq!(state.global.selected_pane_row, 0);

        handle_key_event(key(KeyCode::Char('G')), &mut state);
        assert_eq!(state.global.selected_pane_row, 2);
    }

    #[test]
    fn brackets_jump_between_group_starts() {
        let mut state = state_with_search_groups();
        assert_eq!(
            state
                .layout
                .group_starts
                .iter()
                .map(|g| (g.name.as_str(), g.row_start))
                .collect::<Vec<_>>(),
            vec![("backend", 0), ("frontend", 2)]
        );

        handle_key_event(key(KeyCode::Char(']')), &mut state);
        assert_eq!(state.global.selected_pane_row, 2);
        // Already on the last group: no wrap.
        handle_key_event(key(KeyCode::Char(']')), &mut state);
        assert_eq!(state.global.selected_pane_row, 2);

        handle_key_event(key(KeyCode::Char('[')), &mut state);
        assert_eq!(state.global.selected_pane_row, 0);
        handle_key_event(key(KeyCode::Char('[')), &mut state);
        assert_eq!(state.global.selected_pane_row, 0);
    }

    #[test]
    fn a_cycles_to_the_next_attention_row() {
        let mut state = state_with_search_groups();
        set_pane_attention(&mut state, "%1", PaneAttention::Done);
        set_pane_attention(&mut state, "%3", PaneAttention::Notification);

        // Starting on %1 (attention), the next flagged row is %3.
        handle_key_event(key(KeyCode::Char('a')), &mut state);
        assert_eq!(state.global.selected_pane_row, 2);

        // Wrap back to %1.
        handle_key_event(key(KeyCode::Char('a')), &mut state);
        assert_eq!(state.global.selected_pane_row, 0);
    }

    #[test]
    fn a_includes_window_rows_with_attention() {
        let mut state = AppState::new("%99".into());
        let mut agent = search_pane("%1", AgentType::Claude, "work", "claude-api");
        agent.path = "/tmp/backend".into();
        state.repo_groups = vec![RepoGroup {
            name: "backend".into(),
            has_focus: false,
            panes: vec![(agent, PaneGitInfo::default())],
        }];
        state.global.show_windows = true;
        state.other_windows.insert(
            "/tmp/backend".into(),
            vec![crate::group::OtherWindow {
                window_id: "@5".into(),
                window_index: 0,
                window_name: "tests".into(),
                window_active: false,
                session_name: "work".into(),
                session_attached: true,
                attention: PaneAttention::Notification,
                pane_id: "%9".into(),
                pane_active: true,
                pane_pid: None,
                command: "cargo".into(),
                command_args: None,
                path: "/tmp/backend".into(),
                git_info: PaneGitInfo::default(),
                status: crate::tmux::classify_window_status("cargo"),
                pane_title: String::new(),
                flagged: false,
            }],
        );
        state.rebuild_row_targets();
        assert_eq!(target_ids(&state), vec!["%1", "%9"]);

        handle_key_event(key(KeyCode::Char('a')), &mut state);
        assert_eq!(state.global.selected_pane_row, 1);
    }

    #[test]
    fn a_is_a_no_op_when_nothing_needs_attention() {
        let mut state = state_with_search_groups();
        handle_key_event(key(KeyCode::Char('a')), &mut state);
        assert_eq!(state.global.selected_pane_row, 0);
    }

    #[test]
    fn z_collapses_the_selected_group_and_shift_z_expands_all() {
        let mut state = state_with_search_groups();
        assert_eq!(target_ids(&state), vec!["%1", "%2", "%3"]);

        handle_key_event(key(KeyCode::Char('z')), &mut state);
        assert_eq!(target_ids(&state), vec!["%3"]);
        assert!(state.is_group_collapsed("backend"));
        assert_eq!(
            state
                .layout
                .group_starts
                .iter()
                .map(|g| (g.name.as_str(), g.row_start))
                .collect::<Vec<_>>(),
            vec![("frontend", 0)]
        );

        handle_key_event(key(KeyCode::Char('Z')), &mut state);
        assert_eq!(target_ids(&state), vec!["%1", "%2", "%3"]);
        assert!(!state.is_group_collapsed("backend"));
    }

    #[test]
    fn collapsing_the_last_group_clamps_selection_to_a_visible_row() {
        let mut state = state_with_search_groups();
        state.global.selected_pane_row = 2; // %3, the only frontend row

        handle_key_event(key(KeyCode::Char('z')), &mut state);

        assert_eq!(target_ids(&state), vec!["%1", "%2"]);
        assert_eq!(state.global.selected_pane_row, 1);
        assert_eq!(state.selected_pane().unwrap().pane_id, "%2");
    }

    #[test]
    fn s_toggles_grouping_mode() {
        let mut state = state_with_search_groups();
        assert_eq!(state.global.group_by, crate::state::GroupBy::Repo);

        handle_key_event(key(KeyCode::Char('s')), &mut state);
        assert_eq!(state.global.group_by, crate::state::GroupBy::Session);

        handle_key_event(key(KeyCode::Char('s')), &mut state);
        assert_eq!(state.global.group_by, crate::state::GroupBy::Repo);
    }

    #[test]
    fn q_requests_quit_from_panes_focus() {
        // `q` must tear the TUI down from any non-modal focus: the event
        // loop breaks on `quit_requested`, which closes a popup (via
        // `display-popup -E`) or ends the pane-mode sidebar.
        let mut state = state_with_three_panes();
        handle_key_event(key(KeyCode::Char('q')), &mut state);
        assert!(state.quit_requested);
    }

    #[test]
    fn q_types_into_spawn_input_without_requesting_quit() {
        // The spawn modal owns plain characters — typing `q` into the
        // worktree task field must not tear down the whole sidebar.
        let mut state = state_with_three_panes();
        state.popup = PopupState::SpawnInput {
            input: String::new(),
            target_repo: "repo-a".into(),
            target_repo_root: "/tmp/repo-a".into(),
            agent_idx: 0,
            mode_idx: 0,
            field: SpawnField::Task,
            anchor_y: None,
            error: None,
            working: None,
            area: None,
        };
        handle_key_event(key(KeyCode::Char('q')), &mut state);
        assert!(!state.quit_requested);
        assert!(state.is_spawn_input_open());
        if let PopupState::SpawnInput { input, .. } = &state.popup {
            assert_eq!(input, "q");
        }
    }

    #[test]
    fn ctrl_n_navigates_repo_popup_down() {
        let mut state = state_with_repo_popup_open();
        handle_key_event(ctrl_key('n'), &mut state);
        assert_eq!(state.repo_popup_selected(), 1);
        handle_key_event(ctrl_key('n'), &mut state);
        assert_eq!(state.repo_popup_selected(), 2);
        // Past the last entry the popup nav helper is a no-op.
        handle_key_event(ctrl_key('n'), &mut state);
        assert_eq!(state.repo_popup_selected(), 2);
    }

    #[test]
    fn ctrl_p_navigates_repo_popup_up() {
        let mut state = state_with_repo_popup_open();
        state.set_repo_popup_selected(2);
        handle_key_event(ctrl_key('p'), &mut state);
        assert_eq!(state.repo_popup_selected(), 1);
        handle_key_event(ctrl_key('p'), &mut state);
        assert_eq!(state.repo_popup_selected(), 0);
        // Below 0 the popup nav helper is a no-op.
        handle_key_event(ctrl_key('p'), &mut state);
        assert_eq!(state.repo_popup_selected(), 0);
    }

    #[test]
    fn c_toggles_compact_without_flash() {
        // `c` is global (any non-modal focus) so the toggle works from the
        // filter bar too, not just Panes focus.
        let mut state = state_with_three_panes();
        assert!(!state.global.compact);

        handle_key_event(key(KeyCode::Char('c')), &mut state);
        assert!(state.global.compact);
        assert!(state.flash.is_none(), "toggles set no feedback flash");

        handle_key_event(key(KeyCode::Char('c')), &mut state);
        assert!(!state.global.compact);
        assert!(state.flash.is_none(), "toggles set no feedback flash");
    }

    #[test]
    fn w_toggles_show_windows_without_flash() {
        // `w` is global like `c`.
        let mut state = state_with_three_panes();
        assert!(!state.global.show_windows);

        handle_key_event(key(KeyCode::Char('w')), &mut state);
        assert!(state.global.show_windows);
        assert!(state.flash.is_none(), "toggles set no feedback flash");

        handle_key_event(key(KeyCode::Char('w')), &mut state);
        assert!(!state.global.show_windows);
        assert!(state.flash.is_none(), "toggles set no feedback flash");
    }

    #[test]
    fn question_mark_toggles_help_popup() {
        let mut state = state_with_three_panes();
        assert!(!state.is_help_open());

        handle_key_event(key(KeyCode::Char('?')), &mut state);
        assert!(state.is_help_open());
        assert!(!state.quit_requested, "? must not be read as anything else");

        handle_key_event(key(KeyCode::Char('?')), &mut state);
        assert!(!state.is_help_open());
    }

    #[test]
    fn help_popup_owns_keyboard_until_dismissed() {
        // While open the popup swallows every key; only Esc (or another
        // `?`) closes it. `j` scrolls the overlay, not the list beneath.
        let mut state = state_with_three_panes();
        state.toggle_help_popup();

        handle_key_event(key(KeyCode::Char('j')), &mut state);
        assert!(state.is_help_open());
        assert_eq!(state.global.selected_pane_row, 0);
        assert_eq!(state.help_scroll(), 0, "nothing to scroll before a render");

        handle_key_event(key(KeyCode::Char('q')), &mut state);
        assert!(!state.quit_requested, "q must not quit through the popup");

        handle_key_event(key(KeyCode::Esc), &mut state);
        assert!(!state.is_help_open());
    }

    #[test]
    fn help_scroll_keys_reach_the_overlay_state() {
        let mut state = state_with_three_panes();
        state.toggle_help_popup();
        // Simulate the renderer's write-back for a clipped overlay.
        state.set_help_metrics(30, 10);

        handle_key_event(key(KeyCode::Char('j')), &mut state);
        handle_key_event(key(KeyCode::Down), &mut state);
        assert_eq!(state.help_scroll(), 2);

        handle_key_event(key(KeyCode::Char('k')), &mut state);
        assert_eq!(state.help_scroll(), 1);

        handle_key_event(key(KeyCode::PageDown), &mut state);
        assert_eq!(state.help_scroll(), 9, "pages by viewport - 2");

        handle_key_event(key(KeyCode::PageUp), &mut state);
        assert_eq!(state.help_scroll(), 1, "one page back down from the cap");

        handle_key_event(key(KeyCode::Up), &mut state);
        assert_eq!(state.help_scroll(), 0, "clamped at the top");
    }

    #[test]
    fn w_types_into_spawn_input_without_toggling() {
        let mut state = state_with_three_panes();
        state.popup = PopupState::SpawnInput {
            input: String::new(),
            target_repo: "repo-a".into(),
            target_repo_root: "/tmp/repo-a".into(),
            agent_idx: 0,
            mode_idx: 0,
            field: SpawnField::Task,
            anchor_y: None,
            error: None,
            working: None,
            area: None,
        };
        handle_key_event(key(KeyCode::Char('w')), &mut state);
        assert!(!state.global.show_windows, "popup must swallow the key");
        if let PopupState::SpawnInput { input, .. } = &state.popup {
            assert_eq!(input, "w");
        }
    }

    #[test]
    fn c_inside_remove_confirm_does_not_toggle_compact() {
        // The remove-confirm popup owns `c` ("close window only"); the
        // popup arm runs first, so compact mode must be untouched.
        let mut state = state_with_three_panes();
        state.popup = PopupState::RemoveConfirm {
            pane_id: "%1".into(),
            branch: "agent/x".into(),
            warning: Vec::new(),
            error: None,
            working: None,
            area: None,
        };
        handle_key_event(key(KeyCode::Char('c')), &mut state);
        assert!(!state.global.compact, "popup must swallow the key");
        assert!(state.flash.is_none(), "no toggle feedback expected");
    }

    // ─── t (TODO panel) ──────────────────────────────────────────────

    #[test]
    fn t_opens_the_todo_panel_and_moves_focus() {
        let mut state = state_with_three_panes();
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        assert!(state.todos.visible);
        assert_eq!(state.focus_state.focus, Focus::Todos);

        handle_key_event(key(KeyCode::Char('t')), &mut state);
        assert!(!state.todos.visible);
        assert_eq!(state.focus_state.focus, Focus::Panes);
    }

    #[test]
    fn t_from_panes_focus_focuses_a_visible_todo_panel() {
        let mut state = state_with_three_panes();
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        assert!(state.todos.visible);

        // Esc hands focus back to the list but leaves the panel open.
        handle_key_event(key(KeyCode::Esc), &mut state);
        assert_eq!(state.focus_state.focus, Focus::Panes);

        // `t` now focuses the panel instead of closing it.
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        assert!(state.todos.visible);
        assert_eq!(state.focus_state.focus, Focus::Todos);

        // A second `t` while focused closes it.
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        assert!(!state.todos.visible);
        assert_eq!(state.focus_state.focus, Focus::Panes);
    }

    fn ctrl_code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn todo_fixture() -> AppState {
        let mut state = state_with_three_panes();
        state.todos.items = vec![
            crate::todo::TodoItem {
                text: "a".into(),
                done: false,
            },
            crate::todo::TodoItem {
                text: "b".into(),
                done: false,
            },
        ];
        state
    }

    #[test]
    fn todo_panel_owns_j_k_and_selection_stays_off_the_agent_list() {
        let mut state = todo_fixture();
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        assert_eq!(state.todos.selected, 0);

        handle_key_event(key(KeyCode::Char('j')), &mut state);
        assert_eq!(state.todos.selected, 1);
        assert_eq!(
            state.global.selected_pane_row, 0,
            "j must not move the agent selection while the panel owns focus"
        );

        handle_key_event(key(KeyCode::Char('k')), &mut state);
        assert_eq!(state.todos.selected, 0);
    }

    #[test]
    fn enter_edits_the_selected_todo() {
        let mut state = todo_fixture();
        handle_key_event(key(KeyCode::Char('t')), &mut state);

        handle_key_event(key(KeyCode::Enter), &mut state);

        assert_eq!(
            state.todos.editing.as_ref().unwrap().index,
            0,
            "Enter opens the selected task for editing, Google Tasks style"
        );
        assert!(!state.todos.items[0].done);
    }

    #[test]
    fn enter_on_an_empty_list_starts_a_new_task() {
        let mut state = state_with_three_panes();
        handle_key_event(key(KeyCode::Char('t')), &mut state);

        handle_key_event(key(KeyCode::Enter), &mut state);

        assert_eq!(
            state.todos.adding.as_deref(),
            Some(""),
            "nothing to edit, so Enter creates a task instead"
        );
    }

    #[test]
    fn enter_while_editing_saves_and_starts_the_next_task() {
        let mut state = todo_fixture();
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Enter), &mut state); // edit item 0
        handle_key_event(key(KeyCode::Backspace), &mut state);
        handle_key_event(key(KeyCode::Char('z')), &mut state); // types into the buffer

        handle_key_event(key(KeyCode::Enter), &mut state);

        assert_eq!(state.todos.items[0].text, "z", "the edit saved");
        assert!(state.todos.editing.is_none());
        assert_eq!(
            state.todos.adding.as_deref(),
            Some(""),
            "the newline lands on a fresh task"
        );
    }

    #[test]
    fn escape_while_editing_saves_instead_of_discarding() {
        let mut state = todo_fixture();
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Enter), &mut state); // edit item 0
        handle_key_event(key(KeyCode::Backspace), &mut state);
        handle_key_event(key(KeyCode::Char('z')), &mut state); // types into the buffer

        handle_key_event(key(KeyCode::Esc), &mut state);

        assert_eq!(
            state.todos.items[0].text, "z",
            "Esc is vim's insert-mode exit: the edit stays"
        );
        assert!(state.todos.editing.is_none());
        assert!(state.todos.adding.is_none(), "no fresh line opens");
    }

    #[test]
    fn escape_on_the_add_line_saves_the_typed_text() {
        let mut state = todo_fixture();
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Char('a')), &mut state);
        handle_key_event(key(KeyCode::Char('h')), &mut state);
        handle_key_event(key(KeyCode::Char('i')), &mut state);

        handle_key_event(key(KeyCode::Esc), &mut state);

        assert!(state.todos.adding.is_none(), "insert mode closes");
        assert_eq!(
            state
                .todos
                .items
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "hi"],
            "the buffered text stays"
        );
    }

    #[test]
    fn backspace_on_the_fresh_streak_line_edits_the_previous_task() {
        let mut state = todo_fixture();
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Char('a')), &mut state);
        handle_key_event(key(KeyCode::Char('h')), &mut state);
        handle_key_event(key(KeyCode::Char('i')), &mut state);
        handle_key_event(key(KeyCode::Enter), &mut state); // commits "hi", fresh line
        assert_eq!(state.todos.adding.as_deref(), Some(""));

        handle_key_event(key(KeyCode::Backspace), &mut state);

        let edit = state.todos.editing.as_ref().unwrap();
        assert_eq!(edit.index, 2, "the previous task reopened");
        assert_eq!(edit.input, "hi");
        assert_eq!(edit.cursor, 2, "the caret sits at the end of the text");
        assert!(
            state.todos.adding.is_none(),
            "insert mode continues on the previous task"
        );
    }

    #[test]
    fn backspacing_through_the_whole_draft_lands_on_the_previous_task() {
        let mut state = todo_fixture();
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Char('a')), &mut state);
        for ch in 'a'..'e' {
            handle_key_event(key(KeyCode::Char(ch)), &mut state);
        }
        // Four pops empty the draft; the next backspace returns the caret.
        for _ in 0..5 {
            handle_key_event(key(KeyCode::Backspace), &mut state);
        }

        assert!(state.todos.adding.is_none(), "the draft line closed");
        let edit = state.todos.editing.as_ref().unwrap();
        assert_eq!(edit.index, 1, "the caret returned to the previous task");
        assert_eq!(edit.input, "b");
        assert_eq!(edit.cursor, 1);
    }

    #[test]
    fn home_and_end_jump_the_edit_cursor_without_ctrl() {
        // Regression: the Home/End arms shared a `if ctrl` guard with
        // their Ctrl-a/e aliases, and the guard applies to the whole
        // or-pattern — plain Home/End matched nothing.
        let mut state = todo_fixture();
        state.todos.items[0].text = "alpha beta".into();
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Enter), &mut state);
        let cursor = |s: &AppState| s.todos.editing.as_ref().unwrap().cursor;

        handle_key_event(key(KeyCode::Home), &mut state);
        assert_eq!(cursor(&state), 0, "plain Home jumps to the start");

        handle_key_event(key(KeyCode::End), &mut state);
        assert_eq!(cursor(&state), 10, "plain End jumps to the end");
    }

    #[test]
    fn ctrl_arrows_move_the_edit_cursor_by_words() {
        let mut state = todo_fixture();
        state.todos.items[0].text = "alpha beta gamma".into();
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Enter), &mut state);
        let cursor = |s: &AppState| s.todos.editing.as_ref().unwrap().cursor;

        handle_key_event(ctrl_code(KeyCode::Left), &mut state);
        assert_eq!(cursor(&state), 11, "Ctrl-Left lands on 'gamma'");
        handle_key_event(ctrl_code(KeyCode::Left), &mut state);
        assert_eq!(cursor(&state), 6, "Ctrl-Left lands on 'beta'");
        handle_key_event(ctrl_code(KeyCode::Right), &mut state);
        assert_eq!(cursor(&state), 10, "Ctrl-Right stops after 'beta'");
        handle_key_event(ctrl_code(KeyCode::Right), &mut state);
        assert_eq!(cursor(&state), 16, "Ctrl-Right stops after 'gamma'");
        // Plain arrows still move one char.
        handle_key_event(key(KeyCode::Right), &mut state);
        assert_eq!(cursor(&state), 16);
    }

    #[test]
    fn space_toggles_the_selected_todo() {
        let mut state = todo_fixture();
        state.global.todo_hide_completed = false;
        handle_key_event(key(KeyCode::Char('t')), &mut state);

        handle_key_event(key(KeyCode::Char(' ')), &mut state);
        assert!(state.todos.items[0].done);

        handle_key_event(key(KeyCode::Char(' ')), &mut state);
        assert!(!state.todos.items[0].done);
    }

    #[test]
    fn ctrl_z_restores_a_deleted_todo_like_u() {
        let mut state = todo_fixture();
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Char('d')), &mut state);
        assert_eq!(state.todos.items.len(), 1);

        handle_key_event(ctrl_key('z'), &mut state);

        assert_eq!(state.todos.items.len(), 2, "Ctrl-z restores the delete");
    }

    #[test]
    fn x_deletes_the_selected_todo() {
        let mut state = state_with_three_panes();
        state.todos.items = vec![
            crate::todo::TodoItem {
                text: "a".into(),
                done: false,
            },
            crate::todo::TodoItem {
                text: "b".into(),
                done: false,
            },
        ];
        handle_key_event(key(KeyCode::Char('t')), &mut state);

        handle_key_event(key(KeyCode::Char('x')), &mut state);
        assert_eq!(state.todos.items.len(), 1);
        assert_eq!(state.todos.items[0].text, "b");
        assert_eq!(state.todos.selected, 0);
    }

    #[test]
    fn shift_d_clears_completed_todos() {
        let mut state = state_with_three_panes();
        state.todos.items = vec![
            crate::todo::TodoItem {
                text: "open".into(),
                done: false,
            },
            crate::todo::TodoItem {
                text: "done".into(),
                done: true,
            },
        ];
        handle_key_event(key(KeyCode::Char('t')), &mut state);

        handle_key_event(key(KeyCode::Char('D')), &mut state);
        assert_eq!(state.todos.items.len(), 1);
        assert_eq!(state.todos.items[0].text, "open");
    }

    #[test]
    fn e_edits_the_selected_todo() {
        let mut state = state_with_three_panes();
        state.todos.items = vec![crate::todo::TodoItem {
            text: "old".into(),
            done: false,
        }];
        handle_key_event(key(KeyCode::Char('t')), &mut state);

        handle_key_event(key(KeyCode::Char('e')), &mut state);
        assert_eq!(state.todos.editing.as_ref().unwrap().input, "old");

        handle_key_event(ctrl_key('u'), &mut state);
        for c in "new".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state);
        }
        handle_key_event(key(KeyCode::Enter), &mut state);

        assert_eq!(state.todos.items[0].text, "new");
        assert!(state.todos.editing.is_none());
        assert_eq!(state.focus_state.focus, Focus::Todos);
    }

    #[test]
    fn q_types_into_the_todo_add_line_without_quitting() {
        let mut state = state_with_three_panes();
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Char('a')), &mut state);
        handle_key_event(key(KeyCode::Char('q')), &mut state);

        assert!(!state.quit_requested, "q must edit the add line, not quit");
        assert_eq!(state.todos.adding.as_deref(), Some("q"));

        handle_key_event(key(KeyCode::Enter), &mut state);
        assert_eq!(state.todos.items.len(), 1);
        assert_eq!(state.todos.items[0].text, "q");
    }

    #[test]
    fn escape_in_todo_panel_returns_focus_but_keeps_it_visible() {
        let mut state = state_with_three_panes();
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Esc), &mut state);

        assert!(state.todos.visible);
        assert_eq!(state.focus_state.focus, Focus::Panes);

        // The panel no longer owns navigation.
        handle_key_event(key(KeyCode::Char('j')), &mut state);
        assert_eq!(state.global.selected_pane_row, 1);
    }

    #[test]
    fn ctrl_arrow_keys_reorder_like_ctrl_j_and_ctrl_k() {
        let mut state = todo_fixture();
        handle_key_event(key(KeyCode::Char('t')), &mut state);

        handle_key_event(ctrl_code(KeyCode::Down), &mut state);
        assert_eq!(
            state
                .todos
                .items
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"],
            "Ctrl-Down moves the task down"
        );

        handle_key_event(ctrl_code(KeyCode::Up), &mut state);
        assert_eq!(
            state
                .todos
                .items
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"],
            "Ctrl-Up moves it back"
        );
    }

    #[test]
    fn ctrl_j_and_ctrl_k_reorder_the_selected_todo() {
        let mut state = state_with_three_panes();
        state.todos.items = todo_items(&["a", "b", "c"]);
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        assert_eq!(state.todos.selected, 0);

        handle_key_event(ctrl_key('j'), &mut state);
        assert_eq!(todo_texts(&state), vec!["b", "a", "c"]);
        assert_eq!(state.todos.selected, 1, "selection follows the moved item");
        assert_eq!(
            state.global.selected_pane_row, 0,
            "the agent selection stays put"
        );

        handle_key_event(ctrl_key('k'), &mut state);
        assert_eq!(todo_texts(&state), vec!["a", "b", "c"]);
        assert_eq!(state.todos.selected, 0);
    }

    #[test]
    fn ctrl_k_at_the_top_of_the_open_block_is_a_no_op() {
        let mut state = state_with_three_panes();
        state.todos.items = todo_items(&["a", "b"]);
        handle_key_event(key(KeyCode::Char('t')), &mut state);

        handle_key_event(ctrl_key('k'), &mut state);

        assert_eq!(todo_texts(&state), vec!["a", "b"]);
        assert_eq!(state.todos.selected, 0);
        assert_eq!(
            state.focus_state.focus,
            Focus::Todos,
            "reorder must not walk out of the panel"
        );
    }

    #[test]
    fn ctrl_j_never_reorders_the_completed_block() {
        let mut state = state_with_three_panes();
        state.todos.items = vec![
            crate::todo::TodoItem {
                text: "a".into(),
                done: false,
            },
            crate::todo::TodoItem {
                text: "b".into(),
                done: true,
            },
            crate::todo::TodoItem {
                text: "c".into(),
                done: false,
            },
            crate::todo::TodoItem {
                text: "d".into(),
                done: true,
            },
        ];
        state.global.todo_hide_completed = false;
        handle_key_event(key(KeyCode::Char('t')), &mut state);

        handle_key_event(ctrl_key('j'), &mut state);
        assert_eq!(todo_texts(&state), vec!["c", "b", "a", "d"]);
        assert_eq!(
            state.todos.selected, 2,
            "a moved to c's slot; the done block stayed put"
        );
        assert!(state.todos.items[1].done && state.todos.items[3].done);

        // a is now the last open item: the boundary no-ops instead of
        // pushing it into the done block.
        handle_key_event(ctrl_key('j'), &mut state);
        assert_eq!(todo_texts(&state), vec!["c", "b", "a", "d"]);
        assert_eq!(state.todos.selected, 2);
    }

    #[test]
    fn ctrl_j_with_an_empty_todo_list_is_a_no_op() {
        let mut state = state_with_three_panes();
        handle_key_event(key(KeyCode::Char('t')), &mut state);

        handle_key_event(ctrl_key('j'), &mut state);

        assert!(state.todos.items.is_empty());
        assert_eq!(state.focus_state.focus, Focus::Todos);
    }

    #[test]
    fn ctrl_j_while_the_add_line_is_open_does_not_reorder() {
        let mut state = state_with_three_panes();
        state.todos.items = todo_items(&["a", "b"]);
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Char('a')), &mut state);

        handle_key_event(ctrl_key('j'), &mut state);

        assert_eq!(todo_texts(&state), vec!["a", "b"], "items untouched");
        assert_eq!(
            state.todos.adding.as_deref(),
            Some(""),
            "the add line stays open and owns the keyboard"
        );
        assert_eq!(state.focus_state.focus, Focus::Todos);
    }

    // ─── u (undo delete / clear-completed) ───────────────────────────

    #[test]
    fn u_restores_a_deleted_todo_within_the_window() {
        let mut state = state_with_three_panes();
        state.todos.items = todo_items(&["a", "b", "c"]);
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Char('j')), &mut state); // select b
        handle_key_event(key(KeyCode::Char('x')), &mut state);

        assert_eq!(todo_texts(&state), vec!["a", "c"]);
        assert!(state.todos.undo.is_some(), "the delete stashes the item");
        assert!(
            state.flash.is_none(),
            "no banner — the list is the feedback"
        );

        handle_key_event(key(KeyCode::Char('u')), &mut state);

        assert_eq!(
            todo_texts(&state),
            vec!["a", "b", "c"],
            "the stash is restored in place"
        );
        assert!(state.todos.undo.is_none());
    }

    #[test]
    fn u_restores_cleared_completed_todos() {
        let mut state = state_with_three_panes();
        state.todos.items = vec![
            crate::todo::TodoItem {
                text: "a".into(),
                done: true,
            },
            crate::todo::TodoItem {
                text: "b".into(),
                done: false,
            },
        ];
        handle_key_event(key(KeyCode::Char('t')), &mut state);

        handle_key_event(key(KeyCode::Char('D')), &mut state);
        assert_eq!(todo_texts(&state), vec!["b"]);

        handle_key_event(key(KeyCode::Char('u')), &mut state);
        assert_eq!(todo_texts(&state), vec!["a", "b"], "file order restored");
        assert!(state.todos.items[0].done, "the done flag comes back too");
        assert!(state.todos.undo.is_none());
    }

    #[test]
    fn u_with_no_pending_undo_is_unbound() {
        let mut state = state_with_three_panes();
        handle_key_event(key(KeyCode::Char('u')), &mut state);
        assert_eq!(
            state.global.selected_pane_row, 0,
            "outside the window u changes nothing in the list"
        );

        // Same inside the focused panel.
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Char('u')), &mut state);
        assert!(state.todos.undo.is_none());
        assert_eq!(state.focus_state.focus, Focus::Todos, "u claims no focus");
    }

    #[test]
    fn u_while_the_add_line_is_open_types_into_the_buffer() {
        let mut state = state_with_three_panes();
        state.todos.items = todo_items(&["a", "b"]);
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Char('x')), &mut state); // stash pending
        assert!(state.todos.undo.is_some());

        handle_key_event(key(KeyCode::Char('a')), &mut state); // open add line
        handle_key_event(key(KeyCode::Char('u')), &mut state);

        assert_eq!(
            state.todos.adding.as_deref(),
            Some("u"),
            "the add line owns the keyboard"
        );
        assert!(state.todos.undo.is_some(), "undo did not fire");
        assert_eq!(todo_texts(&state), vec!["b"], "the item stays deleted");
    }

    #[test]
    fn u_while_editing_types_into_the_edit_buffer() {
        let mut state = state_with_three_panes();
        state.todos.items = todo_items(&["a", "b"]);
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Char('x')), &mut state); // stash pending
        handle_key_event(key(KeyCode::Char('e')), &mut state); // edit b

        handle_key_event(key(KeyCode::Char('u')), &mut state);

        assert_eq!(
            state.todos.editing.as_ref().unwrap().input,
            "bu",
            "the edit buffer owns the keyboard"
        );
        assert!(state.todos.undo.is_some(), "undo did not fire");
    }

    #[test]
    fn u_from_panes_focus_undoes_a_pending_delete() {
        let mut state = state_with_three_panes();
        state.todos.items = todo_items(&["a", "b"]);
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Char('x')), &mut state);
        assert_eq!(todo_texts(&state), vec!["b"]);

        handle_key_event(key(KeyCode::Esc), &mut state); // back to the list
        assert_eq!(state.focus_state.focus, Focus::Panes);

        handle_key_event(key(KeyCode::Char('u')), &mut state);
        assert_eq!(
            todo_texts(&state),
            vec!["a", "b"],
            "the banner's u works from the agent list too"
        );
        assert!(state.todos.undo.is_none());
    }

    #[test]
    fn ctrl_u_does_not_fire_undo() {
        let mut state = state_with_three_panes();
        state.todos.items = todo_items(&["a", "b"]);
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Char('x')), &mut state);
        assert!(state.todos.undo.is_some());

        handle_key_event(ctrl_key('u'), &mut state);

        assert_eq!(
            todo_texts(&state),
            vec!["b"],
            "only the plain u is the undo key"
        );
        assert!(state.todos.undo.is_some());
    }

    #[test]
    fn u_types_into_the_search_query_not_undo() {
        let mut state = state_with_three_panes();
        state.todos.items = todo_items(&["a", "b"]);
        handle_key_event(key(KeyCode::Char('t')), &mut state);
        handle_key_event(key(KeyCode::Char('x')), &mut state);
        assert!(state.todos.undo.is_some());

        handle_key_event(key(KeyCode::Esc), &mut state);
        handle_key_event(key(KeyCode::Char('/')), &mut state);
        handle_key_event(key(KeyCode::Char('u')), &mut state);

        assert_eq!(state.search.query, "u", "the search line owns u");
        assert!(state.todos.undo.is_some(), "undo did not fire");
    }

    // ─── f (flag) ────────────────────────────────────────────────────

    fn test_pane(id: &str) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status: PaneStatus::Idle,
            attention: PaneAttention::None,
            agent: AgentType::Claude,
            path: "/tmp".into(),
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

    fn state_with_one_pane() -> AppState {
        let mut state = state_with_three_panes();
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            has_focus: false,
            panes: vec![(test_pane("%1"), PaneGitInfo::default())],
        }];
        state
    }

    #[test]
    fn f_toggles_flag_on_the_selected_agent_pane() {
        let _guard = test_mock::install();
        let mut state = state_with_one_pane();

        handle_key_event(key(KeyCode::Char('f')), &mut state);
        assert!(state.pane_by_id("%1").unwrap().flagged);
        assert_eq!(test_mock::get("%1", PANE_FLAGGED).as_deref(), Some("1"));

        handle_key_event(key(KeyCode::Char('f')), &mut state);
        assert!(!state.pane_by_id("%1").unwrap().flagged);
        assert_eq!(test_mock::get("%1", PANE_FLAGGED).as_deref(), Some("0"));
    }

    #[test]
    fn f_does_not_fire_under_filter_focus() {
        let _guard = test_mock::install();
        let mut state = state_with_one_pane();
        state.focus_state.focus = Focus::Filter;

        handle_key_event(key(KeyCode::Char('f')), &mut state);
        assert!(!state.pane_by_id("%1").unwrap().flagged);
        assert_eq!(test_mock::get("%1", PANE_FLAGGED).as_deref(), Some(""));
    }

    #[test]
    fn f_without_a_row_selection_is_a_no_op() {
        let _guard = test_mock::install();
        let mut state = AppState::new("%99".into());
        state.focus_state.focus = Focus::Panes;
        // No rendered rows: the cursor points at nothing. Must not panic
        // and must not write an option.
        handle_key_event(key(KeyCode::Char('f')), &mut state);
        assert_eq!(test_mock::get("%1", PANE_FLAGGED).as_deref(), Some(""));
    }

    // ─── / (search) ─────────────────────────────────────────────────

    fn search_pane(id: &str, agent: AgentType, tmux_session: &str, session_name: &str) -> PaneInfo {
        PaneInfo {
            agent,
            tmux_session: tmux_session.into(),
            session_name: session_name.into(),
            ..test_pane(id)
        }
    }

    fn state_with_search_groups() -> AppState {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![
            RepoGroup {
                name: "backend".into(),
                has_focus: false,
                panes: vec![
                    (
                        search_pane("%1", AgentType::Claude, "work", "claude-api"),
                        PaneGitInfo::default(),
                    ),
                    (
                        search_pane("%2", AgentType::Codex, "work", "codex"),
                        PaneGitInfo::default(),
                    ),
                ],
            },
            RepoGroup {
                name: "frontend".into(),
                has_focus: false,
                panes: vec![(
                    search_pane("%3", AgentType::Claude, "web", "ui"),
                    PaneGitInfo::default(),
                )],
            },
        ];
        state.rebuild_row_targets();
        state
    }

    fn target_ids(state: &AppState) -> Vec<String> {
        state
            .layout
            .pane_row_targets
            .iter()
            .map(|t| t.pane_id.clone())
            .collect()
    }

    #[test]
    fn slash_opens_search_and_typing_filters_case_insensitively() {
        let mut state = state_with_search_groups();
        handle_key_event(key(KeyCode::Char('/')), &mut state);
        assert!(state.search.is_active());

        for c in "CODEX".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state);
        }
        assert_eq!(state.search.query, "CODEX");
        assert_eq!(target_ids(&state), vec!["%2".to_string()]);
    }

    #[test]
    fn q_types_into_search_without_requesting_quit() {
        let mut state = state_with_search_groups();
        handle_key_event(key(KeyCode::Char('/')), &mut state);
        handle_key_event(key(KeyCode::Char('q')), &mut state);
        assert!(!state.quit_requested, "q must edit the query, not quit");
        assert_eq!(state.search.query, "q");
    }

    #[test]
    fn enter_keeps_query_and_esc_clears_it() {
        let mut state = state_with_search_groups();
        handle_key_event(key(KeyCode::Char('/')), &mut state);
        for c in "codex".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state);
        }
        assert_eq!(target_ids(&state), vec!["%2".to_string()]);

        handle_key_event(key(KeyCode::Enter), &mut state);
        assert!(!state.search.is_active());
        assert_eq!(state.search.query, "codex");
        assert_eq!(target_ids(&state), vec!["%2".to_string()]);

        handle_key_event(key(KeyCode::Esc), &mut state);
        assert!(!state.search.is_active());
        assert_eq!(state.search.query, "");
        assert_eq!(target_ids(&state), vec!["%1", "%2", "%3"]);
    }

    #[test]
    fn ctrl_u_clears_query_but_keeps_input_open() {
        let mut state = state_with_search_groups();
        handle_key_event(key(KeyCode::Char('/')), &mut state);
        for c in "codex".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state);
        }
        handle_key_event(ctrl_key('u'), &mut state);
        assert!(state.search.is_active());
        assert_eq!(state.search.query, "");
        assert_eq!(target_ids(&state), vec!["%1", "%2", "%3"]);
    }

    #[test]
    fn backspace_edits_the_query() {
        let mut state = state_with_search_groups();
        handle_key_event(key(KeyCode::Char('/')), &mut state);
        for c in "codexx".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state);
        }
        assert!(target_ids(&state).is_empty());
        handle_key_event(key(KeyCode::Backspace), &mut state);
        assert_eq!(state.search.query, "codex");
        assert_eq!(target_ids(&state), vec!["%2".to_string()]);
    }

    #[test]
    fn ctrl_w_deletes_the_word_before_the_cursor() {
        let mut state = state_with_search_groups();
        handle_key_event(key(KeyCode::Char('/')), &mut state);
        for c in "backend codex".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state);
        }
        assert_eq!(state.search.query, "backend codex");

        handle_key_event(ctrl_key('w'), &mut state);
        assert_eq!(state.search.query, "backend ");
        assert_eq!(target_ids(&state), vec!["%1", "%2"]);

        handle_key_event(ctrl_key('w'), &mut state);
        assert_eq!(state.search.query, "");
        assert_eq!(target_ids(&state), vec!["%1", "%2", "%3"]);
    }

    #[test]
    fn up_down_navigate_results_without_leaving_search() {
        let mut state = state_with_search_groups();
        handle_key_event(key(KeyCode::Char('/')), &mut state);
        for c in "work".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state);
        }
        assert_eq!(target_ids(&state), vec!["%1", "%2"]);
        handle_key_event(key(KeyCode::Down), &mut state);
        assert_eq!(state.global.selected_pane_row, 1);
        assert!(state.search.is_active());
        handle_key_event(key(KeyCode::Up), &mut state);
        assert_eq!(state.global.selected_pane_row, 0);
        assert_eq!(state.search.query, "work");

        // The same navigation works through the Ctrl-n / Ctrl-p pair.
        handle_key_event(ctrl_key('n'), &mut state);
        assert_eq!(state.global.selected_pane_row, 1);
        handle_key_event(ctrl_key('p'), &mut state);
        assert_eq!(state.global.selected_pane_row, 0);
        assert!(state.search.is_active());
        assert_eq!(state.search.query, "work");
    }

    #[test]
    fn search_preserves_selection_when_the_pane_still_matches() {
        let mut state = state_with_search_groups();
        state.global.selected_pane_row = 1; // %2, the codex pane
        handle_key_event(key(KeyCode::Char('/')), &mut state);
        for c in "codex".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state);
        }
        assert_eq!(target_ids(&state), vec!["%2".to_string()]);
        assert_eq!(state.global.selected_pane_row, 0);
        assert_eq!(state.selected_pane().unwrap().pane_id, "%2");
    }

    #[test]
    fn search_selects_first_match_when_the_selection_falls_out() {
        let mut state = state_with_search_groups();
        state.global.selected_pane_row = 0; // %1, the claude pane
        handle_key_event(key(KeyCode::Char('/')), &mut state);
        for c in "codex".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state);
        }
        assert_eq!(target_ids(&state), vec!["%2".to_string()]);
        assert_eq!(state.global.selected_pane_row, 0);
        assert_eq!(state.selected_pane().unwrap().pane_id, "%2");
    }

    #[test]
    fn jumping_to_a_pane_clears_the_search() {
        let mut state = state_with_search_groups();
        handle_key_event(key(KeyCode::Char('/')), &mut state);
        for c in "codex".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state);
        }
        assert_eq!(target_ids(&state), vec!["%2".to_string()]);

        // First Enter closes the input but keeps the query applied.
        handle_key_event(key(KeyCode::Enter), &mut state);
        assert!(!state.search.is_active());
        assert_eq!(state.search.query, "codex");

        // Second Enter jumps to the selected pane and ends the search.
        handle_key_event(key(KeyCode::Enter), &mut state);
        assert_eq!(state.search.query, "");
        assert!(!state.search.is_active());
        assert_eq!(target_ids(&state), vec!["%1", "%2", "%3"]);
    }

    #[test]
    fn group_name_match_keeps_all_rows_in_that_group() {
        let mut state = state_with_search_groups();
        handle_key_event(key(KeyCode::Char('/')), &mut state);
        for c in "backend".chars() {
            handle_key_event(key(KeyCode::Char(c)), &mut state);
        }
        assert_eq!(target_ids(&state), vec!["%1", "%2"]);
    }
}
