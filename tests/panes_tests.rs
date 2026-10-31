#[allow(dead_code, unused_imports)]
mod test_helpers;

use test_helpers::*;
use tmux_agent_sidebar::group::{PaneGitInfo, RepoGroup};
use tmux_agent_sidebar::state::{AppState, Focus, StatusFilter};
use tmux_agent_sidebar::tmux::{AgentType, PaneInfo, PaneStatus, SessionInfo, WindowInfo};
use tmux_agent_sidebar::ui::colors::ColorTheme;
use tmux_agent_sidebar::ui::icons::StatusIcons;

// ─── Agents: auto-scroll behavior Tests ─────────────────────────────

#[test]
fn test_agents_auto_scroll_keeps_selected_visible() {
    // Create enough agents to overflow a small viewport
    let mut panes = Vec::new();
    for i in 0..10 {
        let mut pane = make_pane(AgentType::Claude, PaneStatus::Idle);
        pane.pane_id = format!("%{}", i);
        panes.push(pane);
    }

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: panes.clone(),
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", panes)];
    state.focus_state.sidebar_focused = true;
    state.focus_state.focus = Focus::Panes;
    state.rebuild_row_targets();

    // Render with a small height. With the 2-row header, the first pane
    // still stays visible without needing to scroll.
    let _ = render_to_string(&mut state, 28, 5);
    assert_eq!(state.scrolls.panes.offset, 0, "initially at top");

    // Select last agent and re-render
    state.global.selected_pane_row = 9;
    let _ = render_to_string(&mut state, 28, 5);
    assert!(
        state.scrolls.panes.offset > 0,
        "should scroll down to show selected agent"
    );
}

#[test]
fn test_panes_scroll_offset_tracks_total_and_visible() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Idle);
    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", vec![pane])];
    state.rebuild_row_targets();

    let _ = render_to_string(&mut state, 28, 26);
    // After rendering, panes_scroll.total_lines and panes_scroll.visible_height should be set
    assert!(
        state.scrolls.panes.total_lines > 0,
        "total lines should be populated"
    );
    assert!(
        state.scrolls.panes.visible_height > 0,
        "visible height should be populated"
    );
}

// ─── Agents: Codex agent color ──────────────────────────────────────

#[test]
fn snapshot_codex_agent_styled() {
    let theme = ColorTheme::default();
    assert_eq!(
        theme.agent_color(&AgentType::Codex),
        ratatui::style::Color::Indexed(141)
    );
}

// ─── Agents: Unknown agent type ─────────────────────────────────────

#[test]
fn snapshot_unknown_agent_styled() {
    let theme = ColorTheme::default();
    assert_eq!(
        theme.agent_color(&AgentType::Unknown),
        ratatui::style::Color::Indexed(244)
    );
}

// ─── Agents: running icon variants via render ───────────────────────

#[test]
fn test_running_icon_blink_off() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Running);
    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", vec![pane])];
    state.rebuild_row_targets();
    state.focus_state.sidebar_focused = false;
    state.spinner_frame = 0;

    insta::assert_snapshot!(render_to_string(&mut state, 28, 25), @"
     ≡1  ●1  ◎0  ◐0  ○0  ✕0
    ⓘ                        — ▾
    project
    ┃ ● claude
    ");
}

#[test]
fn test_running_spinner_frame_advances() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Running);
    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", vec![pane])];
    state.rebuild_row_targets();
    state.focus_state.sidebar_focused = false;
    state.spinner_frame = 3;

    insta::assert_snapshot!(render_to_string(&mut state, 28, 25), @"
     ≡1  ●1  ◎0  ◐0  ○0  ✕0
    ⓘ                        — ▾
    project
    ┃ ● claude
    ");
}

#[test]
fn test_waiting_icon() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Waiting);
    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", vec![pane])];
    state.rebuild_row_targets();
    state.focus_state.sidebar_focused = false;

    insta::assert_snapshot!(render_to_string(&mut state, 28, 25), @"
     ≡1  ●0  ◎0  ◐1  ○0  ✕0
    ⓘ                        — ▾
    project
    ┃ ◐ claude
    ");
}

