#[allow(dead_code, unused_imports)]
mod test_helpers;

use ratatui::style::Color;
use test_helpers::*;
use tmux_agent_sidebar::activity::{TaskProgress, TaskStatus};
use tmux_agent_sidebar::tmux::{
    AgentType, PaneAttention, PaneStatus, PermissionMode, SessionInfo, WindowInfo,
};
use tmux_agent_sidebar::ui::colors::ColorTheme;

// ─── ColorTheme Default Values ──────────────────────────────────────

#[test]
fn test_all_color_theme_defaults() {
    let theme = ColorTheme::default();

    // Core UI colors
    assert_eq!(theme.accent, Color::Indexed(153));
    assert_eq!(theme.border_inactive, Color::Indexed(240));
    assert_eq!(theme.selection_bg, Color::Indexed(239));

    // Status colors
    assert_eq!(theme.status_all, Color::Indexed(111));
    assert_eq!(theme.status_running, Color::Indexed(114));
    assert_eq!(theme.status_waiting, Color::Indexed(221));
    assert_eq!(theme.status_idle, Color::Indexed(110));
    assert_eq!(theme.status_error, Color::Indexed(167));
    assert_eq!(theme.status_unknown, Color::Indexed(244));

    // Agent colors
    assert_eq!(theme.agent_claude, Color::Indexed(174));
    assert_eq!(theme.agent_codex, Color::Indexed(141));

    // Text colors
    assert_eq!(theme.text_active, Color::Indexed(255));
    assert_eq!(theme.text_muted, Color::Indexed(252));
    assert_eq!(theme.text_inactive, Color::Indexed(244));

    // Header/UI element colors
    assert_eq!(theme.session_header, Color::Indexed(39));
    assert_eq!(theme.port, Color::Indexed(246));
    assert_eq!(theme.wait_reason, Color::Indexed(221));
    assert_eq!(theme.branch, Color::Indexed(109));

    // New theme fields
    assert_eq!(theme.badge_danger, Color::Indexed(167));
    assert_eq!(theme.badge_auto, Color::Indexed(221));
    assert_eq!(theme.task_progress, Color::Indexed(223));
    assert_eq!(theme.subagent, Color::Indexed(73));
    assert_eq!(theme.commit_hash, Color::Indexed(221));
    assert_eq!(theme.diff_added, Color::Indexed(114));
    assert_eq!(theme.diff_deleted, Color::Indexed(174));
    assert_eq!(theme.file_change, Color::Indexed(221));
    assert_eq!(theme.pr_link, Color::Indexed(117));
}

// ─── status_color() for all PaneStatus variants ─────────────────────

#[test]
fn test_status_color_all_variants() {
    let theme = ColorTheme::default();

    assert_eq!(
        theme.status_color(&PaneStatus::Running),
        Color::Indexed(114)
    );
    assert_eq!(
        theme.status_color(&PaneStatus::Waiting),
        Color::Indexed(221)
    );
    assert_eq!(theme.status_color(&PaneStatus::Idle), Color::Indexed(110));
    assert_eq!(theme.status_color(&PaneStatus::Error), Color::Indexed(167));
    assert_eq!(
        theme.status_color(&PaneStatus::Unknown),
        Color::Indexed(244)
    );
}

#[test]
fn test_status_color_attention_overrides_all() {
    let theme = ColorTheme::default();

    // Notification attention always renders in the waiting color.
    assert_eq!(
        theme.attention_color(PaneAttention::Notification, 3),
        Some(theme.status_waiting),
        "notification attention should render in the waiting color"
    );
    // Done attention breathes green instead, ending back at the start
    // of the cycle.
    assert_eq!(
        theme.attention_color(PaneAttention::Done, 0),
        Some(Color::Indexed(22)),
        "done attention should start the pulse dark"
    );
    assert_eq!(
        theme.attention_color(PaneAttention::Done, 3),
        Some(Color::Indexed(114)),
        "done attention should peak at the running green"
    );
    assert_eq!(
        theme.attention_color(PaneAttention::Done, 6),
        theme.attention_color(PaneAttention::Done, 0),
        "done attention should wrap its pulse cycle"
    );
    // No flag → no attention color, caller falls back to status_color.
    assert_eq!(theme.attention_color(PaneAttention::None, 2), None);
}

// ─── agent_color() for all AgentType variants ───────────────────────

