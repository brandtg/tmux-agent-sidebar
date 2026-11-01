use ratatui::{
    style::Style,
    text::{Line, Span},
};

use super::SPAWN_BUTTON;
use super::row;
use crate::state::{AppState, Focus};
use crate::ui::text::{display_width, truncate_to_width};

#[derive(Debug, Default)]
pub(super) struct CollectedRows {
    pub lines: Vec<Line<'static>>,
    pub line_to_row: Vec<Option<usize>>,
    pub pending_spawn: Vec<(usize, String, String)>,
    pub pending_remove: Vec<(usize, u16, String)>,
}

pub(super) fn collect(state: &AppState, width: u16) -> CollectedRows {
    let width = width as usize;
    let theme = &state.theme;

    let mut collected = CollectedRows::default();
    let filter = state.global.status_filter;
    let mut first_group = true;
    let mut row_index: usize = 0;

    for group in &state.repo_groups {
        if !state.global.repo_filter.matches_group(&group.name) {
            continue;
        }
        // Same rule as `rebuild_row_targets`: a group-name hit keeps the
        // whole group; otherwise each row must match on its own fields.
        let group_matches_search = state.search_matches_group(group);
        let filtered_panes: Vec<_> = group
            .panes
            .iter()
            .filter(|(pane, git)| {
                filter.matches(pane)
                    && (group_matches_search || state.search_matches_pane(pane, git))
            })
            .collect();

        // A group with no matching agent panes is skipped entirely —
        // unless one of its non-agent window panes still matches the
        // filter (a finished task under `running`, a failed run under
        // `error`, an idle shell under `idle`), in which case the group
        // header and window section render without agent rows.
        let matched_windows: Vec<&crate::group::OtherWindow> = if state.global.show_windows {
            let key = crate::group::group_window_key(state.global.group_by, group);
            state
                .other_windows
                .get(&key)
                .map(|windows| {
                    windows
                        .iter()
                        .filter(|window| {
                            let runtime = state.pane_state(&window.pane_id);
                            filter.matches_window(window, runtime)
                                && (group_matches_search || state.search_matches_window(window))
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if filtered_panes.is_empty() && matched_windows.is_empty() {
            continue;
        }
        let collapsed = state.is_group_collapsed(&group.name);

        if !first_group {
            // Separate repo groups, but do not add a leading blank before
            // the first repo so the list starts immediately below the header.
            collected.lines.push(Line::from(""));
            collected.line_to_row.push(None);
        }
        first_group = false;

        // A focused agent pane lights the header. In session mode a
        // focused process pane does too: the user is still in that
        // session, but `group.panes` only holds agent panes, so a
        // pane-id membership check against it alone would drop the
        // accent the moment they sat in a plain shell. Session-mode
        // process panes bucket under the tmux session name (see
        // `group_window_key`), and pane ids are unique tmux-wide, so
        // only the owning session's bucket can match. Repo mode keeps
        // the agent-only rule: its bucket key is a git-resolved repo
        // root, not a session the user sits in.
        let group_has_focused_pane =
            state
                .focus_state
                .focused_pane_id
                .as_ref()
                .is_some_and(|fid| {
                    group.panes.iter().any(|(p, _)| p.pane_id == *fid)
                        || (state.global.group_by == crate::state::GroupBy::Session
                            && state
                                .other_windows
                                .get(&group.name)
                                .is_some_and(|windows| windows.iter().any(|w| &w.pane_id == fid)))
                });

        // Plain group header at column 0. In repo mode a `[+]` spawn
        // button rides right-aligned on the same row, rendered only when
        // the group has a resolved repo_root — panes outside a git repo
        // get a plain title. Session groups are not a spawn target (a
        // session is not a repo to spawn into), so they always render a
        // plain title.
        // A collapsed group keeps its header (so the user can see what is
        // hidden and expand it) but renders no rows. The `▸` prefix is the
        // only visual cue; expanded headers stay unadorned.
        let title = if collapsed {
            format!("▸ {}", group.name)
        } else {
            group.name.clone()
        };
        let title_color = if group_has_focused_pane {
            theme.accent
        } else {
            theme.text_active
        };
        let repo_root = match state.global.group_by {
            crate::state::GroupBy::Repo => group
                .panes
                .iter()
                .find_map(|(_, git)| git.repo_root.clone()),
            crate::state::GroupBy::Session => None,
        };
        let spans: Vec<Span<'static>> = if let Some(ref root) = repo_root {
            // The `+` button must stay visible: the title is truncated to
            // the space left of the button. An untruncated long repo name
            // used to push the button past the panel edge — the row then
            // rendered no `+` at all while the click target (anchored at
            // the right edge by `click_targets::materialize`) stayed live.
            let button_width = SPAWN_BUTTON.len();
            let title_budget = width.saturating_sub(button_width);
            let title = truncate_to_width(&title, title_budget);
            let title_w = display_width(&title);
            let pad_width = width.saturating_sub(title_w).saturating_sub(button_width);
            collected
                .pending_spawn
                .push((collected.lines.len(), group.name.clone(), root.clone()));
            let button_color = if group_has_focused_pane {
                theme.accent
            } else {
                theme.text_active
            };
            vec![
                Span::styled(title, Style::default().fg(title_color)),
                Span::raw(" ".repeat(pad_width)),
                Span::styled(SPAWN_BUTTON, Style::default().fg(button_color)),
            ]
        } else {
            vec![Span::styled(
                title.clone(),
                Style::default().fg(title_color),
            )]
        };
        collected.lines.push(Line::from(spans));
        collected.line_to_row.push(None);

        if collapsed {
            continue;
        }

        // Agents and non-agent rows interleave in session → window →
        // pane order (see `group::ordered_group_rows`), so each window's
        // processes sit next to its agent instead of pooling below the
        // whole group. Window rows use a diamond status glyph
        // (see `row::window_row`), so they read as distinct without a
        // header line.
        for row in crate::group::ordered_group_rows(&filtered_panes, &matched_windows) {
            match row {
                crate::group::GroupRow::Agent((pane, git_info)) => {
                    let is_selected = state.focus_state.sidebar_focused
                        && state.focus_state.focus == Focus::Panes
                        && row_index == state.global.selected_pane_row;

                    let is_active =
                        state.focus_state.focused_pane_id.as_ref() == Some(&pane.pane_id);

                    let pane_state = state.pane_state(&pane.pane_id);
                    let ports = pane_state.map(|s| s.ports.as_slice());
                    let task_progress = pane_state.and_then(|s| s.task_progress.as_ref());
                    let status_line_idx = collected.lines.len();
                    let compact = state.global.compact;
                    let pane_lines = if compact {
                        row::compact_row(
                            pane,
                            git_info,
                            is_selected,
                            is_active,
                            pane.flagged,
                            width,
                            &state.icons,
                            theme,
                            state.spinner_frame,
                            state.now,
                        )
                    } else {
                        row::render_pane_lines_with_ports(
                            pane,
                            git_info,
                            ports,
                            task_progress,
                            is_selected,
                            is_active,
                            pane.flagged,
                            width,
                            &state.icons,
                            theme,
                            state.spinner_frame,
                            state.now,
                            state.global.preview_lines,
                        )
                    };
                    let pane_line_count = pane_lines.len();
                    collected.lines.extend(pane_lines);
                    for _ in 0..pane_line_count {
                        collected.line_to_row.push(Some(row_index));
                    }

                    // Register the trailing `×` remove marker when the
                    // pane is a sidebar-spawned worktree. In full mode
                    // the marker rides on the branch row
                    // (`status_line_idx + 1`, present only when the
                    // pane rendered >= 2 lines); in compact mode it is
                    // pinned to the status row itself.
                    let remove_eligible = if compact {
                        true
                    } else {
                        git_info.is_worktree && pane_line_count >= 2
                    };
                    if pane.sidebar_spawned
                        && remove_eligible
                        && let Some(x) = row::sidebar_remove_marker_col(
                            git_info,
                            ports,
                            true,
                            width.saturating_sub(2),
                        )
                    {
                        let line_idx = if compact {
                            status_line_idx
                        } else {
                            status_line_idx + 1
                        };
                        collected
                            .pending_remove
                            .push((line_idx, x, pane.pane_id.clone()));
                    }

                    row_index += 1;
                }
                crate::group::GroupRow::Window(window) => {
                    let runtime = state.pane_state(&window.pane_id);
                    let is_selected = state.focus_state.sidebar_focused
                        && state.focus_state.focus == Focus::Panes
                        && row_index == state.global.selected_pane_row;
                    let is_active =
                        window.session_attached && window.window_active && window.pane_active;
                    let finished = runtime.and_then(|s| s.window_finished.clone());
                    // Window rows carry the same density flag as agent
                    // rows: expanded renders the two-line card whose
                    // second line names the *other* grouping dimension
                    // plus the window name (see `row::window_row`), so
                    // `group_by` rides along to pick which dimension is
                    // "other". Every returned line maps to this row so
                    // the whole card is clickable.
                    let lines = row::window_row(
                        window,
                        runtime.map(|s| s.ports.as_slice()),
                        finished.as_ref(),
                        is_selected,
                        is_active,
                        window.flagged,
                        !state.global.compact,
                        state.global.group_by,
                        width,
                        &state.icons,
                        theme,
                        state.spinner_frame,
                    );
                    let line_count = lines.len();
                    collected.lines.extend(lines);
                    for _ in 0..line_count {
                        collected.line_to_row.push(Some(row_index));
                    }
                    row_index += 1;
                }
            }
        }
    }

    // An applied search that matches nothing leaves an empty panel; say so
    // instead of rendering a blank box. This line is not a selectable row,
    // so it maps to `None` in `line_to_row`.
    if collected.lines.is_empty() && state.search.has_query() {
        collected.lines.push(Line::from(Span::styled(
            format!("  no matches for {}", state.search.query.trim()),
            Style::default().fg(theme.text_muted),
        )));
        collected.line_to_row.push(None);
    }

    collected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::{PaneGitInfo, RepoGroup};
    use crate::state::{AppState, GroupBy, StatusFilter};
    use crate::tmux::{
        AgentType, PaneAttention, PaneInfo, PaneStatus, PermissionMode, WorktreeMetadata,
    };

    fn make_pane(id: &str, status: PaneStatus) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status,
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

    fn make_window_pane(
        id: &str,
        status: PaneStatus,
        session: &str,
        window_index: i64,
    ) -> PaneInfo {
        let mut pane = make_pane(id, status);
        pane.tmux_session = session.into();
        pane.window_index = window_index;
        pane
    }

    fn other_window_in(
        pane_id: &str,
        name: &str,
        command: &str,
        session: &str,
        window_index: i64,
    ) -> crate::group::OtherWindow {
        let mut w = other_window(pane_id, name, command);
        w.session_name = session.into();
        w.window_index = window_index;
        w
    }

    #[test]
    fn collect_empty_repo_groups_produces_no_lines() {
        let state = AppState::new("%0".into());
        let collected = collect(&state, 40);
        assert!(collected.lines.is_empty());
        assert!(collected.line_to_row.is_empty());
        assert!(collected.pending_spawn.is_empty());
        assert!(collected.pending_remove.is_empty());
    }

    fn other_window(pane_id: &str, name: &str, command: &str) -> crate::group::OtherWindow {
        crate::group::OtherWindow {
            window_id: "@5".into(),
            window_index: 0,
            window_name: name.into(),
            window_active: false,
            session_name: "main".into(),
            session_attached: true,
            attention: crate::tmux::PaneAttention::None,
            pane_id: pane_id.into(),
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

    fn line_text(line: &ratatui::text::Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn collect_renders_window_section_when_enabled() {
        let mut state = AppState::new("%0".into());
        state.global.show_windows = true;
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            has_focus: false,
            panes: vec![(make_pane("%1", PaneStatus::Idle), PaneGitInfo::default())],
        }];
        state.other_windows.insert(
            "/tmp/repo".into(),
            vec![other_window("%10", "cargo test", "cargo")],
        );

        let collected = collect(&state, 40);
        let text = collected
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("cargo"), "window row missing:\n{text}");
        assert!(text.contains('\u{25C6}'), "window glyph missing:\n{text}");
        // Layout: repo header, agent status, agent idle hint, then the
        // two-line window card (both lines map to row index 1). No
        // sub-header line.
        assert_eq!(
            collected.line_to_row,
            vec![None, Some(0), Some(0), Some(1), Some(1)],
            "the window card must be selectable as row 1 on both lines"
        );
    }

    #[test]
    fn collect_window_rows_stay_single_line_in_compact_mode() {
        // Density contract: compact mode is strictly one line per row, so
        // the window arm's `expanded = !compact` flag must keep the line
        // map at exactly one entry per row.
        let mut state = AppState::new("%0".into());
        state.global.show_windows = true;
        state.global.compact = true;
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            has_focus: false,
            panes: vec![(make_pane("%1", PaneStatus::Idle), PaneGitInfo::default())],
        }];
        state.other_windows.insert(
            "/tmp/repo".into(),
            vec![other_window("%10", "cargo test", "cargo")],
        );

        let collected = collect(&state, 40);
        // Header, one compact agent line, one window line.
        assert_eq!(
            collected.line_to_row,
            vec![None, Some(0), Some(1)],
            "compact density keeps every row to a single line"
        );
    }

    #[test]
    fn collect_interleaves_window_rows_after_their_window_agent() {
        // w mode: an agent plus two server panes in window 0, an agent
        // in window 1 with its own server. Rows must render in session
        // → window → pane order (agent, server1, server2, agent2,
        // server3) — not all agents first, all windows after.
        let mut state = AppState::new("%0".into());
        state.global.show_windows = true;
        let mut agent1 = make_window_pane("%1", PaneStatus::Running, "main", 0);
        agent1.prompt = "fix auth".into();
        let mut agent2 = make_window_pane("%2", PaneStatus::Running, "main", 1);
        agent2.prompt = "fix ui".into();
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            has_focus: false,
            panes: vec![
                (agent1, PaneGitInfo::default()),
                (agent2, PaneGitInfo::default()),
            ],
        }];
        state.other_windows.insert(
            "/tmp/repo".into(),
            vec![
                other_window_in("%10", "backend", "node", "main", 0),
                other_window_in("%11", "frontend", "npm", "main", 0),
                other_window_in("%12", "tests", "cargo", "main", 1),
            ],
        );

        let collected = collect(&state, 40);
        let text = collected
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        let pos = |needle: &str| {
            text.lines()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} missing:\n{text}"))
        };
        assert!(
            pos("fix auth") < pos("node")
                && pos("node") < pos("npm")
                && pos("npm") < pos("fix ui")
                && pos("fix ui") < pos("cargo"),
            "rows must interleave by window:\n{text}"
        );

        // Selection targets follow the same order: window-0 card (both
        // lines) sits between the two agents, window-1's card after
        // agent2. Expanded window cards occupy two lines each.
        assert_eq!(
            collected.line_to_row,
            vec![
                None,
                Some(0),
                Some(0),
                Some(1),
                Some(1),
                Some(2),
                Some(2),
                Some(3),
                Some(3),
                Some(4),
                Some(4)
            ],
            "line_to_row must match the interleaved render order"
        );
    }

    #[test]
    fn collect_collapsed_group_renders_header_only() {
        let mut state = AppState::new("%0".into());
        state.repo_groups = vec![
            RepoGroup {
                name: "backend".into(),
                has_focus: false,
                panes: vec![(make_pane("%1", PaneStatus::Idle), PaneGitInfo::default())],
            },
            RepoGroup {
                name: "frontend".into(),
                has_focus: false,
                panes: vec![(make_pane("%2", PaneStatus::Idle), PaneGitInfo::default())],
            },
        ];
        state.collapsed_groups.insert("backend".into());
        state.rebuild_row_targets();

        let collected = collect(&state, 40);
        let text = collected
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("▸ backend"),
            "collapsed header missing:\n{text}"
        );
        assert!(text.contains("frontend"), "expanded group missing:\n{text}");
        assert_eq!(
            collected.line_to_row,
            vec![None, None, None, Some(0), Some(0)],
            "only the expanded group's agent row is selectable"
        );
    }

    #[test]
    fn collect_hides_window_section_when_disabled() {
        let mut state = AppState::new("%0".into());
        state.global.show_windows = false;
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            has_focus: false,
            panes: vec![(make_pane("%1", PaneStatus::Idle), PaneGitInfo::default())],
        }];
        state.other_windows.insert(
            "/tmp/repo".into(),
            vec![other_window("%10", "cargo test", "cargo")],
        );

        let collected = collect(&state, 40);
        let text = collected
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !text.contains("cargo test") && !text.contains('\u{25C6}'),
            "window section must be hidden:\n{text}"
        );
    }

    #[test]
    fn collect_session_mode_renders_agentless_session_windows() {
        // The w-mode session-manager view: a session without agents still
        // renders its header and its plain windows, and is not a spawn
        // target (a session is not a repo to spawn into).
        let mut state = AppState::new("%0".into());
        state.global.group_by = GroupBy::Session;
        state.global.show_windows = true;
        state.repo_groups = vec![RepoGroup {
            name: "solo".into(),
            has_focus: false,
            panes: vec![],
        }];
        state.other_windows.insert(
            "solo".into(),
            vec![other_window("%10", "cargo test", "cargo")],
        );

        let collected = collect(&state, 40);
        let text = collected
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("solo"), "session header missing:\n{text}");
        assert!(text.contains("cargo"), "window row missing:\n{text}");
        assert!(
            collected.pending_spawn.is_empty(),
            "session headers must not register spawn targets"
        );
    }

    #[test]
    fn collect_session_mode_header_stays_accent_for_focused_process_pane() {
        // The user sits in a plain shell (process pane) of session
        // "main": the header must keep its accent even though the
        // focused pane is not an agent row — and only that session's
        // header lights up, since a process pane belongs to exactly one
        // session. Works with `w` off: the bucket lookup does not go
        // through the rendered window rows.
        let mut state = AppState::new("%0".into());
        state.global.group_by = GroupBy::Session;
        state.repo_groups = vec![
            RepoGroup {
                name: "main".into(),
                has_focus: false,
                panes: vec![(make_pane("%1", PaneStatus::Idle), PaneGitInfo::default())],
            },
            RepoGroup {
                name: "other".into(),
                has_focus: false,
                panes: vec![(make_pane("%2", PaneStatus::Idle), PaneGitInfo::default())],
            },
        ];
        state.other_windows.insert(
            "main".into(),
            vec![other_window("%10", "dev server", "node")],
        );
        state.focus_state.focused_pane_id = Some("%10".into());

        let collected = collect(&state, 40);
        let title_fg = |name: &str| {
            collected
                .lines
                .iter()
                .find(|l| line_text(l) == name)
                .expect("title line")
                .spans
                .first()
                .expect("title span")
                .style
                .fg
        };
        assert_eq!(title_fg("main"), Some(state.theme.accent));
        assert_eq!(title_fg("other"), Some(state.theme.text_active));
    }

    #[test]
    fn collect_session_mode_hides_agentless_sessions_when_windows_off() {
        let mut state = AppState::new("%0".into());
        state.global.group_by = GroupBy::Session;
        state.repo_groups = vec![RepoGroup {
            name: "solo".into(),
            has_focus: false,
            panes: vec![],
        }];
        state.other_windows.insert(
            "solo".into(),
            vec![other_window("%10", "cargo test", "cargo")],
        );

        let collected = collect(&state, 40);
        assert!(
            collected.lines.is_empty(),
            "w off keeps agent-less sessions out of the list"
        );
    }

    #[test]
    fn collect_skips_group_when_status_filter_excludes_all_panes() {
        let mut state = AppState::new("%0".into());
        // The group has only Running panes, so filter to Waiting to drop them all.
        state.global.status_filter = StatusFilter::Waiting;
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            has_focus: false,
            panes: vec![(make_pane("%1", PaneStatus::Running), PaneGitInfo::default())],
        }];
        let collected = collect(&state, 40);
        assert!(collected.lines.is_empty());
        assert!(collected.pending_spawn.is_empty());
    }

    #[test]
    fn collect_records_pending_spawn_when_repo_root_present() {
        let mut state = AppState::new("%0".into());
        let git_info = PaneGitInfo {
            repo_root: Some("/tmp/repo".into()),
            branch: None,
            is_worktree: false,
            ..Default::default()
        };
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            has_focus: false,
            panes: vec![(make_pane("%1", PaneStatus::Running), git_info)],
        }];
        let collected = collect(&state, 40);
        assert_eq!(
            collected.pending_spawn.len(),
            1,
            "groups with a repo_root should emit a spawn target"
        );
        assert_eq!(collected.pending_spawn[0].1, "repo");
        assert_eq!(collected.pending_spawn[0].2, "/tmp/repo");
        // At least the header plus one pane row should have been pushed.
        assert!(!collected.lines.is_empty());
    }

    #[test]
    fn collect_pending_spawn_for_worktree_group_targets_main_repo_root() {
        // Deliberate contract (see Part B of the two-line-process-view
        // plan): the `+` button on a worktree group must spawn a NEW
        // worktree off the MAIN checkout, so it registers the main
        // `repo_root`, never the group's `worktree_root` (the key the
        // group itself is bucketed under). Pin this so nobody later
        // "fixes" the collector to key off the worktree path.
        let mut state = AppState::new("%0".into());
        let git_info = PaneGitInfo {
            repo_root: Some("/tmp/repo".into()),
            branch: None,
            is_worktree: true,
            worktree_root: Some("/tmp/repo-wt/tzcalc-m10".into()),
            ..Default::default()
        };
        state.repo_groups = vec![RepoGroup {
            name: "⎇ tzcalc-m10".into(),
            has_focus: false,
            panes: vec![(make_pane("%1", PaneStatus::Running), git_info)],
        }];
        let collected = collect(&state, 40);
        assert_eq!(
            collected.pending_spawn.len(),
            1,
            "worktree groups must still emit a spawn target"
        );
        assert_eq!(collected.pending_spawn[0].1, "⎇ tzcalc-m10");
        assert_eq!(
            collected.pending_spawn[0].2, "/tmp/repo",
            "the spawn target must be the main repo_root, not the worktree path"
        );
    }

    #[test]
    fn collect_no_pending_spawn_without_repo_root() {
        let mut state = AppState::new("%0".into());
        state.repo_groups = vec![RepoGroup {
            name: "raw-path".into(),
            has_focus: false,
            panes: vec![(make_pane("%1", PaneStatus::Running), PaneGitInfo::default())],
        }];
        let collected = collect(&state, 40);
        assert!(
            collected.pending_spawn.is_empty(),
            "groups without repo_root must not produce spawn targets"
        );
    }

    #[test]
    fn collect_pending_spawn_grows_with_repo_root_bearing_groups() {
        let mut state = AppState::new("%0".into());
        let with_root = |root: &str, name: &str, pane_id: &str| RepoGroup {
            name: name.into(),
            has_focus: false,
            panes: vec![(
                make_pane(pane_id, PaneStatus::Running),
                PaneGitInfo {
                    repo_root: Some(root.into()),
                    branch: None,
                    is_worktree: false,
                    ..Default::default()
                },
            )],
        };
        state.repo_groups = vec![
            with_root("/repo/a", "a", "%1"),
            with_root("/repo/b", "b", "%2"),
            with_root("/repo/c", "c", "%3"),
        ];
        let collected = collect(&state, 40);
        assert_eq!(collected.pending_spawn.len(), 3);
    }
}