#[test]
fn test_error_icon() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Error);
    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", vec![pane])];
    state.rebuild_row_targets();
    state.focus_state.sidebar_focused = false;

    insta::assert_snapshot!(render_to_string(&mut state, 28, 25), @"
     ≡1  ●0  ◎0  ◐0  ○0  ✕1
    ⓘ                        — ▾
    project
    ┃ ✕ claude
    ");
}

#[test]
fn test_unknown_status_icon() {
    let icons = StatusIcons::default();
    assert_eq!(icons.status_icon(&PaneStatus::Unknown), "·");
}

// ─── Agents: auto-scroll keeps selected pane visible ───────────────

#[test]
fn test_agents_auto_scroll_shows_last_selected_pane() {
    // When the last agent in a group is selected, the auto-scroll
    // should bring it into view (the selection marker must be visible).
    let mut panes = Vec::new();
    for i in 0..6 {
        let mut pane = make_pane(AgentType::Claude, PaneStatus::Idle);
        pane.pane_id = format!("%{}", i);
        panes.push(pane);
    }

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: panes.clone(),
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", panes)];
    state.focus_state.sidebar_focused = true;
    state.focus_state.focus = Focus::Panes;
    state.rebuild_row_targets();

    // Select the last agent
    state.global.selected_pane_row = 5;
    // Use a tight height so the agent list area is small.
    let _ = render_to_string(&mut state, 28, 5);

    // Auto-scroll should have moved forward to keep the last-selected pane visible.
    assert!(
        state.scrolls.panes.offset > 0,
        "selecting the last agent should scroll the list"
    );
}

