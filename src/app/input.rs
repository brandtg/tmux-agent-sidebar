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
                state.handle_mouse_scroll(3);
                true
            }
            MouseEventKind::ScrollUp => {
                state.handle_mouse_scroll(-3);
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
        KeyCode::Char('z') => {
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

fn pane_nav_down(state: &mut AppState) {
    match state.focus_state.focus {
        Focus::Filter => {
            state.focus_state.focus = Focus::Panes;
        }
        Focus::Panes => {
            if state.move_pane_selection(1) {
                state.global.queue_cursor_save();
            }
        }
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
        Focus::Filter => {}
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
