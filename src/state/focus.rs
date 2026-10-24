use super::AppState;
use crate::cli::toggle::popup_max_width_from_setting;
use crate::tmux;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Focus {
    Filter,
    #[default]
    Panes,
}

#[derive(Debug, Clone)]
pub struct FocusState {
    pub sidebar_focused: bool,
    /// Whether the sidebar's window is the session's active window, as of
    /// the last successful tmux snapshot. Stored on the state so a failed
    /// query can ride out the tick on the last known-good value instead of
    /// fabricating `false` (which would trip the event loop's inactive
    /// reload path).
    pub window_active: bool,
    pub focus: Focus,
    pub focused_pane_id: Option<String>,
    pub prev_focused_pane_id: Option<String>,
}

impl FocusState {
    pub fn new() -> Self {
        Self {
            sidebar_focused: false,
            window_active: false,
            focus: Focus::Panes,
            focused_pane_id: None,
            prev_focused_pane_id: None,
        }
    }
}

impl Default for FocusState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn pane_by_id(&self, pane_id: &str) -> Option<&tmux::PaneInfo> {
        for group in &self.repo_groups {
            for (pane, _) in &group.panes {
                if pane.pane_id == pane_id {
                    return Some(pane);
                }
            }
        }
        None
    }

    pub fn selected_pane(&self) -> Option<&tmux::PaneInfo> {
        let target = self
            .layout
            .pane_row_targets
            .get(self.global.selected_pane_row)?;
        self.pane_by_id(&target.pane_id)
    }

    /// Resolve the focused (non-sidebar) pane from the sidebar's window
    /// pane list, which arrives as part of the same single `list-panes -a`
    /// snapshot as everything else in the tick — this used to be its own
    /// tmux query. The list covers all panes in the window, not just agent
    /// panes, so activity/git info can be displayed even when the focused
    /// pane has no agent running. When the sidebar has focus the list
    /// yields no active non-sidebar pane — preserve the previously focused
    /// pane so row data stays stable.
    pub(crate) fn find_focused_pane_from(&mut self, panes: &[(String, bool, String)]) {
        if let Some((id, _)) = tmux::pick_active_pane(&self.tmux_pane, panes) {
            self.focus_state.focused_pane_id = Some(id);
        }
    }

    /// Move agent selection. Returns true if moved, false if at boundary.
    pub fn move_pane_selection(&mut self, delta: isize) -> bool {
        if self.layout.pane_row_targets.is_empty() {
            return false;
        }
        let len = self.layout.pane_row_targets.len() as isize;
        let next = self.global.selected_pane_row as isize + delta;
        if next >= 0 && next < len {
            self.global.selected_pane_row = next as usize;
            true
        } else {
            false
        }
    }

    /// Page-jump the selection by roughly one viewport of rows. A page
    /// that overshoots clamps to the
    /// edge row: paging to the end should land on the last row, not fall
    /// through. The page size is the agent list's last rendered visible
    /// height (the same viewport the render's auto-scroll keeps the
    /// selection within), with a 1-row floor for keypresses before the
    /// first frame. Returns whether the selection moved.
    pub fn page_pane_selection(&mut self, direction: isize) -> bool {
        let len = self.layout.pane_row_targets.len();
        if len == 0 {
            return false;
        }
        let page = self.scrolls.panes.visible_height.max(1) as isize;
        let current = self.global.selected_pane_row as isize;
        let next = (current + direction * page).clamp(0, len as isize - 1);
        if next == current {
            return false;
        }
        self.global.selected_pane_row = next as usize;
        true
    }

    pub fn activate_selected_pane(&mut self) {
        let Some(target) = self
            .layout
            .pane_row_targets
            .get(self.global.selected_pane_row)
        else {
            return;
        };
        let target_pane_id = target.pane_id.clone();

        // Update the sidebar immediately so the active marker and
        // repo header highlight move without waiting for the next
        // periodic tmux refresh.
        self.focus_state.focused_pane_id = Some(target_pane_id.clone());
        tmux::select_pane(&target_pane_id);

        // The jump may land on a window that has no sidebar of its own —
        // `@sidebar_auto_create off`, or one the mobile takeover closed
        // while a narrow client was attached to its session. Summon one
        // there so the sidebar "follows" the jump instead of disappearing.
        // Window rows summon too: under toggle-all the user opted into
        // sidebars on every window, and the create-only path still
        // respects the recorded mode, `@sidebar_auto_create`, and the
        // popup threshold, so this never spawns where the user keeps an
        // editor or a test run unless the mode says so. create-only is a
        // no-op when that window already has a sidebar.
        let window_id = tmux::display_message(&target_pane_id, "#{window_id}");
        if window_id.is_empty() {
            return;
        }
        // Below the popup threshold a window is popup territory: a forced
        // split would strand a sliver, and the summon must not fight the
        // auto-hide. This reads the post-jump width — `select_pane` may
        // have just grown the window (the switched-to client becomes the
        // latest active one, and `window-size latest` resizes to it), so
        // a desktop jump into a session a phone was driving summons
        // normally while a genuinely narrow arrival does not.
        let width: u32 = tmux::display_message(&window_id, "#{window_width}")
            .parse()
            .unwrap_or(0);
        let threshold = popup_max_width_from_setting(&tmux::display_message(
            &window_id,
            &format!("#{{{}}}", tmux::SIDEBAR_POPUP_MAX_WIDTH),
        ));
        if threshold > 0 && width > 0 && width < threshold {
            return;
        }
        let pane_path = tmux::display_message(&target_pane_id, "#{pane_current_path}");
        let pane_path = if pane_path.is_empty() {
            "~".to_string()
        } else {
            pane_path
        };
        crate::cli::toggle::cmd_toggle(&["--create-only".to_string(), window_id, pane_path]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_default_is_panes() {
        assert_eq!(Focus::default(), Focus::Panes);
    }

    #[test]
    fn focus_state_new_has_expected_initial_values() {
        let state = FocusState::new();
        assert!(!state.sidebar_focused);
        assert_eq!(state.focus, Focus::Panes);
        assert!(state.focused_pane_id.is_none());
        assert!(state.prev_focused_pane_id.is_none());
    }

    #[test]
    fn focus_state_default_delegates_to_new() {
        let state = FocusState::default();
        assert!(!state.sidebar_focused);
        assert_eq!(state.focus, Focus::Panes);
        assert!(state.focused_pane_id.is_none());
        assert!(state.prev_focused_pane_id.is_none());
    }

    // ─── AppState focus accessors ────────────────────────────────────

    use crate::group::{PaneGitInfo, RepoGroup};
    use crate::state::layout::RowTarget;
    use crate::tmux::{
        AgentType, PaneAttention, PaneInfo, PaneStatus, PermissionMode, WorktreeMetadata,
    };

    fn test_pane(id: &str) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status: PaneStatus::Running,
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
        }
    }

    #[test]
    fn pane_by_id_searches_all_groups() {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![
            RepoGroup {
                name: "alpha".into(),
                has_focus: true,
                panes: vec![(test_pane("%1"), PaneGitInfo::default())],
            },
            RepoGroup {
                name: "beta".into(),
                has_focus: false,
                panes: vec![(test_pane("%42"), PaneGitInfo::default())],
            },
        ];

        assert!(state.pane_by_id("%1").is_some());
        assert!(state.pane_by_id("%42").is_some());
        assert!(state.pane_by_id("%does-not-exist").is_none());
    }

    #[test]
    fn selected_pane_tracks_pane_row_targets() {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![RepoGroup {
            name: "alpha".into(),
            has_focus: true,
            panes: vec![
                (test_pane("%1"), PaneGitInfo::default()),
                (test_pane("%2"), PaneGitInfo::default()),
            ],
        }];
        state.layout.pane_row_targets = vec![
            RowTarget {
                pane_id: "%1".into(),
                is_window: false,
            },
            RowTarget {
                pane_id: "%2".into(),
                is_window: false,
            },
        ];

        state.global.selected_pane_row = 0;
        assert_eq!(
            state.selected_pane().map(|p| p.pane_id.as_str()),
            Some("%1")
        );
        state.global.selected_pane_row = 1;
        assert_eq!(
            state.selected_pane().map(|p| p.pane_id.as_str()),
            Some("%2")
        );
        // Out-of-range cursor → None (not a panic).
        state.global.selected_pane_row = 99;
        assert!(state.selected_pane().is_none());
    }

    #[test]
    fn move_pane_selection_stays_within_bounds() {
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
        ];
        state.global.selected_pane_row = 0;
        assert!(!state.move_pane_selection(-1));
        assert!(state.move_pane_selection(1));
        assert_eq!(state.global.selected_pane_row, 1);
        assert!(!state.move_pane_selection(5));
    }
}
