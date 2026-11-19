#[allow(dead_code, unused_imports)]
mod test_helpers;

use test_helpers::*;
use tmux_agent_sidebar::state::Focus;
use tmux_agent_sidebar::tmux::{AgentType, PaneStatus, SessionInfo, WindowInfo};

// ─── Styled Snapshot Tests for Selection and Focus ─────────────────

#[test]
fn snapshot_selected_focused_styled() {
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
    state.focus_state.sidebar_focused = true;
    state.global.selected_pane_row = 0;

    // Styled snapshot locks in the selected row's ┃[fg:153,bg:239] marker
    // and the selection background spanning its content cells.
    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 10), @"
    ☰[fg:252]                        ⓘ[fg:221] ▾[fg:252]
    ▾[fg:153] [fg:153]p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]
    ┃[fg:153,bg:239] [bg:239]○[fg:110,bg:239] [fg:173,bg:239]c[fg:173,bg:239]l[fg:173,bg:239]a[fg:173,bg:239]u[fg:173,bg:239]d[fg:173,bg:239]e[fg:173,bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239]
       [fg:255] [fg:255]W[fg:255]a[fg:255]i[fg:255]t[fg:255]i[fg:255]n[fg:255]g[fg:255] [fg:255]f[fg:255]o[fg:255]r[fg:255] [fg:255]p[fg:255]r[fg:255]o[fg:255]m[fg:255]p[fg:255]t[fg:255]…[fg:255]
    ");
}

#[test]
fn selection_marker_uses_accent_color_with_selection_bg() {
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
    state.focus_state.sidebar_focused = true;
    state.focus_state.focus = Focus::Panes;
    state.global.selected_pane_row = 0;

    // Styled snapshot locks in:
    //   1. the selected row begins with `┃[fg:153,bg:239]` (accent + selection bg)
    //   2. the selected row never contains the old frame `│`
    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 24), @"
    ☰[fg:252]                        ⓘ[fg:221] ▾[fg:252]
    ▾[fg:153] [fg:153]p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]
    ┃[fg:153,bg:239] [bg:239]●[fg:82,bg:239] [fg:173,bg:239]c[fg:173,bg:239]l[fg:173,bg:239]a[fg:173,bg:239]u[fg:173,bg:239]d[fg:173,bg:239]e[fg:173,bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239]
    ");
}

#[test]
fn selection_bg_covers_inner_padding() {
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
    state.focus_state.sidebar_focused = true;
    state.focus_state.focus = Focus::Panes;
    state.global.selected_pane_row = 0;

    // Styled snapshot locks in the selection background extending across the
    // inner padding immediately after the `┃` marker (` [bg:239]`).
    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 24), @"
    ☰[fg:252]                        ⓘ[fg:221] ▾[fg:252]
    ▾[fg:153] [fg:153]p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]
    ┃[fg:153,bg:239] [bg:239]○[fg:110,bg:239] [fg:173,bg:239]c[fg:173,bg:239]l[fg:173,bg:239]a[fg:173,bg:239]u[fg:173,bg:239]d[fg:173,bg:239]e[fg:173,bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239]
       [fg:255] [fg:255]W[fg:255]a[fg:255]i[fg:255]t[fg:255]i[fg:255]n[fg:255]g[fg:255] [fg:255]f[fg:255]o[fg:255]r[fg:255] [fg:255]p[fg:255]r[fg:255]o[fg:255]m[fg:255]p[fg:255]t[fg:255]…[fg:255]
    ");
}

#[test]
fn no_selection_bg_when_not_selected() {
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
    state.focus_state.sidebar_focused = false; // not focused → no selection

    // Styled snapshot locks in the absence of any selection background
    // (bg:239) while the sidebar is not focused.
    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 24), @"
    ☰[fg:252]                        ⓘ[fg:221] ▾[fg:252]
    ▾[fg:153] [fg:153]p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]
    ┃[fg:153] ●[fg:82] [fg:173]c[fg:173]l[fg:173]a[fg:173]u[fg:173]d[fg:173]e[fg:173]
    ");
}

// ─── Custom Theme Tests ─────────────────────────────────────────────