#[test]
fn test_agents_auto_scroll_up_shows_group_header() {
    // After scrolling down, selecting the first agent should scroll
    // back up enough to show the group header.
    let mut panes = Vec::new();
    for i in 0..8 {
        let mut pane = make_pane(AgentType::Claude, PaneStatus::Idle);
        pane.pane_id = format!("%{}", i);
        panes.push(pane);
    }

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: panes.clone(),
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", panes)];
    state.focus_state.sidebar_focused = true;
    state.focus_state.focus = Focus::Panes;
    state.rebuild_row_targets();

    // Scroll to bottom
    state.global.selected_pane_row = 7;
    let _ = render_to_string(&mut state, 28, 5);
    assert!(state.scrolls.panes.offset > 0, "should have scrolled down");

    // Now select first agent and re-render
    state.global.selected_pane_row = 0;
    // The snapshot locks in that the `project` repo header is visible after
    // scrolling back up to the first agent.
    insta::assert_snapshot!(render_to_string(&mut state, 28, 5), @r"
     ≡8  ●0  ◎0  ◐0  ○8  ✕0
    ⓘ                        — ▾
    project
      ○ claude
        Waiting for prompt…
    ");
}

// ─── Agents: window/pane switch resets scroll ───────────────────────

#[test]
fn test_window_switch_reset_returns_list_to_top() {
    // A tmux window/pane switch reloads the shared `@sidebar_cursor`, which
    // another window may have left on a low row; `app::run` then calls
    // `reset_pane_scroll`. Without it the auto-scroll snaps this list down
    // to the adopted row and hides the agents above it.
    let mut panes = Vec::new();
    for i in 0..8 {
        let mut pane = make_pane(AgentType::Claude, PaneStatus::Idle);
        pane.pane_id = format!("%{}", i);
        panes.push(pane);
    }

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: panes.clone(),
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", panes)];
    state.focus_state.sidebar_focused = true;
    state.focus_state.focus = Focus::Panes;
    state.rebuild_row_targets();

    // Cursor adopted from another window; auto-scroll moves the list down.
    state.global.selected_pane_row = 7;
    let _ = render_to_string(&mut state, 28, 5);
    assert!(
        state.scrolls.panes.offset > 0,
        "an adopted cursor should scroll the list"
    );

    state.reset_pane_scroll();
    let _ = render_to_string(&mut state, 28, 5);
    assert_eq!(state.global.selected_pane_row, 0);
    assert_eq!(
        state.scrolls.panes.offset, 0,
        "window switch must anchor the list at the top"
    );
}

// ─── Agents: sidebar focus gain selects the tmux context ────────────

#[test]
fn test_focus_gain_selects_context_agent_and_scrolls_it_into_view() {
    // When the user jumps into the sidebar, `app::run` calls
    // `select_focused_context_row` on the sidebar_focused false→true
    // transition: the agent matching the pane the user came from is
    // selected instead of the top row, and the auto-scroll brings that
    // row into view.
    let mut panes = Vec::new();
    for i in 0..10 {
        let mut pane = make_pane(AgentType::Claude, PaneStatus::Idle);
        pane.pane_id = format!("%{}", i);
        panes.push(pane);
    }

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: panes.clone(),
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", panes)];
    state.focus_state.focus = Focus::Panes;
    state.rebuild_row_targets();

    // The pane that held focus in this window just before the jump,
    // preserved across the focus-gain tick by `find_focused_pane_from`.
    state.focus_state.focused_pane_id = Some("%9".into());

    assert!(state.select_focused_context_row());
    assert_eq!(state.global.selected_pane_row, 9);

    let _ = render_to_string(&mut state, 28, 5);
    assert!(
        state.scrolls.panes.offset > 0,
        "the context agent should be scrolled into view"
    );
}

#[test]
fn test_empty_view_clamps_scroll_and_resets_cursor() {
    // Switching to a status view with no matching agents must not keep a
    // stale offset (which renders blank) or a stale cursor (which the next
    // view would auto-scroll to).
    let mut panes = Vec::new();
    for i in 0..8 {
        let mut pane = make_pane(AgentType::Claude, PaneStatus::Idle);
        pane.pane_id = format!("%{}", i);
        panes.push(pane);
    }

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: panes.clone(),
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", panes)];
    state.focus_state.sidebar_focused = true;
    state.focus_state.focus = Focus::Panes;
    state.rebuild_row_targets();

    state.global.selected_pane_row = 7;
    let _ = render_to_string(&mut state, 28, 5);
    assert!(state.scrolls.panes.offset > 0, "scrolled down in All view");

    state.global.status_filter = StatusFilter::Error;
    state.rebuild_row_targets();
    let _ = render_to_string(&mut state, 28, 5);

    assert_eq!(state.scrolls.panes.total_lines, 0);
    assert_eq!(
        state.scrolls.panes.offset, 0,
        "empty view must not keep a stale offset"
    );
    assert_eq!(
        state.global.selected_pane_row, 0,
        "empty view must reset the cursor"
    );
}

// ─── Repo popup rendering ───────────────────────────────────────────

#[test]
fn repo_popup_renders_repo_names_when_open() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Idle);
    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "frontend".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![
        make_repo_group("frontend", vec![pane.clone()]),
        make_repo_group("backend", vec![pane.clone()]),
    ];
    state.rebuild_row_targets();
    state.popup = tmux_agent_sidebar::state::PopupState::Repo {
        selected: 0,
        area: None,
    };

    // The snapshot locks in that the popup lists the `All` entry plus both
    // repo names when opened.
    insta::assert_snapshot!(render_to_string(&mut state, 40, 30), @"
     ≡2  ●0  ◎0  ◐0  ○2  ✕0
    ⓘ                                    — ▾
    frontend                    ┌──────────┐
    ┃ ○ claude                  │ All      │
        Waiting for prompt…     │ frontend │
                                │ backend  │
    backend                     └──────────┘
    ┃ ○ claude
        Waiting for prompt…
    ");
    // The popup area is required for click hit-testing and is non-visual
    // state, so it stays as a direct assertion.
    assert!(
        state.repo_popup_area().is_some(),
        "render should populate repo popup area for hit-testing"
    );
}

