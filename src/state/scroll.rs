#[derive(Debug, Clone, Default)]
pub struct ScrollState {
    pub offset: usize,
    pub total_lines: usize,
    pub visible_height: usize,
}

impl ScrollState {
    pub fn scroll(&mut self, delta: isize) {
        let max = self.total_lines.saturating_sub(self.visible_height);
        let next = self.offset as isize + delta;
        self.offset = next.max(0).min(max as isize) as usize;
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScrollStates {
    pub panes: ScrollState,
    /// Line-based scroll offset for the manual TODO panel.
    pub todos: ScrollState,
}

impl super::AppState {
    /// Select the row of the agent matching the current tmux context: the
    /// pane that held focus in this window just before the sidebar itself
    /// was focused. Across the focus-gain tick `find_focused_pane_from`
    /// preserves that id (the sidebar is the active pane then, so
    /// `pick_active_pane` yields nothing new), which is what makes it
    /// readable here. Returns `true` when a row was selected; when the
    /// context pane is unknown or filtered out the caller keeps the
    /// cursor (and the viewport) exactly where the user left them —
    /// the selection is never reset to row 0, and the scroll offset is
    /// not parked: erasing either on an unfocused tick made the
    /// highlight, Enter, and the viewport land somewhere else after the
    /// user returned from a jump. The offset only moves while focused
    /// (`compute_scroll_offset` follows the selection then), so a
    /// preserved cursor is still in view when focus returns.
    pub fn select_focused_context_row(&mut self) -> bool {
        // In a popup the tracked focus id stays empty — `TMUX_PANE` is
        // pinned to the window's active pane at popup-open time and
        // `pick_active_pane` excludes that pane — so the pinned pane
        // itself is the context the popup was opened from.
        let context_pane_id = self
            .focus_state
            .focused_pane_id
            .clone()
            .or_else(|| self.popup_mode.then(|| self.tmux_pane.clone()));
        let Some(pane_id) = context_pane_id else {
            return false;
        };
        if let Some(index) = self
            .layout
            .pane_row_targets
            .iter()
            .position(|target| target.pane_id == pane_id)
        {
            self.global.selected_pane_row = index;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_state_default_is_all_zero() {
        let state = ScrollState::default();
        assert_eq!(state.offset, 0);
        assert_eq!(state.total_lines, 0);
        assert_eq!(state.visible_height, 0);
    }

    #[test]
    fn scroll_forward_advances_within_bounds() {
        let mut state = ScrollState {
            offset: 0,
            total_lines: 20,
            visible_height: 5,
        };
        state.scroll(3);
        assert_eq!(state.offset, 3);
    }

    #[test]
    fn scroll_clamps_to_max_offset() {
        // max = total_lines - visible_height = 15
        let mut state = ScrollState {
            offset: 10,
            total_lines: 20,
            visible_height: 5,
        };
        state.scroll(100);
        assert_eq!(
            state.offset, 15,
            "offset should clamp to total_lines - visible_height"
        );
    }

    #[test]
    fn scroll_clamps_to_zero_on_negative_overshoot() {
        let mut state = ScrollState {
            offset: 2,
            total_lines: 20,
            visible_height: 5,
        };
        state.scroll(-50);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn scroll_with_visible_height_exceeding_total_clamps_to_zero() {
        // total_lines.saturating_sub(visible_height) = 0, so any forward
        // scroll collapses back to 0 rather than underflowing.
        let mut state = ScrollState {
            offset: 0,
            total_lines: 3,
            visible_height: 10,
        };
        state.scroll(5);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn scroll_states_default_both_at_default() {
        let states = ScrollStates::default();
        assert_eq!(states.panes.offset, 0);
        assert_eq!(states.panes.total_lines, 0);
        assert_eq!(states.panes.visible_height, 0);
        assert_eq!(states.panes.offset, 0);
        assert_eq!(states.panes.total_lines, 0);
        assert_eq!(states.panes.visible_height, 0);
        assert_eq!(states.todos.offset, 0);
        assert_eq!(states.todos.total_lines, 0);
        assert_eq!(states.todos.visible_height, 0);
    }

    // ─── select_focused_context_row ──────────────────────────────────

    use crate::state::layout::RowTarget;

    fn row_target(pane_id: &str) -> RowTarget {
        RowTarget {
            pane_id: pane_id.into(),
            is_window: false,
        }
    }

    #[test]
    fn select_focused_context_row_selects_row_of_tracked_focus() {
        let mut state = super::super::AppState::new("%99".into());
        state.layout.pane_row_targets = vec![row_target("%1"), row_target("%2"), row_target("%3")];
        state.focus_state.focused_pane_id = Some("%3".into());
        state.global.selected_pane_row = 0;
        state.scrolls.panes.offset = 9;

        assert!(state.select_focused_context_row());
        assert_eq!(state.global.selected_pane_row, 2);
        assert_eq!(
            state.scrolls.panes.offset, 9,
            "the viewport is preserved across the refocus; render auto-scroll only nudges it if the row fell out of view"
        );
    }

    #[test]
    fn select_focused_context_row_keeps_cursor_and_viewport_when_unlisted() {
        // The context pane is filtered out of the list (or is a non-agent
        // pane with no window row): no row is selected, and the cursor —
        // and the viewport — stay exactly where the user left them.
        // Used to park the scroll at the top, which made the viewport
        // jump on refocus when the preserved cursor sat below the first
        // screen (audit 2026-11-16, "viewport jumps when I tab back").
        let mut state = super::super::AppState::new("%99".into());
        state.layout.pane_row_targets = vec![row_target("%1")];
        state.focus_state.focused_pane_id = Some("%404".into());
        state.global.selected_pane_row = 5;
        state.scrolls.panes.offset = 12;

        assert!(!state.select_focused_context_row());
        assert_eq!(state.global.selected_pane_row, 5);
        assert_eq!(state.scrolls.panes.offset, 12);
    }

    #[test]
    fn select_focused_context_row_without_context_returns_false() {
        let mut state = super::super::AppState::new("%99".into());
        state.layout.pane_row_targets = vec![row_target("%1")];

        assert!(!state.select_focused_context_row());
    }

    #[test]
    fn select_focused_context_row_pinned_pane_is_context_in_popup_mode() {
        // In a popup `pick_active_pane` excludes the pinned TMUX_PANE, so
        // the tracked focus id stays empty — the pinned pane itself is the
        // context the popup was opened from.
        let mut state = super::super::AppState::new("%7".into());
        state.popup_mode = true;
        state.layout.pane_row_targets = vec![row_target("%1"), row_target("%7")];
        assert!(state.focus_state.focused_pane_id.is_none());

        assert!(state.select_focused_context_row());
        assert_eq!(state.global.selected_pane_row, 1);
    }
}
