use super::{AppState, BottomTab};

impl AppState {
    /// Auto-switch the bottom tab when a brand-new agent appears in the
    /// focused pane: the Feed is the "something just happened" surface, so
    /// a new agent landing is exactly when the user wants it visible.
    /// Otherwise the current tab is left alone — both tabs are global
    /// cross-agent views now, so there is no per-pane preference to save
    /// or restore.
    ///
    /// `detect_new_agents` also prunes panes that disappeared (removing
    /// their seen status), so relaunching an agent in the same pane is
    /// detected as new. It must run every tick regardless of focus.
    pub(crate) fn auto_switch_tab(&mut self) {
        let new_agent_ids = self.detect_new_agents();
        if let Some(ref focused) = self.focus_state.focused_pane_id
            && new_agent_ids.contains(focused)
        {
            self.bottom_tab = BottomTab::Feed;
        }
    }

    /// Register all current agent panes. Returns IDs of newly appeared agents.
    /// Also removes agents that have disappeared so that re-launching
    /// an agent in the same pane is detected as new.
    fn detect_new_agents(&mut self) -> std::collections::HashSet<String> {
        let mut current: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut new_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for group in &self.repo_groups {
            for (pane, _) in &group.panes {
                current.insert(pane.pane_id.clone());
                if self.pane_states.seen.insert(pane.pane_id.clone()) {
                    new_ids.insert(pane.pane_id.clone());
                }
            }
        }
        // Remove disappeared agents from the seen set so that relaunching
        // an agent is detected as new.
        let removed: Vec<String> = self
            .pane_states
            .seen
            .iter()
            .filter(|id| !current.contains(id.as_str()))
            .cloned()
            .collect();
        for id in &removed {
            self.pane_states.seen.remove(id);
        }
        new_ids
    }

    pub fn next_bottom_tab(&mut self) {
        self.bottom_tab = match self.bottom_tab {
            BottomTab::Feed => BottomTab::Repos,
            BottomTab::Repos => BottomTab::Feed,
        };
    }

    /// Handle mouse click on the bottom panel tab header.
    ///
    /// The clickable column ranges are recomputed every frame by
    /// `draw_bottom` (the Feed label's width varies with the
    /// unseen-events badge) and stored on `layout`; the fallback matches
    /// the badge-less static title for frames before the first draw.
    pub fn handle_bottom_tab_click(&mut self, col: u16) {
        let (feed_range, repos_range) = self.layout.bottom_tab_click_cols;
        let x = col as usize;
        if x >= feed_range.0 && x < feed_range.1 {
            self.bottom_tab = BottomTab::Feed;
        } else if x >= repos_range.0 && x < repos_range.1 {
            self.bottom_tab = BottomTab::Repos;
        }
    }

    pub fn scroll_bottom(&mut self, delta: isize) {
        match self.bottom_tab {
            BottomTab::Feed => self.feed.scroll.scroll(delta),
            BottomTab::Repos => self.scrolls.repos.scroll(delta),
        }
    }

    /// Effective height of the bottom panel this frame, accounting for the
    /// `@sidebar_bottom_height` setting (0 = hidden) and the minimized state
    /// (collapsed to just the tab header row).
    pub fn effective_bottom_height(&self) -> u16 {
        if self.bottom_panel_height == 0 {
            0
        } else if self.bottom_minimized {
            crate::ui::MINIMIZED_BOTTOM_HEIGHT
        } else {
            self.bottom_panel_height
        }
    }

    /// Re-evaluate auto-minimize against the current terminal height. A user
    /// override from `m` — shared across sidebars via
    /// [`GlobalState::bottom_minimize_override`] — pins the choice;
    /// otherwise the panel minimizes when the viewport is too short to fit
    /// the configured panel plus [`crate::ui::AUTO_MINIMIZE_MIN_LIST_ROWS`]
    /// list rows.
    pub fn apply_auto_minimize(&mut self, term_height: u16) {
        if self.global.bottom_minimize_override.is_none() {
            let needed = self
                .bottom_panel_height
                .saturating_add(crate::ui::AUTO_MINIMIZE_MIN_LIST_ROWS);
            self.bottom_minimized = term_height <= needed;
        }
    }