#[test]
fn test_agent_color_all_variants() {
    let theme = ColorTheme::default();

    assert_eq!(theme.agent_color(&AgentType::Claude), Color::Indexed(174));
    assert_eq!(theme.agent_color(&AgentType::Codex), Color::Indexed(141));
    assert_eq!(theme.agent_color(&AgentType::Unknown), theme.status_unknown);
}

// ─── PermissionMode badge colors ────────────────────────────────────

#[test]
fn test_permission_mode_bypass_all_renders_danger_color() {
    let mut pane = make_pane(AgentType::Claude, PaneStatus::Running);
    pane.permission_mode = PermissionMode::BypassPermissions;

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

    // Styled snapshot locks in the BypassAll `!` badge rendered with
    // badge_danger (fg:167).
    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 26), @"
     ≡[fg:111]1[fg:255]  ●[fg:245]1[fg:255]  ◎[fg:245]0[fg:245]  ◐[fg:245]0[fg:245]  ○[fg:245]0[fg:245]  ✕[fg:245]0[fg:245]
    ⓘ[fg:221]                        —[fg:252] ▾[fg:252]
    p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]
    ┃[fg:153] ●[fg:82] [fg:174]c[fg:174]l[fg:174]a[fg:174]u[fg:174]d[fg:174]e[fg:174] [fg:167]![fg:167]
    ");
}

#[test]
fn test_permission_mode_full_auto_renders_auto_color() {
    let mut pane = make_pane(AgentType::Claude, PaneStatus::Running);
    pane.permission_mode = PermissionMode::Auto;

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

    // Styled snapshot locks in the Auto `auto` badge rendered with
    // badge_auto (fg:221).
    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 26), @"
     ≡[fg:111]1[fg:255]  ●[fg:245]1[fg:255]  ◎[fg:245]0[fg:245]  ◐[fg:245]0[fg:245]  ○[fg:245]0[fg:245]  ✕[fg:245]0[fg:245]
    ⓘ[fg:221]                        —[fg:252] ▾[fg:252]
    p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]
    ┃[fg:153] ●[fg:82] [fg:174]c[fg:174]l[fg:174]a[fg:174]u[fg:174]d[fg:174]e[fg:174] [fg:221]a[fg:221]u[fg:221]t[fg:221]o[fg:221]
    ");
}

#[test]
fn test_permission_mode_normal_no_badge() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Running);
    // permission_mode is Normal by default in make_pane

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

    // Snapshot locks in that the agent row shows no badge in Normal mode.
    insta::assert_snapshot!(render_to_string(&mut state, 28, 25), @r"
 ≡1  ●1  ◎0  ◐0  ○0  ✕0
ⓘ                        — ▾
project
┃ ● claude
    ");
}

// ─── Git status summary colors ──────────────────────────────────────

#[test]
fn test_task_progress_line_uses_task_progress_color() {
    let mut pane = make_pane(AgentType::Claude, PaneStatus::Running);
    pane.pane_id = "%1".into();

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

    // Set task progress for pane %1
    let progress = TaskProgress {
        tasks: vec![
            ("Task A".into(), TaskStatus::Completed),
            ("Task B".into(), TaskStatus::InProgress),
            ("Task C".into(), TaskStatus::Pending),
        ],
    };
    state.set_pane_task_progress("%1", Some(progress));

    // Styled snapshot locks in both the task_progress color (fg:223) and
    // the progress glyphs (✔/◼/◻) with the "1/3" count.
    insta::assert_snapshot!(render_to_styled_string(&mut state, 40, 40), @"
     ≡[fg:111]1[fg:255]  ●[fg:245]1[fg:255]  ◎[fg:245]0[fg:245]  ◐[fg:245]0[fg:245]  ○[fg:245]0[fg:245]  ✕[fg:245]0[fg:245]
    ⓘ[fg:221]                                    —[fg:252] ▾[fg:252]
    p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]
    ┃[fg:153] ●[fg:82] [fg:174]c[fg:174]l[fg:174]a[fg:174]u[fg:174]d[fg:174]e[fg:174]
       [fg:223] [fg:223]✔[fg:223]◼[fg:223]◻[fg:223] [fg:223]1[fg:223]/[fg:223]3[fg:223]
    ");
}