#[test]
fn repo_popup_highlights_selected_entry_with_background() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Idle);
    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "frontend".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![
        make_repo_group("frontend", vec![pane.clone()]),
        make_repo_group("backend", vec![pane.clone()]),
    ];
    state.rebuild_row_targets();
    state.focus_state.sidebar_focused = false; // surface raw colors instead of REVERSED
    state.popup = tmux_agent_sidebar::state::PopupState::Repo {
        selected: 2, // "backend" (0=All, 1=frontend, 2=backend)
        area: None,
    };

    // Styled snapshot locks in that the `backend` row carries the selection
    // background (bg:239) on each cell of the entry.
    insta::assert_snapshot!(render_to_styled_string(&mut state, 40, 30), @"
     ≡[fg:111]2[fg:255]  ●[fg:245]0[fg:245]  ◎[fg:245]0[fg:245]  ◐[fg:245]0[fg:245]  ○[fg:245]2[fg:255]  ✕[fg:245]0[fg:245]
    ⓘ[fg:221]                                    —[fg:255] ▾[fg:255]
    f[fg:153]r[fg:153]o[fg:153]n[fg:153]t[fg:153]e[fg:153]n[fg:153]d[fg:153]                    ┌[fg:153]─[fg:153]─[fg:153]─[fg:153]─[fg:153]─[fg:153]─[fg:153]─[fg:153]─[fg:153]─[fg:153]─[fg:153]┐[fg:153]
    ┃[fg:153] ○[fg:110] [fg:173]c[fg:173]l[fg:173]a[fg:173]u[fg:173]d[fg:173]e[fg:173]                  │[fg:153] [fg:255]A[fg:255]l[fg:255]l[fg:255] [fg:255] [fg:255] [fg:255] [fg:255] [fg:255] [fg:255]│[fg:153]
       [fg:255] [fg:255]W[fg:255]a[fg:255]i[fg:255]t[fg:255]i[fg:255]n[fg:255]g[fg:255] [fg:255]f[fg:255]o[fg:255]r[fg:255] [fg:255]p[fg:255]r[fg:255]o[fg:255]m[fg:255]p[fg:255]t[fg:255]…[fg:255]     │[fg:153] [fg:252]f[fg:252]r[fg:252]o[fg:252]n[fg:252]t[fg:252]e[fg:252]n[fg:252]d[fg:252] [fg:252]│[fg:153]
                                │[fg:153] [fg:255,bg:239]b[fg:255,bg:239]a[fg:255,bg:239]c[fg:255,bg:239]k[fg:255,bg:239]e[fg:255,bg:239]n[fg:255,bg:239]d[fg:255,bg:239] [fg:255,bg:239] [fg:255,bg:239]│[fg:153]
    b[fg:153]a[fg:153]c[fg:153]k[fg:153]e[fg:153]n[fg:153]d[fg:153]                     └[fg:153]─[fg:153]─[fg:153]─[fg:153]─[fg:153]─[fg:153]─[fg:153]─[fg:153]─[fg:153]─[fg:153]─[fg:153]┘[fg:153]
    ┃[fg:153] ○[fg:110] [fg:173]c[fg:173]l[fg:173]a[fg:173]u[fg:173]d[fg:173]e[fg:173]
       [fg:255] [fg:255]W[fg:255]a[fg:255]i[fg:255]t[fg:255]i[fg:255]n[fg:255]g[fg:255] [fg:255]f[fg:255]o[fg:255]r[fg:255] [fg:255]p[fg:255]r[fg:255]o[fg:255]m[fg:255]p[fg:255]t[fg:255]…[fg:255]
    ");
}

// ─── Agents: compact mode ───────────────────────────────────────────