    /// Toggle the bottom panel between expanded and minimized, pinning the
    /// choice so it no longer follows the viewport. The pin is saved to the
    /// `@sidebar_bottom_minimized` tmux global and broadcast, so every open
    /// sidebar (and any opened later) collapses or expands in lockstep.
    /// No-op when the panel is hidden entirely (`@sidebar_bottom_height 0`).
    pub fn toggle_bottom_minimized(&mut self) {
        if self.bottom_panel_height == 0 {
            return;
        }
        let next = !self.bottom_minimized;
        self.bottom_minimized = next;
        self.global.set_bottom_minimize_override(Some(next));
    }

    /// Re-apply the shared minimize pin after a `GlobalState` sync. A pin
    /// written by another sidebar must land on the effective collapsed
    /// state immediately — the periodic auto-minimize pass is a no-op
    /// while a pin exists, so without this the toggle would lag a tick.
    pub fn adopt_shared_bottom_minimize(&mut self) {
        if let Some(minimized) = self.global.bottom_minimize_override {
            self.bottom_minimized = minimized;
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

    fn test_pane(id: &str) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: true,
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
        }
    }

    fn agent_group(pane_id: &str) -> RepoGroup {
        RepoGroup {
            name: "project".into(),
            has_focus: true,
            panes: vec![(test_pane(pane_id), PaneGitInfo::default())],
        }
    }

    fn state_with_groups(repo_groups: Vec<RepoGroup>, focused_pane_id: Option<&str>) -> AppState {
        let mut state = AppState::new("%99".into());
        state.repo_groups = repo_groups;
        state.focus_state.focused_pane_id = focused_pane_id.map(str::to_string);
        state
    }

    // ─── detect_new_agents ──────────────────────────────────────

    #[test]
    fn detect_new_agents_empty() {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![];
        assert!(state.detect_new_agents().is_empty());
    }

    #[test]
    fn detect_new_agents_first_time() {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![agent_group("%1")];
        let new_ids = state.detect_new_agents();
        assert!(new_ids.contains("%1"));
        assert!(state.pane_states.seen.contains("%1"));
    }

    #[test]
    fn detect_new_agents_already_seen() {
        let mut state = AppState::new("%99".into());
        state.pane_states.seen.insert("%1".into());
        state.repo_groups = vec![agent_group("%1")];
        assert!(state.detect_new_agents().is_empty());
    }

    #[test]
    fn detect_new_agents_prunes_disappeared_panes() {
        let mut state = state_with_groups(vec![agent_group("%1")], Some("%1"));
        state.auto_switch_tab();
        assert!(state.pane_states.seen.contains("%1"));

        // Agent %1 closes: the seen entry must go so a relaunch in the
        // same pane counts as new again.
        state.repo_groups = vec![];
        state.auto_switch_tab();
        assert!(!state.pane_states.seen.contains("%1"));
    }

    // ─── auto_switch_tab ────────────────────────────────────────

    #[test]
    fn new_agent_in_focused_pane_switches_to_feed() {
        let mut state = state_with_groups(vec![], Some("%1"));
        state.bottom_tab = BottomTab::Repos;
        state.auto_switch_tab();
        assert_eq!(
            state.bottom_tab,
            BottomTab::Repos,
            "no agent yet: stays put"
        );

        state.repo_groups = vec![agent_group("%1")];
        state.auto_switch_tab();
        assert_eq!(state.bottom_tab, BottomTab::Feed, "new agent in focus");
    }