#[test]
fn test_subagent_line_uses_subagent_color() {
    let mut pane = make_pane(AgentType::Claude, PaneStatus::Running);
    pane.subagents = vec!["Explore #1".into()];

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

    // Styled snapshot locks in the subagent line color (fg:73) plus the
    // rendered "Explore #1" label.
    insta::assert_snapshot!(render_to_styled_string(&mut state, 40, 27), @"
     ≡[fg:111]1[fg:255]  ●[fg:245]1[fg:255]  ◎[fg:245]0[fg:245]  ◐[fg:245]0[fg:245]  ○[fg:245]0[fg:245]  ✕[fg:245]0[fg:245]
    ⓘ[fg:221]                                    —[fg:252] ▾[fg:252]
    p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]
    ┃[fg:153] ●[fg:82] [fg:174]c[fg:174]l[fg:174]a[fg:174]u[fg:174]d[fg:174]e[fg:174]
       [fg:252] [fg:252]└[fg:252] [fg:252]E[fg:73]x[fg:73]p[fg:73]l[fg:73]o[fg:73]r[fg:73]e[fg:73] [fg:73]#[fg:73]1[fg:73]
    ");
}

#[test]
fn test_response_arrow_uses_response_arrow_color() {
    let mut pane = make_pane(AgentType::Claude, PaneStatus::Idle);
    pane.pane_active = false;
    pane.prompt = "Task completed successfully".into();
    pane.prompt_is_response = true;

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

    // Styled snapshot locks in:
    //   • response_arrow color (fg:81) + bold on the ▷ glyph
    //   • text_active color (fg:255) on the focused response text
    insta::assert_snapshot!(render_to_styled_string(&mut state, 40, 27), @"
     ≡[fg:111]1[fg:255]  ●[fg:245]0[fg:245]  ◎[fg:245]0[fg:245]  ◐[fg:245]0[fg:245]  ○[fg:245]1[fg:255]  ✕[fg:245]0[fg:245]
    ⓘ[fg:221]                                    —[fg:252] ▾[fg:252]
    p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]
    ┃[fg:153] ○[fg:110] [fg:174]c[fg:174]l[fg:174]a[fg:174]u[fg:174]d[fg:174]e[fg:174]
      ▷[fg:81,bold] [fg:81,bold]T[fg:244]a[fg:244]s[fg:244]k[fg:244] [fg:244]c[fg:244]o[fg:244]m[fg:244]p[fg:244]l[fg:244]e[fg:244]t[fg:244]e[fg:244]d[fg:244] [fg:244]s[fg:244]u[fg:244]c[fg:244]c[fg:244]e[fg:244]s[fg:244]s[fg:244]f[fg:244]u[fg:244]l[fg:244]l[fg:244]y[fg:244]
    ");
}

// test_commit_hash_uses_commit_hash_color was removed because
// git_last_commit and commit hash rendering no longer exist.

#[test]
fn test_custom_theme_new_fields_override() {
    let theme = ColorTheme {
        badge_danger: Color::Indexed(196),
        badge_auto: Color::Indexed(226),
        task_progress: Color::Indexed(200),
        subagent: Color::Indexed(100),
        commit_hash: Color::Indexed(150),
        diff_added: Color::Indexed(46),
        diff_deleted: Color::Indexed(160),
        file_change: Color::Indexed(208),
        pr_link: Color::Indexed(33),
        port: Color::Indexed(82),
        ..ColorTheme::default()
    };

    assert_eq!(theme.badge_danger, Color::Indexed(196));
    assert_eq!(theme.badge_auto, Color::Indexed(226));
    assert_eq!(theme.task_progress, Color::Indexed(200));
    assert_eq!(theme.subagent, Color::Indexed(100));
    assert_eq!(theme.commit_hash, Color::Indexed(150));
    assert_eq!(theme.diff_added, Color::Indexed(46));
    assert_eq!(theme.diff_deleted, Color::Indexed(160));
    assert_eq!(theme.file_change, Color::Indexed(208));
    assert_eq!(theme.pr_link, Color::Indexed(33));
    assert_eq!(theme.port, Color::Indexed(82));

    // Original fields should still be default
    assert_eq!(theme.accent, Color::Indexed(153));
    assert_eq!(theme.agent_claude, Color::Indexed(174));
}

// ─── Branch color in styled output ──────────────────────────────────

