use std::io;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::state::{AppState, BottomTab, Focus};
use crate::worktree::RemoveMode;

/// Dispatch a single crossterm [`Event`] into the [`AppState`], returning
/// `true` when a redraw should be scheduled.
///
/// The terminal handle is only borrowed to query its size for mouse
/// coordinate conversion; it is never written to from here.
pub(super) fn handle_event(
    ev: Event,
    state: &mut AppState,
    terminal: &Terminal<CrosstermBackend<io::Stdout>>,
) -> bool {
    match ev {
        Event::Key(key) => handle_key_event(key, state),
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let term_height = terminal.size().map(|s| s.height).unwrap_or(0);
                let bottom_h = state.effective_bottom_height();
                let bottom_start = term_height.saturating_sub(bottom_h);
                if mouse.row < bottom_start {
                    state.handle_mouse_click(mouse.row, mouse.column);
                } else if mouse.row == bottom_start {
                    state.handle_bottom_tab_click(mouse.column);
                }
                true
            }
            MouseEventKind::ScrollDown => {
                let term_height = terminal.size().map(|s| s.height).unwrap_or(0);
                let bottom_h = state.effective_bottom_height();
                state.handle_mouse_scroll(mouse.row, term_height, bottom_h, 3);
                true
            }
            MouseEventKind::ScrollUp => {
                let term_height = terminal.size().map(|s| s.height).unwrap_or(0);
                let bottom_h = state.effective_bottom_height();
                state.handle_mouse_scroll(mouse.row, term_height, bottom_h, -3);
                true
            }
            // Motion, drag, and button-release events mutate nothing; only
            // the kinds above change state, and only they may schedule a
            // redraw. Returning `true` for every mouse event used to force
            // a full frame render on each movement burst.
            _ => false,
        },
        // Re-evaluate auto-minimize immediately when the viewport changes;
        // without this a short terminal keeps the full-height panel until
        // the next refresh tick.
        Event::Resize(_, height) => {
            state.apply_auto_minimize(height);
            true
        }
        _ => false,
    }
}

/// Dispatch a single [`KeyEvent`]. Split out from [`handle_event`] so that
/// unit tests can drive the keyboard path without constructing a real
/// terminal handle (the [`Terminal`] argument is only needed for mouse
/// coordinate conversion).
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
    match key.code {
        KeyCode::Esc => {
            if state.focus_state.focus == Focus::BottomPanel
                || state.focus_state.focus == Focus::Filter
            {
                state.focus_state.focus = Focus::Panes;
            }
        }
        KeyCode::Char('q') => state.quit_requested = true,
        KeyCode::Char('j') | KeyCode::Down => pane_nav_down(state),
        KeyCode::Char('n') if ctrl => pane_nav_down(state),
        KeyCode::Char('k') | KeyCode::Up => pane_nav_up(state),
        KeyCode::Char('p') if ctrl => pane_nav_up(state),
        KeyCode::Char('h') | KeyCode::Left => match state.focus_state.focus {
            Focus::Filter => {
                state.global.status_filter = state.global.status_filter.prev();
                state.global.save_filter();
                state.rebuild_row_targets();
            }
            // Two tabs, so both directions cycle the same pair.
            Focus::BottomPanel => state.next_bottom_tab(),
            _ => {}
        },
        KeyCode::Char('l') | KeyCode::Right => match state.focus_state.focus {
            Focus::Filter => {
                state.global.status_filter = state.global.status_filter.next();
                state.global.save_filter();
                state.rebuild_row_targets();
            }
            Focus::BottomPanel => state.next_bottom_tab(),
            // "Go into" the agent: Right (and its vim pair `l`) mirrors
            // Enter's jump-to-pane from the agent list.
            Focus::Panes => state.activate_selected_pane(),
        },
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
        // Toggles are self-evident in the pane list; `?` explains them.
        KeyCode::Char('c') => {
            state.global.toggle_compact();
        }
        KeyCode::Char('w') => {
            state.global.toggle_show_windows();
            state.rebuild_row_targets();
        }
        KeyCode::Char('?') => {
            state.toggle_help_popup();
        }
        KeyCode::Char('m') => {
            if state.bottom_panel_height > 0 {
                state.toggle_bottom_minimized();
            }
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
        KeyCode::BackTab => {
            state.next_bottom_tab();
        }
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
            } else {
                state.focus_state.focus = Focus::BottomPanel;
            }
        }
        Focus::BottomPanel => state.scroll_bottom(1),
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
        Focus::BottomPanel => {
            let at_top = match state.bottom_tab {
                BottomTab::Feed => state.feed.scroll.offset == 0,
                BottomTab::Repos => state.scrolls.repos.offset == 0,
            };
            if at_top {
                state.focus_state.focus = Focus::Panes;
            } else {
                state.scroll_bottom(-1);
            }
        }
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
    use crate::group::RepoGroup;
    use crate::state::{PopupState, RowTarget, SpawnField};

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
    fn arrows_switch_bottom_tab_while_panel_focused() {
        let mut state = state_with_three_panes();
        state.focus_state.focus = Focus::BottomPanel;
        assert_eq!(state.bottom_tab, BottomTab::Feed);

        handle_key_event(key(KeyCode::Right), &mut state);
        assert_eq!(state.bottom_tab, BottomTab::Repos);
        handle_key_event(key(KeyCode::Left), &mut state);
        assert_eq!(state.bottom_tab, BottomTab::Feed);

        // h/l mirror the arrows, matching j/k pane navigation.
        handle_key_event(key(KeyCode::Char('l')), &mut state);
        assert_eq!(state.bottom_tab, BottomTab::Repos);
        handle_key_event(key(KeyCode::Char('h')), &mut state);
        assert_eq!(state.bottom_tab, BottomTab::Feed);
    }

    #[test]
    fn arrows_do_not_switch_tabs_from_panes_focus() {
        let mut state = state_with_three_panes();
        assert_eq!(state.focus_state.focus, Focus::Panes);

        handle_key_event(key(KeyCode::Right), &mut state);
        handle_key_event(key(KeyCode::Left), &mut state);
        assert_eq!(state.bottom_tab, BottomTab::Feed);
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
        // activity log or filter bar too, not just Panes focus.
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
        // `?`) closes it. `j` must not move the selection underneath.
        let mut state = state_with_three_panes();
        state.toggle_help_popup();

        handle_key_event(key(KeyCode::Char('j')), &mut state);
        assert!(state.is_help_open());
        assert_eq!(state.global.selected_pane_row, 0);

        handle_key_event(key(KeyCode::Char('q')), &mut state);
        assert!(!state.quit_requested, "q must not quit through the popup");

        handle_key_event(key(KeyCode::Esc), &mut state);
        assert!(!state.is_help_open());
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
    fn m_toggles_bottom_minimize() {
        // `m` is global (any non-modal focus) like `c`, so the panel can be
        // minimized without first returning to the pane list.
        let mut state = state_with_three_panes();
        assert!(!state.bottom_minimized);

        handle_key_event(key(KeyCode::Char('m')), &mut state);
        assert!(state.bottom_minimized);
        assert!(state.flash.is_none(), "no toggle feedback expected");

        handle_key_event(key(KeyCode::Char('m')), &mut state);
        assert!(!state.bottom_minimized);
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
}