    #[test]
    fn new_agent_elsewhere_keeps_current_tab() {
        let mut state = state_with_groups(vec![], Some("%5"));
        state.auto_switch_tab();

        state.bottom_tab = BottomTab::Repos;
        state.repo_groups = vec![agent_group("%1")];
        state.auto_switch_tab();
        assert_eq!(
            state.bottom_tab,
            BottomTab::Repos,
            "agent in a different pane must not force a switch"
        );
    }

    #[test]
    fn existing_agent_does_not_retrigger_feed_switch() {
        let mut state = state_with_groups(vec![agent_group("%1")], Some("%1"));
        state.auto_switch_tab();
        assert_eq!(state.bottom_tab, BottomTab::Feed);

        // User moves to Repos; further ticks (no new agents) respect it.
        state.bottom_tab = BottomTab::Repos;
        state.auto_switch_tab();
        state.auto_switch_tab();
        assert_eq!(state.bottom_tab, BottomTab::Repos);
    }

    #[test]
    fn relaunched_agent_in_same_pane_switches_to_feed() {
        let mut state = state_with_groups(vec![agent_group("%1")], Some("%1"));
        state.auto_switch_tab();

        // Agent closes...
        state.repo_groups = vec![];
        state.auto_switch_tab();
        // ...and relaunches in the same pane with focus already there.
        state.repo_groups = vec![agent_group("%1")];
        state.auto_switch_tab();
        assert_eq!(state.bottom_tab, BottomTab::Feed);
    }

    // ─── next_bottom_tab / scroll_bottom ─────────────────────────

    #[test]
    fn next_bottom_tab_toggles() {
        let mut state = AppState::new("%99".into());
        assert_eq!(state.bottom_tab, BottomTab::Feed);
        state.next_bottom_tab();
        assert_eq!(state.bottom_tab, BottomTab::Repos);
        state.next_bottom_tab();
        assert_eq!(state.bottom_tab, BottomTab::Feed);
    }

    #[test]
    fn scroll_bottom_routes_to_the_active_tab() {
        let mut state = AppState::new("%99".into());
        state.feed.entries = (0..10)
            .map(|i| crate::state::FeedEntry {
                epoch: 0,
                timestamp: format!("10:{i:02}"),
                agent: AgentType::Claude,
                kind: crate::activity::FeedKind::FileChange,
                label: "x".into(),
            })
            .collect();
        state.feed.scroll.total_lines = 10;
        state.feed.scroll.visible_height = 5;
        state.scrolls.repos.total_lines = 10;
        state.scrolls.repos.visible_height = 5;

        state.scroll_bottom(3);
        assert_eq!(state.feed.scroll.offset, 3);
        assert_eq!(state.scrolls.repos.offset, 0);

        state.bottom_tab = BottomTab::Repos;
        state.scroll_bottom(2);
        assert_eq!(state.scrolls.repos.offset, 2);
        assert_eq!(state.feed.scroll.offset, 3);
    }

    // ─── handle_bottom_tab_click ──────────────────────────────────

    #[test]
    fn tab_click_uses_frame_stored_ranges() {
        let mut state = AppState::new("%99".into());
        // Badge-widened layout as stored by draw_bottom:
        // " Feed ·2 │ Repos" — feed 1..9, repos 12..18.
        state.layout.bottom_tab_click_cols = ((1, 9), (12, 18));

        state.handle_bottom_tab_click(3);
        assert_eq!(state.bottom_tab, BottomTab::Feed);
        state.handle_bottom_tab_click(14);
        assert_eq!(state.bottom_tab, BottomTab::Repos);
        // The separator and the border column are dead zones.
        state.handle_bottom_tab_click(10);
        assert_eq!(state.bottom_tab, BottomTab::Repos, "sep click keeps tab");
        state.handle_bottom_tab_click(0);
        assert_eq!(state.bottom_tab, BottomTab::Repos, "border click keeps tab");
    }