#[test]
fn test_branch_color_in_agent_panel() {
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
    state.repo_groups = vec![tmux_agent_sidebar::group::RepoGroup {
        name: "project".into(),
        has_focus: true,
        panes: vec![(
            pane,
            tmux_agent_sidebar::group::PaneGitInfo {
                repo_root: Some("/home/user/project".into()),
                branch: Some("feature/cool-feature".into()),
                is_worktree: false,
                ..Default::default()
            },
        )],
    }];
    state.rebuild_row_targets();
    state.focus_state.sidebar_focused = false;

    // Styled snapshot locks in the branch name rendered with branch color
    // (fg:109).
    insta::assert_snapshot!(render_to_styled_string(&mut state, 40, 26), @r"
     ≡[fg:111]1[fg:255]  ●[fg:245]1[fg:255]  ◎[fg:245]0[fg:245]  ◐[fg:245]0[fg:245]  ○[fg:245]0[fg:245]  ✕[fg:245]0[fg:245]
    ⓘ[fg:221]                                    —[fg:252] ▾[fg:252]
    p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]                                +[fg:153]
    ┃[fg:153] ●[fg:82] [fg:174]c[fg:174]l[fg:174]a[fg:174]u[fg:174]d[fg:174]e[fg:174]
    ┃[fg:153]  [fg:109] [fg:109]f[fg:109]e[fg:109]a[fg:109]t[fg:109]u[fg:109]r[fg:109]e[fg:109]/[fg:109]c[fg:109]o[fg:109]o[fg:109]l[fg:109]-[fg:109]f[fg:109]e[fg:109]a[fg:109]t[fg:109]u[fg:109]r[fg:109]e[fg:109]
    ");
}

// ─── Selection background color ─────────────────────────────────────

#[test]
fn test_selection_bg_color_applied() {
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

    // Styled snapshot locks in the selected agent row's selection
    // background (bg:239).
    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 25), @"
     ≡[fg:111]1[fg:255]  ●[fg:245]0[fg:245]  ◎[fg:245]0[fg:245]  ◐[fg:245]0[fg:245]  ○[fg:245]1[fg:255]  ✕[fg:245]0[fg:245]
    ⓘ[fg:221]                        —[fg:252] ▾[fg:252]
    p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]
    ┃[fg:153,bg:239] [bg:239]○[fg:110,bg:239] [fg:174,bg:239]c[fg:174,bg:239]l[fg:174,bg:239]a[fg:174,bg:239]u[fg:174,bg:239]d[fg:174,bg:239]e[fg:174,bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239]
       [fg:255] [fg:255]W[fg:255]a[fg:255]i[fg:255]t[fg:255]i[fg:255]n[fg:255]g[fg:255] [fg:255]f[fg:255]o[fg:255]r[fg:255] [fg:255]p[fg:255]r[fg:255]o[fg:255]m[fg:255]p[fg:255]t[fg:255]…[fg:255]
    ");
}

// ─── Accent (focused) vs border_inactive colors ─────────────────────

#[test]
fn test_accent_vs_border_inactive_colors() {
    let mut pane1 = make_pane(AgentType::Claude, PaneStatus::Running);
    pane1.pane_id = "%1".into();
    let mut pane2 = make_pane(AgentType::Codex, PaneStatus::Idle);
    pane2.pane_id = "%2".into();

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@0".into(),
            window_name: "fish".into(),
            window_active: true,
            auto_rename: true,
            panes: vec![pane1.clone(), pane2.clone()],
        }],
    }]);
    state.repo_groups = vec![
        tmux_agent_sidebar::group::RepoGroup {
            name: "focused-repo".into(),
            has_focus: true,
            panes: vec![(pane1, tmux_agent_sidebar::group::PaneGitInfo::default())],
        },
        tmux_agent_sidebar::group::RepoGroup {
            name: "unfocused-repo".into(),
            has_focus: false,
            panes: vec![(pane2, tmux_agent_sidebar::group::PaneGitInfo::default())],
        },
    ];
    state.focus_state.focused_pane_id = Some("%1".into());
    state.rebuild_row_targets();

    // Styled snapshot locks in:
    //   • focused group header rendered with accent (fg:153)
    //   • unfocused group header rendered with border_inactive (fg:240)
    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 30), @"
     ≡[fg:111]2[fg:255]  ●[fg:245]1[fg:255]  ◎[fg:245]0[fg:245]  ◐[fg:245]0[fg:245]  ○[fg:245]1[fg:255]  ✕[fg:245]0[fg:245]
    ⓘ[fg:221]                        —[fg:252] ▾[fg:252]
    f[fg:153]o[fg:153]c[fg:153]u[fg:153]s[fg:153]e[fg:153]d[fg:153]-[fg:153]r[fg:153]e[fg:153]p[fg:153]o[fg:153]
    ┃[fg:153,bg:239] [bg:239]●[fg:82,bg:239] [fg:174,bg:239]c[fg:174,bg:239]l[fg:174,bg:239]a[fg:174,bg:239]u[fg:174,bg:239]d[fg:174,bg:239]e[fg:174,bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239] [bg:239]

    u[fg:255]n[fg:255]f[fg:255]o[fg:255]c[fg:255]u[fg:255]s[fg:255]e[fg:255]d[fg:255]-[fg:255]r[fg:255]e[fg:255]p[fg:255]o[fg:255]
      ○[fg:110] [fg:141]c[fg:141]o[fg:141]d[fg:141]e[fg:141]x[fg:141]
       [fg:244] [fg:244]W[fg:244]a[fg:244]i[fg:244]t[fg:244]i[fg:244]n[fg:244]g[fg:244] [fg:244]f[fg:244]o[fg:244]r[fg:244] [fg:244]p[fg:244]r[fg:244]o[fg:244]m[fg:244]p[fg:244]t[fg:244]…[fg:244]
    ");
}