#[test]
fn snapshot_custom_theme_colors() {
    use ratatui::style::Color;
    use tmux_agent_sidebar::ui::colors::ColorTheme;

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

    // Override theme with custom colors
    state.theme = ColorTheme {
        accent: Color::Indexed(196),       // red accent
        agent_claude: Color::Indexed(226), // yellow agent
        status_idle: Color::Indexed(46),   // green idle
        port: Color::Indexed(39),          // cyan port
        ..ColorTheme::default()
    };
    // Unfocus sidebar so selected row doesn't use REVERSED (which hides colors)
    state.focus_state.sidebar_focused = false;

    // Styled snapshot locks in the custom theme colors (accent fg:196,
    // agent_claude fg:226, status_idle fg:46).
    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 10), @"
    ☰[fg:252]                        ⓘ[fg:221] ▾[fg:252]
    ▾[fg:196] [fg:196]p[fg:196]r[fg:196]o[fg:196]j[fg:196]e[fg:196]c[fg:196]t[fg:196]
    ┃[fg:196] ○[fg:46] [fg:226]c[fg:226]l[fg:226]a[fg:226]u[fg:226]d[fg:226]e[fg:226]
       [fg:255] [fg:255]W[fg:255]a[fg:255]i[fg:255]t[fg:255]i[fg:255]n[fg:255]g[fg:255] [fg:255]f[fg:255]o[fg:255]r[fg:255] [fg:255]p[fg:255]r[fg:255]o[fg:255]m[fg:255]p[fg:255]t[fg:255]…[fg:255]
    ");
}

#[test]
fn test_theme_default_matches_shell_colors() {
    use ratatui::style::Color;
    use tmux_agent_sidebar::ui::colors::ColorTheme;

    let theme = ColorTheme::default();

    // Verify defaults match shell version's agent-sidebar.conf
    assert_eq!(theme.accent, Color::Indexed(153));
    assert_eq!(theme.border_inactive, Color::Indexed(240));
    assert_eq!(theme.status_running, Color::Indexed(114));
    assert_eq!(theme.status_waiting, Color::Indexed(221));
    assert_eq!(theme.status_idle, Color::Indexed(110));
    assert_eq!(theme.status_error, Color::Indexed(167));
    assert_eq!(theme.agent_claude, Color::Indexed(173));
    assert_eq!(theme.agent_codex, Color::Indexed(141));
    assert_eq!(theme.text_active, Color::Indexed(255));
    assert_eq!(theme.text_muted, Color::Indexed(252));
    assert_eq!(theme.window_header, Color::Indexed(249));
    assert_eq!(theme.session_header, Color::Indexed(39));
    assert_eq!(theme.wait_reason, Color::Indexed(221));
}

// ─── Opt-in Sidebar Background ──────────────────────────────────────

// `@sidebar_color_background` is opt-in and off by default: every other
// styled snapshot in this file renders with no bg annotations outside
// the selection row, which is what keeps transparent terminals
// transparent. Set, the color must paint under the whole pane — header
// row and empty gutter included — while the selection wash still layers
// on top of it.
#[test]
fn snapshot_custom_background_paints_every_cell() {
    use ratatui::style::Color;

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
    state.theme.background = Some(Color::Indexed(236));

    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 10), @"
    ☰[fg:252,bg:236]  [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]ⓘ[fg:221,bg:236] [bg:236]▾[fg:252,bg:236]
    ▾[fg:153,bg:236] [fg:153,bg:236]p[fg:153,bg:236]r[fg:153,bg:236]o[fg:153,bg:236]j[fg:153,bg:236]e[fg:153,bg:236]c[fg:153,bg:236]t[fg:153,bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]
    ┃[fg:153,bg:239] [bg:239]○[fg:110,bg:239] [fg:173,bg:239]c[fg:173,bg:239]l[fg:173,bg:239]a[fg:173,bg:239]u[fg:173,bg:239]d[fg:173,bg:239]e[fg:173,bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239]
     [bg:236] [bg:236] [fg:255,bg:236] [fg:255,bg:236]W[fg:255,bg:236]a[fg:255,bg:236]i[fg:255,bg:236]t[fg:255,bg:236]i[fg:255,bg:236]n[fg:255,bg:236]g[fg:255,bg:236] [fg:255,bg:236]f[fg:255,bg:236]o[fg:255,bg:236]r[fg:255,bg:236] [fg:255,bg:236]p[fg:255,bg:236]r[fg:255,bg:236]o[fg:255,bg:236]m[fg:255,bg:236]p[fg:255,bg:236]t[fg:255,bg:236]…[fg:255,bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]
     [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]
     [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]
     [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]
     [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]
     [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]
     [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]
    ");
}

