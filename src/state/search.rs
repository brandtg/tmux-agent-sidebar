use super::AppState;

/// Local, per-sidebar search state.
///
/// Deliberately not part of [`super::GlobalState`]: the repo filter is
/// shared across sidebar instances through tmux options, but a search
/// query is transient and must not broadcast a refresh to every other
/// sidebar on every keystroke.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub query: String,
    /// Whether the `/` input line currently owns the keyboard. The query
    /// survives closing the input (`Enter`), so an applied filter stays
    /// visible until it is cleared.
    pub active: bool,
    /// Where the user's cursor was when `/` opened on an empty query —
    /// the position Esc restores. The search moves the cursor freely
    /// (every keystroke rebuilds the list), but the selection is the
    /// user's, not the search's (audit 2026-11-21 F-5). Captured only on
    /// a fresh open: reopening to refine an applied query keeps the
    /// original. Cleared when a user-driven move (plain j/k, page)
    /// supersedes it, and consumed by any cancel.
    pub pre_search_row: Option<usize>,
    /// Pane id of the captured row, so the restore re-anchors by identity
    /// rather than trusting an index into a list that may have changed.
    pub pre_search_pane_id: Option<String>,
}

impl SearchState {
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// A whitespace-only query behaves like no query at all.
    pub fn has_query(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// The trimmed query, lowercased once per collection pass. The
    /// matchers run per field per row, and lowering the query inside each
    /// call was dozens of avoidable allocations per frame during an
    /// active search (audit 2026-11-19 B6).
    pub fn lowercased_query(&self) -> String {
        self.query.trim().to_lowercase()
    }
}

/// Case-insensitive substring match against a pre-lowered query (see
/// [`SearchState::lowercased_query`]). Whitespace-only queries match
/// everything, which is what an empty search box means.
///
/// Compares without allocating: the old `haystack.to_lowercase()`
/// lowered every matched field of every row on both the rebuild and the
/// render pass (audit 2026-11-25 L-inp-2). Folding is per char, which
/// matches the old behavior for everything except the exotic
/// multi-char-fold cases (`İ`-style) where a lowered haystack grows by a
/// combining mark — the old code's substring match missed those too.
fn query_matches_lower(query_lower: &str, haystack: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    'starts: for (start, _) in haystack.char_indices() {
        let mut hay = &haystack[start..];
        for needle_char in query_lower.chars() {
            let Some(hay_char) = hay.chars().next() else {
                // Ran out of haystack mid-needle: no later start has
                // more room either.
                return false;
            };
            if !chars_eq_ignore_case(hay_char, needle_char) {
                continue 'starts;
            }
            hay = &hay[hay_char.len_utf8()..];
        }
        return true;
    }
    false
}

fn chars_eq_ignore_case(a: char, b: char) -> bool {
    if a == b {
        return true;
    }
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(&b);
    }
    a.to_lowercase().eq(b.to_lowercase())
}

impl AppState {
    /// Open the `/` input line. The current query (if any) is preserved so
    /// reopening lets the user refine rather than retype.
    pub fn open_search(&mut self) {
        // A fresh search captures the cursor for Esc to return to;
        // reopening to refine an applied query keeps the original
        // capture.
        if !self.search.has_query() {
            self.search.pre_search_row = Some(self.global.selected_pane_row);
            self.search.pre_search_pane_id = self
                .layout
                .pane_row_targets
                .get(self.global.selected_pane_row)
                .map(|target| target.pane_id.clone());
        }
        self.search.active = true;
        // Search acts on the list, so give it the selection highlight even
        // when the chyron happened to own the header row.
        self.focus_state.focus = super::Focus::Panes;
        // The input claims the header row: a chyron mid-fade is dismissed
        // rather than left to fight the input for row 0.
        self.chyron.current = None;
    }

    /// Leave the input line, keeping the query applied to the list.
    pub fn finish_search(&mut self) {
        self.search.active = false;
    }

    /// Cancel the search: close the input line and clear the query,
    /// restoring the unfiltered list — and the cursor with it. The
    /// keystroke rebuilds moved the selection freely; Esc returns it to
    /// where the user stood before `/` (audit 2026-11-21 F-5). Re-anchor
    /// by pane id against the unfiltered list so the restore lands on the
    /// same pane even if rows above it appeared or vanished; when the
    /// pane is gone, the captured index rides the normal clamp.
    pub fn cancel_search(&mut self) {
        self.cancel_search_inner(true);
    }