#[test]
fn snapshot_compact_mode_renders_one_line_per_pane() {
    // Compact mode collapses each pane to a single row: status icon,
    // session title, badges, elapsed.
    let mut running = make_pane(AgentType::Claude, PaneStatus::Running);
    running.pane_id = "%1".into();
    running.session_name = "fix-api".into();
    running.started_at = Some(FIXED_NOW - 65);

    let mut idle = make_pane(AgentType::Codex, PaneStatus::Idle);
    idle.pane_id = "%2".into();
    idle.session_name = "docs".into();
    idle.subagents = vec!["Explore".into(), "Plan".into()];

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![running.clone(), idle.clone()],
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", vec![running, idle])];
    state.rebuild_row_targets();
    state.focus_state.sidebar_focused = false;
    state.global.compact = true;

    insta::assert_snapshot!(render_to_string(&mut state, 28, 14), @"
     ≡2  ●1  ◎0  ◐0  ○1  ✕0
    ⓘ                        — ▾
    project
    ┃ ● fix-api             1m5s
      ○ docs +2
    ");
}

#[test]
fn snapshot_compact_mode_multiple_repo_groups() {
    let mut a = make_pane(AgentType::Claude, PaneStatus::Running);
    a.pane_id = "%1".into();
    a.session_name = "alpha".into();
    let mut b = make_pane(AgentType::Codex, PaneStatus::Running);
    b.pane_id = "%2".into();
    b.session_name = "beta".into();

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![a.clone(), b.clone()],
        }],
    }]);
    state.repo_groups = vec![
        make_repo_group("frontend", vec![a]),
        make_repo_group("backend", vec![b]),
    ];
    state.rebuild_row_targets();
    state.focus_state.sidebar_focused = false;
    state.global.compact = true;

    insta::assert_snapshot!(render_to_string(&mut state, 28, 14), @"
     ≡2  ●2  ◎0  ◐0  ○0  ✕0
    ⓘ                        — ▾
    frontend
    ┃ ● alpha
    backend
      ● beta
    ");
}

#[test]
fn snapshot_compact_mode_spawned_worktree_remove_marker() {
    // Sidebar-spawned worktrees keep the trailing `×` remove affordance
    // in compact mode, pinned to the rightmost column.
    let mut pane = make_pane(AgentType::Claude, PaneStatus::Running);
    pane.pane_id = "%1".into();
    pane.session_name = "fix-api".into();
    pane.sidebar_spawned = true;

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![RepoGroup {
        name: "project".into(),
        has_focus: true,
        panes: vec![(
            pane,
            PaneGitInfo {
                repo_root: Some("/home/user/project".into()),
                branch: Some("agent/fix-api".into()),
                is_worktree: true,
                ..Default::default()
            },
        )],
    }];
    state.rebuild_row_targets();
    state.focus_state.sidebar_focused = false;
    state.global.compact = true;

    insta::assert_snapshot!(render_to_string(&mut state, 28, 14), @"
     ≡1  ●1  ◎0  ◐0  ○0  ✕0
    ⓘ                        — ▾
    project                    +
    ┃ ● fix-api + agent/fix-a… ×
    ");
}

// ─── Search ─────────────────────────────────────────────────────────

fn search_pane(id: &str, agent: AgentType, tmux_session: &str, session_name: &str) -> PaneInfo {
    let mut pane = make_pane(agent, PaneStatus::Idle);
    pane.pane_id = id.into();
    pane.tmux_session = tmux_session.into();
    pane.session_name = session_name.into();
    pane
}

fn search_state() -> AppState {
    let mut state = make_state(vec![]);
    state.repo_groups = vec![
        make_repo_group(
            "backend",
            vec![
                search_pane("%1", AgentType::Claude, "work", "claude-api"),
                search_pane("%2", AgentType::Codex, "work", "codex-ui"),
            ],
        ),
        make_repo_group(
            "frontend",
            vec![search_pane("%3", AgentType::Claude, "web", "frontend")],
        ),
    ];
    state.rebuild_row_targets();
    state
}

#[test]
fn snapshot_search_bar_filters_list_but_keeps_group() {
    let mut state = search_state();
    state.search.query = "codex".into();
    state.search.active = true;
    state.rebuild_row_targets();

    insta::assert_snapshot!(render_to_string(&mut state, 40, 20), @"
     ≡1  ●0  ◎0  ◐0  ○1  ✕0
    ⓘ /codex█                            — ▾
    backend
      ○ codex-ui
        Waiting for prompt…
    ");
}

#[test]
fn snapshot_search_no_matches_empty_state() {
    let mut state = search_state();
    state.search.query = "zzz".into();
    state.search.active = false;
    state.rebuild_row_targets();

    insta::assert_snapshot!(render_to_string(&mut state, 40, 20), @"
     ≡0  ●0  ◎0  ◐0  ○0  ✕0
    ⓘ /zzz                               — ▾
      no matches for zzz
    ");
}