// ─── Status color rendering for each PaneStatus ─────────────────────

#[test]
fn test_running_status_color_in_output() {
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

    // Styled snapshot locks in the running spinner using SPINNER_PULSE[0]
    // color (fg:82).
    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 25), @r"
     ≡[fg:111]1[fg:255]  ●[fg:245]1[fg:255]  ◎[fg:245]0[fg:245]  ◐[fg:245]0[fg:245]  ○[fg:245]0[fg:245]  ✕[fg:245]0[fg:245]
    ⓘ[fg:221]                        —[fg:252] ▾[fg:252]
    p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]
    ┃[fg:153] ●[fg:82] [fg:174]c[fg:174]l[fg:174]a[fg:174]u[fg:174]d[fg:174]e[fg:174]
    ");
}

#[test]
fn test_waiting_status_color_in_output() {
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

    // Styled snapshot locks in the waiting status using status_waiting
    // color (fg:221).
    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 25), @r"
     ≡[fg:111]1[fg:255]  ●[fg:245]0[fg:245]  ◎[fg:245]0[fg:245]  ◐[fg:245]1[fg:255]  ○[fg:245]0[fg:245]  ✕[fg:245]0[fg:245]
    ⓘ[fg:221]                        —[fg:252] ▾[fg:252]
    p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]
    ┃[fg:153] ◐[fg:221] [fg:174]c[fg:174]l[fg:174]a[fg:174]u[fg:174]d[fg:174]e[fg:174]
    ");
}

#[test]
fn test_error_status_color_in_output() {
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

    // Styled snapshot locks in the error status using status_error
    // color (fg:167).
    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 25), @r"
     ≡[fg:111]1[fg:255]  ●[fg:245]0[fg:245]  ◎[fg:245]0[fg:245]  ◐[fg:245]0[fg:245]  ○[fg:245]0[fg:245]  ✕[fg:245]1[fg:255]
    ⓘ[fg:221]                        —[fg:252] ▾[fg:252]
    p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]
    ┃[fg:153] ✕[fg:167] [fg:174]c[fg:174]l[fg:174]a[fg:174]u[fg:174]d[fg:174]e[fg:174]
    ");
}

#[test]
fn test_idle_status_color_in_output() {
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
    state.focus_state.sidebar_focused = false;

    // Styled snapshot locks in the idle status using status_idle
    // color (fg:110).
    insta::assert_snapshot!(render_to_styled_string(&mut state, 28, 25), @"
     ≡[fg:111]1[fg:255]  ●[fg:245]0[fg:245]  ◎[fg:245]0[fg:245]  ◐[fg:245]0[fg:245]  ○[fg:245]1[fg:255]  ✕[fg:245]0[fg:245]
    ⓘ[fg:221]                        —[fg:252] ▾[fg:252]
    p[fg:153]r[fg:153]o[fg:153]j[fg:153]e[fg:153]c[fg:153]t[fg:153]
    ┃[fg:153] ○[fg:110] [fg:174]c[fg:174]l[fg:174]a[fg:174]u[fg:174]d[fg:174]e[fg:174]
       [fg:255] [fg:255]W[fg:255]a[fg:255]i[fg:255]t[fg:255]i[fg:255]n[fg:255]g[fg:255] [fg:255]f[fg:255]o[fg:255]r[fg:255] [fg:255]p[fg:255]r[fg:255]o[fg:255]m[fg:255]p[fg:255]t[fg:255]…[fg:255]
    ");
}

#[test]
fn test_unknown_status_color_in_output() {
    let theme = tmux_agent_sidebar::ui::colors::ColorTheme::default();
    assert_eq!(
        theme.status_color(&PaneStatus::Unknown),
        ratatui::style::Color::Indexed(244)
    );
}