    /// Cancel without the restore, for the jump path
    /// (`activate_selected_pane`): the pane the user jumped to is their
    /// newest choice, taken from the post-search cursor, and must not be
    /// overwritten by the pre-search position.
    pub(super) fn cancel_search_for_jump(&mut self) {
        self.cancel_search_inner(false);
    }

    fn cancel_search_inner(&mut self, restore_cursor: bool) {
        self.search.active = false;
        let had_query = !self.search.query.is_empty();
        self.search.query.clear();
        let restore_row = self.search.pre_search_row.take();
        let restore_id = self.search.pre_search_pane_id.take();
        if had_query {
            self.rebuild_row_targets();
        }
        if !restore_cursor {
            return;
        }
        if let Some(row) = restore_row {
            let index = restore_id
                .as_deref()
                .and_then(|id| {
                    self.layout
                        .pane_row_targets
                        .iter()
                        .position(|target| target.pane_id == id)
                })
                .unwrap_or(row)
                .min(self.layout.pane_row_targets.len().saturating_sub(1));
            self.global.selected_pane_row = index;
            self.global.queue_cursor_save();
        }
    }

    /// Clear the query but keep editing (Ctrl-u).
    pub fn clear_search_query(&mut self) {
        if !self.search.query.is_empty() {
            self.search.query.clear();
            self.rebuild_row_targets();
        }
    }

    pub fn search_push_char(&mut self, c: char) {
        self.search.query.push(c);
        self.rebuild_row_targets();
    }

    pub fn search_pop_char(&mut self) {
        if self.search.query.pop().is_some() {
            self.rebuild_row_targets();
        }
    }

    /// Delete the word before the cursor (Ctrl-w): drop trailing
    /// whitespace, then the word itself, leaving the space that separated
    /// it from the previous word.
    pub fn search_pop_word(&mut self) {
        if self.search.query.is_empty() {
            return;
        }
        {
            let query = &mut self.search.query;
            while query.ends_with(char::is_whitespace) {
                query.pop();
            }
            while let Some(last) = query.chars().last() {
                if last.is_whitespace() {
                    break;
                }
                query.pop();
            }
        }
        self.rebuild_row_targets();
    }

    /// Does the group header itself match? When it does every row in the
    /// group stays visible, so typing a repo or tmux session name shows the
    /// whole group rather than only rows whose own fields contain it.
    /// `query_lower` is [`SearchState::lowercased_query`], computed once
    /// per pass.
    pub fn search_matches_group(&self, query_lower: &str, group: &crate::group::RepoGroup) -> bool {
        query_matches_lower(query_lower, &group.name)
    }

    /// Does an agent row match on its own fields? The caller ORs this with
    /// [`Self::search_matches_group`] so a group-name hit keeps every row.
    pub fn search_matches_pane(
        &self,
        query_lower: &str,
        pane: &crate::tmux::PaneInfo,
        git: &crate::group::PaneGitInfo,
    ) -> bool {
        query_matches_lower(query_lower, &pane.tmux_session)
            || query_matches_lower(query_lower, &pane.session_name)
            // A row with no session title renders the agent label as its
            // title, so typing what the user sees must still match.
            || query_matches_lower(query_lower, pane.agent.label())
            || query_matches_lower(query_lower, git.branch.as_deref().unwrap_or(""))
    }

