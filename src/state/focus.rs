use super::AppState;
use crate::cli::toggle::popup_max_width_from_setting;
use crate::tmux;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Focus {
    Filter,
    #[default]
    Panes,
    /// The manual TODO panel owns the keyboard.
    Todos,
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
    /// Agent pane standing in for the user's cursor while it sits on a
    /// non-agent pane whose row is not rendered: the first agent sharing
    /// the cursor's window. Recomputed on every snapshot by
    /// [`AppState::resolve_proxy_focus`]; a deliberate jump clears it so
    /// the optimistic `focused_pane_id` rules until fresh data lands.
    /// The render layer applies it only while the `w` toggle keeps
    /// window rows hidden — with them visible, the window row carries
    /// the active marker itself.
    pub proxy_focused_pane_id: Option<String>,
    pub prev_focused_pane_id: Option<String>,
}

impl FocusState {
    pub fn new() -> Self {
        Self {
            sidebar_focused: false,
            window_active: false,
            focus: Focus::Panes,
            focused_pane_id: None,
            proxy_focused_pane_id: None,
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

    /// Resolve the proxy focus: when the cursor sits on a non-agent pane
    /// and the window rows that would carry its active marker are hidden,
    /// no agent row can light up and the user loses the "where am I" cue
    /// — so the first agent sharing the cursor's window stands in for it.
    ///
    /// Resolved once per snapshot (`apply_session_snapshot`) rather than
    /// per frame: a deliberate jump optimistically sets `focused_pane_id`
    /// and clears this proxy, so the marker names the jumped-to pane for
    /// the tick in flight even though the snapshot still shows the old
    /// cursor. Toggling `w` needs no re-resolution — the render layer
    /// decides applicability from the live toggle state.
    pub(crate) fn resolve_proxy_focus(&mut self) {
        self.focus_state.proxy_focused_pane_id = self.compute_proxy_focus();
    }

    /// The cursor scan reads only data already in the last snapshot: the
    /// active non-agent pane is the `other_windows` entry with
    /// `session_attached && window_active && pane_active` — the same
    /// predicate the window-row marker (`row_collector`) and
    /// `mark_active_window_panes_seen` use. Sidebar panes never enter
    /// `other_windows` (they are skipped at collection), so sitting on
    /// the sidebar itself yields no proxy and the preserved
    /// `focused_pane_id` keeps ruling.
    fn compute_proxy_focus(&self) -> Option<String> {
        // Multiple attached clients can mark several panes active at
        // once; `other_windows` is an IndexMap in tmux line order, so
        // first-wins stays deterministic across frames.
        let cursor = self
            .other_windows
            .values()
            .flatten()
            .find(|w| w.session_attached && w.window_active && w.pane_active)?;
        // Group iteration order matches the render order's stable sort
        // (`ordered_group_rows`: session → window → pane), so the first
        // match is the cursor window's first pane in tmux order.
        self.repo_groups
            .iter()
            .flat_map(|group| group.panes.iter())
            .find(|(pane, _)| {
                pane.tmux_session == cursor.session_name && pane.window_index == cursor.window_index
            })
            .map(|(pane, _)| pane.pane_id.clone())
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

        // A jump ends the search: clear the query so this sidebar returns
        // to the full list. Jumping to another window already looked
        // cleared because that window's sidebar is a separate instance
        // with its own search state; clearing here makes a jump inside
        // the same window behave the same way.
        self.cancel_search();

        // Update the sidebar immediately so the active marker and
        // repo header highlight move without waiting for the next
        // periodic tmux refresh. The proxy stands down for the tick in
        // flight: the last snapshot still shows the previous cursor, so
        // an unresolved proxy would keep pointing at the window just
        // left instead of the pane being activated.
        self.focus_state.focused_pane_id = Some(target_pane_id.clone());
        self.focus_state.proxy_focused_pane_id = None;
        tmux::select_pane(&target_pane_id);

        // The jump may land on a window that has no sidebar of its own —
        // `@sidebar_auto_create off`, or one auto-hide closed while a
        // narrow client was attached. Summon one there so the sidebar
        // "follows" the jump instead of disappearing.
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

    /// Toggle the user's inbox flag (`@sidebar_flagged`) on the
    /// currently selected row — agent panes and non-agent window rows
    /// alike; a selection with no row behind it (header, empty list)
    /// does nothing. Writes the pane option first so the pin survives
    /// restarts and is visible to every sidebar instance, then flips
    /// the in-memory copy so the marker renders on the very next frame
    /// instead of waiting for the next tmux tick. The option write goes
    /// through `set_pane_option`, whose failures are traced like every
    /// other pane-option write (see `exec_tmux`).
    pub fn toggle_selected_flag(&mut self) {
        let Some(target) = self
            .layout
            .pane_row_targets
            .get(self.global.selected_pane_row)
        else {
            return;
        };
        let pane_id = target.pane_id.clone();

        let flagged = if target.is_window {
            let mut found = None;
            for windows in self.other_windows.values_mut() {
                for window in windows.iter_mut() {
                    if window.pane_id == pane_id {
                        window.flagged = !window.flagged;
                        found = Some(window.flagged);
                    }
                }
            }
            found
        } else {
            let mut found = None;
            for group in &mut self.repo_groups {
                for (pane, _) in &mut group.panes {
                    if pane.pane_id == pane_id {
                        pane.flagged = !pane.flagged;
                        found = Some(pane.flagged);
                    }
                }
            }
            found
        };
        let Some(flagged) = flagged else {
            return;
        };
        tmux::set_pane_option(
            &pane_id,
            tmux::PANE_FLAGGED,
            if flagged { "1" } else { "0" },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::{PANE_FLAGGED, test_mock};

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
            flagged: false,
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

    // ─── toggle_selected_flag ────────────────────────────────────────

    fn test_window_row(pane_id: &str) -> crate::group::OtherWindow {
        crate::group::OtherWindow {
            window_id: "@5".into(),
            window_index: 0,
            window_name: "tests".into(),
            window_active: false,
            session_name: "main".into(),
            session_attached: true,
            attention: PaneAttention::None,
            pane_id: pane_id.into(),
            pane_active: true,
            pane_pid: None,
            command: "cargo".into(),
            command_args: None,
            path: "/tmp".into(),
            git_info: crate::group::PaneGitInfo::default(),
            status: crate::tmux::WindowStatus::Idle,
            pane_title: String::new(),
            flagged: false,
        }
    }

    #[test]
    fn toggle_selected_flag_flips_window_row_and_writes_option() {
        let _guard = test_mock::install();
        let mut state = AppState::new("%99".into());
        state.layout.pane_row_targets = vec![RowTarget {
            pane_id: "%10".into(),
            is_window: true,
        }];
        state.global.selected_pane_row = 0;
        state
            .other_windows
            .insert("repo".into(), vec![test_window_row("%10")]);

        state.toggle_selected_flag();
        assert!(state.other_windows["repo"][0].flagged);
        assert_eq!(test_mock::get("%10", PANE_FLAGGED).as_deref(), Some("1"));

        state.toggle_selected_flag();
        assert!(!state.other_windows["repo"][0].flagged);
        assert_eq!(test_mock::get("%10", PANE_FLAGGED).as_deref(), Some("0"));
    }

    #[test]
    fn toggle_selected_flag_without_a_row_is_a_no_op() {
        let _guard = test_mock::install();
        let mut state = AppState::new("%99".into());
        // No rendered rows: the cursor points at nothing (header/empty
        // list). The toggle must neither panic nor write an option.
        state.toggle_selected_flag();
        assert_eq!(test_mock::get("%1", PANE_FLAGGED).as_deref(), Some(""));
    }

    // ─── proxy focus (w-off cursor parked on a hidden non-agent pane) ─

    /// An on-screen non-agent cursor: the predicate `compute_proxy_focus`
    /// scans for (`session_attached && window_active && pane_active`).
    fn cursor_in(session: &str, window_index: i64, pane_id: &str) -> crate::group::OtherWindow {
        let mut cursor = test_window_row(pane_id);
        cursor.session_name = session.into();
        cursor.window_index = window_index;
        cursor.session_attached = true;
        cursor.window_active = true;
        cursor.pane_active = true;
        cursor
    }

    fn agent_in(id: &str, session: &str, window_index: i64) -> PaneInfo {
        let mut pane = test_pane(id);
        pane.tmux_session = session.into();
        pane.window_index = window_index;
        pane
    }

    #[test]
    fn proxy_focus_names_first_agent_of_cursor_window() {
        let mut state = AppState::new("%99".into());
        state.global.show_windows = false;
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            has_focus: false,
            panes: vec![
                (agent_in("%1", "main", 0), PaneGitInfo::default()),
                (agent_in("%2", "main", 0), PaneGitInfo::default()),
            ],
        }];
        state
            .other_windows
            .insert("repo".into(), vec![cursor_in("main", 0, "%10")]);

        state.resolve_proxy_focus();
        assert_eq!(
            state.focus_state.proxy_focused_pane_id.as_deref(),
            Some("%1"),
            "the window's first pane in tmux order stands in for the hidden cursor"
        );
    }

    #[test]
    fn proxy_focus_ignores_windows_without_agents() {
        let mut state = AppState::new("%99".into());
        state.global.show_windows = false;
        // The agent lives in window 1; the cursor sits in window 2,
        // which has no agent panes to stand in.
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            has_focus: false,
            panes: vec![(agent_in("%1", "main", 1), PaneGitInfo::default())],
        }];
        state
            .other_windows
            .insert("repo".into(), vec![cursor_in("main", 0, "%10")]);

        state.resolve_proxy_focus();
        assert!(state.focus_state.proxy_focused_pane_id.is_none());
    }

    #[test]
    fn proxy_focus_needs_an_on_screen_cursor() {
        let mut state = AppState::new("%99".into());
        state.global.show_windows = false;
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            has_focus: false,
            panes: vec![(agent_in("%1", "main", 0), PaneGitInfo::default())],
        }];
        // A background window (no attached client) is nobody's cursor.
        let mut cursor = cursor_in("main", 0, "%10");
        cursor.window_active = false;
        state.other_windows.insert("repo".into(), vec![cursor]);

        state.resolve_proxy_focus();
        assert!(state.focus_state.proxy_focused_pane_id.is_none());
    }

    #[test]
    fn proxy_focus_prefers_cursor_window_over_stale_focus() {
        // The cursor sits with %2 in window 1; %1 is the stale preserved
        // focus from the window the user left. The proxy must name %2 —
        // the render side lets it outrank `focused_pane_id`.
        let mut state = AppState::new("%99".into());
        state.global.show_windows = false;
        state.focus_state.focused_pane_id = Some("%1".into());
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            has_focus: false,
            panes: vec![
                (agent_in("%1", "main", 0), PaneGitInfo::default()),
                (agent_in("%2", "main", 1), PaneGitInfo::default()),
            ],
        }];
        state
            .other_windows
            .insert("repo".into(), vec![cursor_in("main", 1, "%10")]);

        state.resolve_proxy_focus();
        assert_eq!(
            state.focus_state.proxy_focused_pane_id.as_deref(),
            Some("%2")
        );
    }
}