    #[test]
    fn tab_click_falls_back_to_static_ranges_before_first_frame() {
        let mut state = AppState::new("%99".into());
        state.handle_bottom_tab_click(3);
        assert_eq!(state.bottom_tab, BottomTab::Feed);
        state.handle_bottom_tab_click(11);
        assert_eq!(state.bottom_tab, BottomTab::Repos);
    }

    // ─── bottom panel minimize ──────────────────────────────────

    #[test]
    fn effective_height_uses_configured_value_when_expanded() {
        let mut state = AppState::new("%99".into());
        state.bottom_panel_height = 20;
        state.bottom_minimized = false;
        assert_eq!(state.effective_bottom_height(), 20);
    }

    #[test]
    fn effective_height_collapses_to_header_when_minimized() {
        let mut state = AppState::new("%99".into());
        state.bottom_panel_height = 20;
        state.bottom_minimized = true;
        assert_eq!(
            state.effective_bottom_height(),
            crate::ui::MINIMIZED_BOTTOM_HEIGHT
        );
    }

    #[test]
    fn effective_height_stays_zero_when_panel_hidden() {
        let mut state = AppState::new("%99".into());
        state.bottom_panel_height = 0;
        state.bottom_minimized = true;
        assert_eq!(state.effective_bottom_height(), 0);
    }

    #[test]
    fn auto_minimize_triggers_when_list_would_be_squeezed() {
        let mut state = AppState::new("%99".into());
        state.bottom_panel_height = 20;
        let threshold = 20 + crate::ui::AUTO_MINIMIZE_MIN_LIST_ROWS;
        state.apply_auto_minimize(threshold);
        assert!(state.bottom_minimized);
        state.apply_auto_minimize(threshold + 1);
        assert!(!state.bottom_minimized);
    }

    #[test]
    fn auto_minimize_threshold_tracks_configured_panel_height() {
        let mut state = AppState::new("%99".into());
        state.bottom_panel_height = 30;
        // A 26-row viewport clears a fixed 24 cutoff but must still minimize
        // against a 30-row panel.
        state.apply_auto_minimize(26);
        assert!(state.bottom_minimized);
    }

    #[test]
    fn auto_minimize_respects_manual_override() {
        let mut state = AppState::new("%99".into());
        state.toggle_bottom_minimized();
        assert!(state.bottom_minimized);
        // A tall viewport must not undo the user's explicit minimize.
        state.apply_auto_minimize(60);
        assert!(state.bottom_minimized);
    }

    #[test]
    fn toggle_bottom_minimized_pins_the_choice() {
        let mut state = AppState::new("%99".into());
        state.toggle_bottom_minimized();
        assert!(state.bottom_minimized);
        assert_eq!(state.global.bottom_minimize_override, Some(true));
        state.toggle_bottom_minimized();
        assert!(!state.bottom_minimized);
        assert_eq!(state.global.bottom_minimize_override, Some(false));
    }

    #[test]
    fn shared_minimize_pin_is_adopted_on_sync() {
        let mut state = AppState::new("%99".into());
        // Another sidebar pinned minimized; the sync lands it here even
        // though this viewport is tall enough to stay expanded.
        state.global.bottom_minimize_override = Some(true);
        state.adopt_shared_bottom_minimize();
        assert!(state.bottom_minimized);

        // No pin (viewport-driven mode): adoption must not touch the
        // effective state — the next auto-minimize pass owns it.
        let mut state = AppState::new("%99".into());
        state.bottom_minimized = true;
        state.global.bottom_minimize_override = None;
        state.adopt_shared_bottom_minimize();
        assert!(state.bottom_minimized);
    }

    #[test]
    fn toggle_bottom_minimized_noop_when_panel_hidden() {
        let mut state = AppState::new("%99".into());
        state.bottom_panel_height = 0;
        state.toggle_bottom_minimized();
        assert!(!state.bottom_minimized);
        assert_eq!(state.global.bottom_minimize_override, None);
    }
}
