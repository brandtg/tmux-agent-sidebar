use super::AppState;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StatusFilter {
    All,
    Running,
    Background,
    Waiting,
    Idle,
    Error,
}

impl StatusFilter {
    pub const VARIANTS: [StatusFilter; 6] = [
        StatusFilter::All,
        StatusFilter::Running,
        StatusFilter::Background,
        StatusFilter::Waiting,
        StatusFilter::Idle,
        StatusFilter::Error,
    ];

    pub fn next(self) -> Self {
        let idx = StatusFilter::VARIANTS
            .iter()
            .position(|v| *v == self)
            .unwrap_or(0);
        StatusFilter::VARIANTS[(idx + 1) % StatusFilter::VARIANTS.len()]
    }

    pub fn prev(self) -> Self {
        let idx = StatusFilter::VARIANTS
            .iter()
            .position(|v| *v == self)
            .unwrap_or(0);
        StatusFilter::VARIANTS
            [(idx + StatusFilter::VARIANTS.len() - 1) % StatusFilter::VARIANTS.len()]
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Running => "running",
            Self::Background => "background",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
            Self::Error => "error",
        }
    }

    /// Parse a tmux-option label into a `StatusFilter`. Unknown values
    /// fall back to `All`.
    pub fn from_label(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "background" => Self::Background,
            "waiting" => Self::Waiting,
            "idle" => Self::Idle,
            "error" => Self::Error,
            _ => Self::All,
        }
    }

    /// Does a pane pass this filter?
    ///
    /// The `Running` filter doubles as the "what's going on" view: besides
    /// actively running agents it also matches panes carrying any
    /// `@pane_attention` flag (a finished-but-unseen turn, a waiting /
    /// permission prompt), so one filter surfaces everything that wants
    /// the user's eye right now. The flag clears when the pane gains
    /// focus, at which point the pane drops back to its status-only
    /// filters (all + idle/waiting).
    pub fn matches(self, pane: &crate::tmux::PaneInfo) -> bool {
        match self {
            StatusFilter::All => true,
            StatusFilter::Running => {
                pane.status == crate::tmux::PaneStatus::Running
                    || pane.attention != crate::tmux::PaneAttention::None
            }
            StatusFilter::Background => pane.status == crate::tmux::PaneStatus::Background,
            StatusFilter::Waiting => pane.status == crate::tmux::PaneStatus::Waiting,
            StatusFilter::Idle => pane.status == crate::tmux::PaneStatus::Idle,
            StatusFilter::Error => pane.status == crate::tmux::PaneStatus::Error,
        }
    }

    /// Does a non-agent window pane pass this filter? Mirrors
    /// [`Self::matches`]:
    ///
    /// - a Task-classified foreground command counts as running, and a
    ///   finished-but-unseen task counts as attention, so both surface in
    ///   the `Running` ("what's going on") view;
    /// - a shell or an interactive program (editor, pager) counts as
    ///   idle — the "nothing ran here" case stays separated from tasks;
    /// - a finished task whose shell integration reported a non-zero
    ///   exit code surfaces in `Error` with a red diamond;
    /// - there is no non-agent analogue for the `Background` and
    ///   `Waiting` buckets, which is fine: those express agent-session
    ///   semantics.
    pub fn matches_window(
        self,
        window: &crate::group::OtherWindow,
        runtime: Option<&super::pane_runtime::PaneRuntimeState>,
    ) -> bool {
        let finished = runtime.and_then(|s| s.window_finished.as_ref());
        let task_running = runtime
            .map(|s| s.window_task_command.is_some())
            .unwrap_or(false);
        match self {
            StatusFilter::All => true,
            StatusFilter::Running => {
                task_running
                    || finished.is_some()
                    // Fallback while runtime tracking has not caught up yet
                    // (first tick after a sidebar restart): the pane's own
                    // Task classification still reads as "running".
                    || window.status == crate::tmux::WindowStatus::Task
            }
            StatusFilter::Background => false,
            StatusFilter::Waiting => false,
            StatusFilter::Idle => {
                !task_running
                    && finished.is_none()
                    && matches!(
                        window.status,
                        crate::tmux::WindowStatus::Idle | crate::tmux::WindowStatus::Busy
                    )
            }
            StatusFilter::Error => {
                finished.is_some_and(|f| f.exit_code.is_some_and(|code| code != 0))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RepoFilter {
    All,
    Repo(String),
}

impl RepoFilter {
    pub fn as_str(&self) -> &str {
        match self {
            Self::All => "all",
            Self::Repo(name) => name.as_str(),
        }
    }

    /// Parse a tmux-option label into a `RepoFilter`. `""` and `"all"`
    /// map to `All`; any other value is stored as `Repo(name)`.
    pub fn from_label(s: &str) -> Self {
        match s {
            "all" | "" => Self::All,
            name => Self::Repo(name.to_string()),
        }
    }

    pub fn matches_group(&self, group_name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Repo(name) => name == group_name,
        }
    }
}

impl AppState {
    /// Count agents per status across all repo groups.
    ///
    /// There is no dedicated filter button for `Unknown` panes, but the
    /// list renders them under the `All` filter — so they are counted
    /// into the `all` total. Excluding them made the header claim fewer
    /// agents than the list actually showed.
    ///
    /// `running` mirrors the `Running` filter's match rule (see
    /// [`StatusFilter::matches`]): attention-flagged panes show up there
    /// as well, so a done-but-unseen agent raises the running count too
    /// (and still counts into its own status bucket).
    pub fn status_counts(&self) -> (usize, usize, usize, usize, usize, usize) {
        let (mut running, mut background, mut waiting, mut idle, mut error) = (0, 0, 0, 0, 0);
        for group in &self.repo_groups {
            if !self.global.repo_filter.matches_group(&group.name) {
                continue;
            }
            for (pane, _) in &group.panes {
                if pane.status == crate::tmux::PaneStatus::Running
                    || pane.attention != crate::tmux::PaneAttention::None
                {
                    running += 1;
                }
                match pane.status {
                    crate::tmux::PaneStatus::Running => {}
                    crate::tmux::PaneStatus::Background => background += 1,
                    crate::tmux::PaneStatus::Waiting => waiting += 1,
                    crate::tmux::PaneStatus::Idle => idle += 1,
                    crate::tmux::PaneStatus::Error => error += 1,
                    crate::tmux::PaneStatus::Unknown => {}
                }
            }
        }
        // Non-agent window panes count into the same buckets when they
        // would render (show_windows on, repo filter matched) and match
        // the bucket's window rule. Note the counts are filter-view
        // independent, like the agent counts above.
        // Counted per pane, not as a bucket sum: an attention-flagged
        // pane lands in both `running` and its status bucket above.
        for group in &self.repo_groups {
            if !self.global.repo_filter.matches_group(&group.name) {
                continue;
            }
            if !self.global.show_windows {
                continue;
            }
            let key = crate::group::repo_group_key(group);
            let Some(windows) = self.other_windows.get(key.as_str()) else {
                continue;
            };
            for window in windows {
                let runtime = self.pane_state(&window.pane_id);
                if crate::state::StatusFilter::Running.matches_window(window, runtime) {
                    running += 1;
                }
                if crate::state::StatusFilter::Idle.matches_window(window, runtime) {
                    idle += 1;
                }
                if crate::state::StatusFilter::Error.matches_window(window, runtime) {
                    error += 1;
                }
            }
        }
        let all = self.pane_count() + self.window_pane_count();
        (all, running, background, waiting, idle, error)
    }

    /// Total non-agent window panes that would render, mirroring the
    /// renderer's gating (`show_windows` on and the repo filter matched).
    fn window_pane_count(&self) -> usize {
        if !self.global.show_windows {
            return 0;
        }
        self.repo_groups
            .iter()
            .filter(|g| self.global.repo_filter.matches_group(&g.name))
            .map(|g| {
                let key = crate::group::repo_group_key(g);
                let runtime = &self.pane_states;
                self.other_windows
                    .get(&key)
                    .map(|windows| {
                        windows
                            .iter()
                            .filter(|w| {
                                crate::state::StatusFilter::All
                                    .matches_window(w, runtime.get(&w.pane_id))
                            })
                            .count()
                    })
                    .unwrap_or(0)
            })
            .sum()
    }

    /// Total agent panes under the active repo filter.
    fn pane_count(&self) -> usize {
        self.repo_groups
            .iter()
            .filter(|g| self.global.repo_filter.matches_group(&g.name))
            .map(|g| g.panes.len())
            .sum()
    }

    /// Return list of repo names for the popup: ["All", repo1, repo2, ...]
    pub fn repo_names(&self) -> Vec<String> {
        let mut names = vec!["All".to_string()];
        for group in &self.repo_groups {
            names.push(group.name.clone());
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::PaneStatus;

    // ─── StatusFilter tests ───────────────────────────────────────────

    #[test]
    fn status_filter_next_cycles() {
        assert_eq!(StatusFilter::All.next(), StatusFilter::Running);
        assert_eq!(StatusFilter::Running.next(), StatusFilter::Background);
        assert_eq!(StatusFilter::Background.next(), StatusFilter::Waiting);
        assert_eq!(StatusFilter::Waiting.next(), StatusFilter::Idle);
        assert_eq!(StatusFilter::Idle.next(), StatusFilter::Error);
        assert_eq!(StatusFilter::Error.next(), StatusFilter::All);
    }

    #[test]
    fn status_filter_prev_cycles() {
        assert_eq!(StatusFilter::All.prev(), StatusFilter::Error);
        assert_eq!(StatusFilter::Error.prev(), StatusFilter::Idle);
        assert_eq!(StatusFilter::Idle.prev(), StatusFilter::Waiting);
        assert_eq!(StatusFilter::Waiting.prev(), StatusFilter::Background);
        assert_eq!(StatusFilter::Background.prev(), StatusFilter::Running);
        assert_eq!(StatusFilter::Running.prev(), StatusFilter::All);
    }

    #[test]
    fn status_filter_matches_panes() {
        let mut pane = test_pane("%1", PaneStatus::Running);
        pane.attention = PaneAttention::None;

        assert!(StatusFilter::All.matches(&pane));
        assert!(StatusFilter::Running.matches(&pane));

        pane.status = PaneStatus::Background;
        assert!(StatusFilter::Background.matches(&pane));
        assert!(!StatusFilter::Running.matches(&pane));

        pane.status = PaneStatus::Waiting;
        assert!(StatusFilter::Waiting.matches(&pane));

        pane.status = PaneStatus::Idle;
        assert!(StatusFilter::Idle.matches(&pane));

        pane.status = PaneStatus::Error;
        assert!(StatusFilter::Error.matches(&pane));
    }

    #[test]
    fn running_filter_matches_any_attention_flag() {
        // The Running filter is the "what's going on" view: an
        // attention flag on any status makes the pane show up there.
        for status in [
            PaneStatus::Idle,
            PaneStatus::Waiting,
            PaneStatus::Running,
            PaneStatus::Unknown,
        ] {
            for attention in [PaneAttention::Done, PaneAttention::Notification] {
                let mut pane = test_pane("%1", status.clone());
                pane.attention = attention;
                assert!(
                    StatusFilter::Running.matches(&pane),
                    "{status:?} with {attention:?} should match the Running filter"
                );
                // The pane keeps its status-only filter membership too.
                match status {
                    PaneStatus::Idle => assert!(StatusFilter::Idle.matches(&pane)),
                    PaneStatus::Waiting => assert!(StatusFilter::Waiting.matches(&pane)),
                    PaneStatus::Running => {}
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn running_filter_drops_pane_once_attention_clears() {
        // "Received attention" = the pane gains focus, which clears the
        // flag. The pane must then fall out of the Running filter and
        // back into its status-only filters.
        let mut pane = test_pane("%1", PaneStatus::Idle);
        pane.attention = PaneAttention::Done;
        assert!(StatusFilter::Running.matches(&pane));
        pane.attention = PaneAttention::None;
        assert!(!StatusFilter::Running.matches(&pane));
        assert!(StatusFilter::Idle.matches(&pane));
        assert!(StatusFilter::All.matches(&pane));
    }

    // ─── StatusFilter as_str / from_str tests ─────────────────────────

    #[test]
    fn status_filter_as_str_all_variants() {
        assert_eq!(StatusFilter::All.as_str(), "all");
        assert_eq!(StatusFilter::Running.as_str(), "running");
        assert_eq!(StatusFilter::Background.as_str(), "background");
        assert_eq!(StatusFilter::Waiting.as_str(), "waiting");
        assert_eq!(StatusFilter::Idle.as_str(), "idle");
        assert_eq!(StatusFilter::Error.as_str(), "error");
    }

    #[test]
    fn status_filter_from_str_all_variants() {
        assert_eq!(StatusFilter::from_label("all"), StatusFilter::All);
        assert_eq!(StatusFilter::from_label("running"), StatusFilter::Running);
        assert_eq!(
            StatusFilter::from_label("background"),
            StatusFilter::Background
        );
        assert_eq!(StatusFilter::from_label("waiting"), StatusFilter::Waiting);
        assert_eq!(StatusFilter::from_label("idle"), StatusFilter::Idle);
        assert_eq!(StatusFilter::from_label("error"), StatusFilter::Error);
    }

    #[test]
    fn status_filter_from_str_unknown_defaults_to_all() {
        assert_eq!(StatusFilter::from_label(""), StatusFilter::All);
        assert_eq!(StatusFilter::from_label("unknown"), StatusFilter::All);
        assert_eq!(StatusFilter::from_label("Running"), StatusFilter::All); // case-sensitive
    }

    #[test]
    fn status_filter_roundtrip() {
        for filter in StatusFilter::VARIANTS {
            assert_eq!(StatusFilter::from_label(filter.as_str()), filter);
        }
    }

    // ─── RepoFilter tests ─────────────────────────────────────

    #[test]
    fn repo_filter_persistence_roundtrip() {
        assert_eq!(RepoFilter::from_label("all"), RepoFilter::All);
        assert_eq!(RepoFilter::from_label(""), RepoFilter::All);
        assert_eq!(
            RepoFilter::from_label("my-app"),
            RepoFilter::Repo("my-app".into())
        );
        assert_eq!(RepoFilter::All.as_str(), "all");
        assert_eq!(RepoFilter::Repo("my-app".into()).as_str(), "my-app");
    }

    #[test]
    fn repo_filter_matches_group() {
        assert!(RepoFilter::All.matches_group("anything"));
        assert!(RepoFilter::Repo("app".into()).matches_group("app"));
        assert!(!RepoFilter::Repo("app".into()).matches_group("other"));
    }

    // ─── AppState status_counts / repo_names ─────────────────────────

    use crate::group::{PaneGitInfo, RepoGroup};
    use crate::tmux::{AgentType, PaneAttention, PaneInfo, PermissionMode, WorktreeMetadata};

    fn test_pane(id: &str, status: PaneStatus) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status,
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
            sidebar_spawned: false,
            bg_shell_cmd: None,
        }
    }

    #[test]
    fn status_counts_on_empty_state_is_all_zeroes() {
        let state = AppState::new("%99".into());
        assert_eq!(state.status_counts(), (0, 0, 0, 0, 0, 0));
    }

    #[test]
    fn status_counts_sums_across_repo_groups_and_filters() {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![
            RepoGroup {
                name: "app".into(),
                has_focus: true,
                panes: vec![
                    (test_pane("%1", PaneStatus::Running), PaneGitInfo::default()),
                    (test_pane("%2", PaneStatus::Idle), PaneGitInfo::default()),
                    (
                        test_pane("%4", PaneStatus::Background),
                        PaneGitInfo::default(),
                    ),
                ],
            },
            RepoGroup {
                name: "lib".into(),
                has_focus: false,
                panes: vec![(test_pane("%3", PaneStatus::Waiting), PaneGitInfo::default())],
            },
        ];

        // All repos: 4 total
        let (all, r, b, w, i, e) = state.status_counts();
        assert_eq!((all, r, b, w, i, e), (4, 1, 1, 1, 1, 0));

        // Restrict to "app"
        state.global.repo_filter = RepoFilter::Repo("app".into());
        let (all, r, b, w, i, e) = state.status_counts();
        assert_eq!((all, r, b, w, i, e), (3, 1, 1, 0, 1, 0));
    }

    #[test]
    fn status_counts_includes_unknown_in_all() {
        // Unknown panes have no filter button of their own but DO render
        // under the All filter — the header's `all` count must include
        // them or the bar undercounts the visible rows.
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![RepoGroup {
            name: "app".into(),
            has_focus: true,
            panes: vec![
                (test_pane("%1", PaneStatus::Running), PaneGitInfo::default()),
                (test_pane("%2", PaneStatus::Unknown), PaneGitInfo::default()),
            ],
        }];

        let (all, r, b, w, i, e) = state.status_counts();
        assert_eq!((all, r, b, w, i, e), (2, 1, 0, 0, 0, 0));
    }

    #[test]
    fn status_counts_running_includes_attention_panes() {
        // Attention-flagged panes raise the running count (the Running
        // filter shows them) while still landing in their status bucket;
        // `all` counts each pane exactly once.
        let mut state = AppState::new("%99".into());
        let mut done = test_pane("%1", PaneStatus::Idle);
        done.attention = PaneAttention::Done;
        let mut waiting = test_pane("%2", PaneStatus::Waiting);
        waiting.attention = PaneAttention::Notification;
        state.repo_groups = vec![RepoGroup {
            name: "app".into(),
            has_focus: true,
            panes: vec![
                (done, PaneGitInfo::default()),
                (waiting, PaneGitInfo::default()),
                (test_pane("%3", PaneStatus::Running), PaneGitInfo::default()),
            ],
        }];

        let (all, r, b, w, i, e) = state.status_counts();
        assert_eq!((all, r, b, w, i, e), (3, 3, 0, 1, 1, 0));
    }

    #[test]
    fn repo_names_leads_with_all_sentinel() {
        let mut state = AppState::new("%99".into());
        assert_eq!(state.repo_names(), vec!["All"]);
        state.repo_groups = vec![
            RepoGroup {
                name: "alpha".into(),
                has_focus: true,
                panes: vec![],
            },
            RepoGroup {
                name: "beta".into(),
                has_focus: false,
                panes: vec![],
            },
        ];
        assert_eq!(state.repo_names(), vec!["All", "alpha", "beta"]);
    }
}