// The `?` help overlay is the one surface that starts with a `Clear`
// widget (which resets cells back to the terminal-default background),
// so it is where a custom background would visibly fall apart if the
// re-application after `Clear` regressed: the overlay must carry the
// custom bg across its border, gutter, and trailing blank rows.
#[test]
fn snapshot_custom_background_survives_help_overlay_clear() {
    use ratatui::style::Color;

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
    state.theme.background = Some(Color::Indexed(236));
    state.toggle_help_popup();

    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 12), @"
    ╭[fg:153,bg:236] [fg:153,bg:236]s[fg:153,bg:236]h[fg:153,bg:236]o[fg:153,bg:236]r[fg:153,bg:236]t[fg:153,bg:236]c[fg:153,bg:236]u[fg:153,bg:236]t[fg:153,bg:236]s[fg:153,bg:236] [fg:153,bg:236]·[fg:153,bg:236] [fg:153,bg:236]e[fg:153,bg:236]s[fg:153,bg:236]c[fg:153,bg:236] [fg:153,bg:236]t[fg:153,bg:236]o[fg:153,bg:236] [fg:153,bg:236]c[fg:153,bg:236]l[fg:153,bg:236]o[fg:153,bg:236]s[fg:153,bg:236]e[fg:153,bg:236] [fg:153,bg:236]╮[fg:153,bg:236]
    │[fg:153,bg:236] [fg:153,bg:236,bold]l[fg:153,bg:236,bold]i[fg:153,bg:236,bold]s[fg:153,bg:236,bold]t[fg:153,bg:236,bold] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]│[fg:153,bg:236]
    │[fg:153,bg:236] [fg:255,bg:236]j[fg:255,bg:236] [fg:255,bg:236]/[fg:255,bg:236] [fg:255,bg:236]k[fg:255,bg:236] [fg:255,bg:236]/[fg:255,bg:236] [fg:255,bg:236]↑[fg:255,bg:236] [fg:255,bg:236]/[fg:255,bg:236] [fg:255,bg:236]↓[fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236]M[fg:252,bg:236]o[fg:252,bg:236]v[fg:252,bg:236]e[fg:252,bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]│[fg:153,bg:236]
    │[fg:153,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236]s[fg:252,bg:236]e[fg:252,bg:236]l[fg:252,bg:236]e[fg:252,bg:236]c[fg:252,bg:236]t[fg:252,bg:236]i[fg:252,bg:236]o[fg:252,bg:236]n[fg:252,bg:236] [bg:236]│[fg:153,bg:236]
    │[fg:153,bg:236] [fg:255,bg:236]g[fg:255,bg:236] [fg:255,bg:236]/[fg:255,bg:236] [fg:255,bg:236]G[fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236]J[fg:252,bg:236]u[fg:252,bg:236]m[fg:252,bg:236]p[fg:252,bg:236] [fg:252,bg:236]t[fg:252,bg:236]o[fg:252,bg:236] [bg:236] [bg:236] [bg:236]│[fg:153,bg:236]
    │[fg:153,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236]t[fg:252,bg:236]o[fg:252,bg:236]p[fg:252,bg:236] [fg:252,bg:236]/[fg:252,bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]│[fg:153,bg:236]
    │[fg:153,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236]b[fg:252,bg:236]o[fg:252,bg:236]t[fg:252,bg:236]t[fg:252,bg:236]o[fg:252,bg:236]m[fg:252,bg:236] [bg:236] [bg:236] [bg:236] [bg:236]│[fg:153,bg:236]
    │[fg:153,bg:236] [fg:255,bg:236]P[fg:255,bg:236]g[fg:255,bg:236]U[fg:255,bg:236]p[fg:255,bg:236] [fg:255,bg:236]/[fg:255,bg:236] [fg:255,bg:236]P[fg:255,bg:236]g[fg:255,bg:236]D[fg:255,bg:236]n[fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236]P[fg:252,bg:236]a[fg:252,bg:236]g[fg:252,bg:236]e[fg:252,bg:236] [fg:252,bg:236]t[fg:252,bg:236]h[fg:252,bg:236]e[fg:252,bg:236] [bg:236] [bg:236]│[fg:153,bg:236]
    │[fg:153,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236]l[fg:252,bg:236]i[fg:252,bg:236]s[fg:252,bg:236]t[fg:252,bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]│[fg:153,bg:236]
    │[fg:153,bg:236] [fg:255,bg:236]a[fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236]N[fg:252,bg:236]e[fg:252,bg:236]x[fg:252,bg:236]t[fg:252,bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]│[fg:153,bg:236]
    │[fg:153,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236] [fg:255,bg:236]a[fg:252,bg:236]t[fg:252,bg:236]t[fg:252,bg:236]e[fg:252,bg:236]n[fg:252,bg:236]t[fg:252,bg:236]i[fg:252,bg:236]o[fg:252,bg:236]n[fg:252,bg:236] [bg:236]│[fg:153,bg:236]
    ╰[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]─[fg:153,bg:236]╯[fg:153,bg:236]
    ");
}