    /// Does a non-agent window row match? Mirrors the pane rule and covers
    /// the fields the row actually renders, so `w` mode behaves
    /// consistently with the agent list.
    pub fn search_matches_window(
        &self,
        query_lower: &str,
        window: &crate::group::OtherWindow,
    ) -> bool {
        query_matches_lower(query_lower, &window.session_name)
            || query_matches_lower(query_lower, &window.window_name)
            || query_matches_lower(query_lower, &window.command)
            || query_matches_lower(query_lower, window.command_args.as_deref().unwrap_or(""))
            || query_matches_lower(query_lower, &window.pane_title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::{OtherWindow, PaneGitInfo, RepoGroup};
    use crate::tmux::{
        AgentType, PaneAttention, PaneInfo, PaneStatus, PermissionMode, WorktreeMetadata,
    };

    fn pane(id: &str, tmux_session: &str, session_name: &str) -> PaneInfo {
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
            session_name: session_name.into(),
            tmux_session: tmux_session.into(),
            window_index: 0,
            window_name: String::new(),
            sidebar_spawned: false,
            bg_shell_cmd: None,
            bg_shell_pid: None,
            pane_title: String::new(),
            flagged: false,
        }
    }

    fn git(branch: Option<&str>) -> PaneGitInfo {
        PaneGitInfo {
            branch: branch.map(str::to_string),
            ..Default::default()
        }
    }

    fn window(session: &str, name: &str, command: &str) -> OtherWindow {
        OtherWindow {
            window_id: "@5".into(),
            window_index: 0,
            window_name: name.into(),
            window_active: false,
            session_name: session.into(),
            session_attached: true,
            attention: PaneAttention::None,
            pane_id: "%10".into(),
            pane_active: true,
            pane_pid: None,
            command: command.into(),
            command_args: None,
            path: "/tmp/repo".into(),
            git_info: PaneGitInfo::default(),
            status: crate::tmux::classify_window_status(command),
            pane_title: String::new(),
            flagged: false,
        }
    }

    /// Three agent rows in one group, targets rebuilt.
    fn three_pane_state() -> AppState {
        let mut state = AppState::new("%0".into());
        state.repo_groups = vec![RepoGroup {
            name: "work".into(),
            has_focus: false,
            panes: vec![
                (pane("%1", "work", "claude-alpha"), git(Some("main"))),
                (pane("%2", "work", "codex-beta"), git(Some("main"))),
                (pane("%3", "work", "claude-gamma"), git(Some("dev"))),
            ],
        }];
        state.rebuild_row_targets();
        state
    }

    fn selected_pane_id(state: &AppState) -> &str {
        &state.layout.pane_row_targets[state.global.selected_pane_row].pane_id
    }

    #[test]
    fn esc_restores_the_cursor_to_where_the_user_stood_before_the_search() {
        // Audit 2026-11-21 F-5: the keystroke rebuilds strand the cursor
        // (row 0 when it drops the pane); Esc must return it to the
        // pre-search pane, re-anchored by identity.
        let mut state = three_pane_state();
        state.global.selected_pane_row = 2;

        state.open_search();
        state.search_push_char('c');
        state.search_push_char('o');
        state.search_push_char('d');
        state.search_push_char('e');
        state.search_push_char('x');
        // The filter leaves only %2; the cursor was stranded.
        assert_eq!(state.global.selected_pane_row, 0);

        state.cancel_search();
        assert_eq!(state.global.selected_pane_row, 2);
        assert_eq!(selected_pane_id(&state), "%3");
    }

    #[test]
    fn esc_falls_back_to_the_captured_row_when_the_pane_is_gone() {
        let mut state = three_pane_state();
        state.global.selected_pane_row = 2;
        state.open_search();
        state.search.query = "codex".into();

        // %3 vanishes while the filter is up; the restored index clamps
        // into the surviving list rather than stranding off the end.
        state.repo_groups[0].panes.pop();
        state.cancel_search();
        assert_eq!(state.global.selected_pane_row, 1);
    }

    #[test]
    fn reopening_a_refined_search_keeps_the_original_capture() {
        let mut state = three_pane_state();
        state.global.selected_pane_row = 2;
        state.open_search();
        state.search_push_char('c');
        state.search_push_char('o');
        state.search_push_char('d');
        state.search_push_char('e');
        state.search_push_char('x');
        state.finish_search();

        // Reopening `/` on an applied query refines it — the pre-search
        // position must not be re-captured from the search-moved cursor.
        state.open_search();
        state.cancel_search();
        assert_eq!(selected_pane_id(&state), "%3");
    }

    #[test]
    fn a_jump_after_a_finished_search_keeps_the_cursor_on_the_jumped_row() {
        // The jump target supersedes the pre-search position: restoring
        // here would drag the highlight off the pane the user just
        // activated.
        let mut state = three_pane_state();
        state.global.selected_pane_row = 2;
        state.open_search();
        for c in "codex".chars() {
            state.search_push_char(c);
        }
        state.finish_search();
        assert_eq!(selected_pane_id(&state), "%2");

        state.activate_selected_pane();
        assert_eq!(selected_pane_id(&state), "%2");
        assert!(!state.search.has_query());
    }

    #[test]
    fn plain_navigation_after_a_finished_search_supersedes_the_restore_point() {
        let mut state = three_pane_state();
        state.global.selected_pane_row = 0;
        state.open_search();
        state.finish_search();

        // A user-driven move outside the input line is a new choice; a
        // later Esc returns nothing (stale captures must not teleport).
        state.move_pane_selection(2);
        assert_eq!(state.global.selected_pane_row, 2);
        state.cancel_search();
        assert_eq!(state.global.selected_pane_row, 2);
    }

    #[test]
    fn empty_query_matches_everything() {
        let state = AppState::new("%0".into());
        assert!(state.search_matches_pane(
            &state.search.lowercased_query(),
            &pane("%1", "work", "claude-api"),
            &git(Some("main"))
        ));
        assert!(state.search_matches_window(
            &state.search.lowercased_query(),
            &window("work", "tests", "cargo")
        ));
        assert!(state.search_matches_group(
            &state.search.lowercased_query(),
            &RepoGroup {
                name: "repo".into(),
                has_focus: false,
                panes: vec![],
            }
        ));
    }

    #[test]
    fn query_is_case_insensitive() {
        let mut state = AppState::new("%0".into());
        state.search.query = "CLAUDE".into();
        assert!(state.search_matches_pane(
            &state.search.lowercased_query(),
            &pane("%1", "work", "claude-api"),
            &git(None)
        ));

        state.search.query = "work".into();
        assert!(state.search_matches_pane(
            &state.search.lowercased_query(),
            &pane("%2", "WORK", "x"),
            &git(None)
        ));

        state.search.query = "MAIN".into();
        assert!(state.search_matches_pane(
            &state.search.lowercased_query(),
            &pane("%3", "other", "codex"),
            &git(Some("main"))
        ));

        state.search.query = "missing".into();
        assert!(!state.search_matches_pane(
            &state.search.lowercased_query(),
            &pane("%4", "work", "claude"),
            &git(Some("main"))
        ));
    }

    #[test]
    fn query_matches_tmux_session_agent_title_and_branch() {
        let mut state = AppState::new("%0".into());

        state.search.query = "backend".into();
        assert!(state.search_matches_pane(
            &state.search.lowercased_query(),
            &pane("%1", "backend", "claude"),
            &git(Some("main"))
        ));

        state.search.query = "fix-auth".into();
        assert!(state.search_matches_pane(
            &state.search.lowercased_query(),
            &pane("%1", "backend", "fix-auth"),
            &git(Some("main"))
        ));

        state.search.query = "agent/".into();
        assert!(state.search_matches_pane(
            &state.search.lowercased_query(),
            &pane("%1", "backend", "claude"),
            &git(Some("agent/search"))
        ));

        state.search.query = "missing".into();
        assert!(!state.search_matches_pane(
            &state.search.lowercased_query(),
            &pane("%1", "backend", "claude"),
            &git(Some("main"))
        ));
    }

    #[test]
    fn group_name_hit_keeps_all_rows_visible() {
        let mut state = AppState::new("%0".into());
        state.search.query = "tzcalc".into();
        let group = RepoGroup {
            name: "⎇ tzcalc-m10".into(),
            has_focus: false,
            panes: vec![],
        };
        assert!(state.search_matches_group(&state.search.lowercased_query(), &group));
    }

    #[test]
    fn window_rows_match_rendered_fields() {
        let mut state = AppState::new("%0".into());

        state.search.query = "tests".into();
        assert!(state.search_matches_window(
            &state.search.lowercased_query(),
            &window("work", "tests", "cargo")
        ));

        state.search.query = "cargo".into();
        assert!(state.search_matches_window(
            &state.search.lowercased_query(),
            &window("work", "tests", "cargo")
        ));

        state.search.query = "work".into();
        assert!(state.search_matches_window(
            &state.search.lowercased_query(),
            &window("work", "tests", "cargo")
        ));

        state.search.query = "missing".into();
        assert!(!state.search_matches_window(
            &state.search.lowercased_query(),
            &window("work", "tests", "cargo")
        ));
    }
}
