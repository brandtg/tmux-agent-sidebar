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
fn query_matches_lower(query_lower: &str, haystack: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    haystack.to_lowercase().contains(query_lower)
}

impl AppState {
    /// Open the `/` input line. The current query (if any) is preserved so
    /// reopening lets the user refine rather than retype.
    pub fn open_search(&mut self) {
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
    /// restoring the unfiltered list.
    pub fn cancel_search(&mut self) {
        self.search.active = false;
        let had_query = !self.search.query.is_empty();
        self.search.query.clear();
        if had_query {
            self.rebuild_row_targets();
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
