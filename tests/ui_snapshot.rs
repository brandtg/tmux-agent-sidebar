#[allow(dead_code, unused_imports)]
mod test_helpers;

use test_helpers::*;
use tmux_agent_sidebar::group::OtherWindow;
use tmux_agent_sidebar::state::{AppState, RepoFilter};
use tmux_agent_sidebar::tmux::{
    AgentType, PaneAttention, PaneInfo, PaneStatus, SessionInfo, WindowInfo, WindowStatus,
    WorktreeMetadata,
};

// ─── UI Snapshot Tests ─────────────────────────────────────────────

#[test]
fn snapshot_single_agent_idle_ui() {
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

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @"
     ≡1  ●0  ◎0  ◐0  ○1  ✕0
    ⓘ                        — ▾
    project
    ┃ ○ claude
        Waiting for prompt…
    ");
}

// The done-unseen indicator is purely a color pulse on the status icon
// (asserted in color_tests), so this snapshot locks the text layout:
// a finished, unseen pane must render exactly like an idle one apart
// from the glyph color.
#[test]
fn snapshot_done_unseen_pane_matches_idle_layout() {
    let mut pane = make_pane(AgentType::Claude, PaneStatus::Idle);
    pane.attention = PaneAttention::Done;
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

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @"
     ≡1  ●1  ◎0  ◐0  ○1  ✕0
    ⓘ                        — ▾
    project
    ┃ ○ claude
        Waiting for prompt…
    ");
}

// Locks down the secondary header layout when there are no notices —
// `make_state()` injects a Claude missing-hook notice as the shared
// baseline so the ⓘ badge is on every other snapshot, which means a
// regression in the no-notices path would slip past unnoticed without
// this dedicated coverage.
#[test]
fn snapshot_secondary_header_without_notices() {
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
    state.notices.missing_hook_groups.clear();
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @"
     ≡1  ●0  ◎0  ◐0  ○1  ✕0
                             — ▾
    project
    ┃ ○ claude
        Waiting for prompt…
    ");
}

#[test]
fn snapshot_secondary_header_long_repo_filter_truncated() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Idle);
    let repo_name = "very-long-repository-name-that-exceeds-width";
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
    state.repo_groups = vec![make_repo_group(repo_name, vec![pane])];
    state.global.repo_filter = RepoFilter::Repo(repo_name.into());
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @"
     ≡1  ●0  ◎0  ◐0  ○1  ✕0
    ⓘ  very-long-repository-n… ▾
    very-long-repository-name-th
    ┃ ○ claude
        Waiting for prompt…
    ");
}

#[test]
fn snapshot_version_banner_does_not_duplicate_in_scroll_area() {
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
    state.version_notice = Some(tmux_agent_sidebar::version::UpdateNotice {
        local_version: "0.2.6".into(),
        latest_version: "0.2.7".into(),
    });
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 28, 10);
    insta::assert_snapshot!(output, @r"
     ≡1  ●0  ◎0  ◐0  ○1  ✕0
    ⓘ                        — ▾
    project
    ┃ ○ claude
        Waiting for prompt…
    ");
}

// Popup mode (mobile viewport): the toggle subcommand launches the TUI
// inside a tmux popup with SIDEBAR_POPUP=1, forcing window-active/focused
// semantics so the compact viewport stays readable.
#[test]
fn snapshot_popup_mode_forces_focus_semantics() {
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
    state.popup_mode = true;
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 40, 20);
    insta::assert_snapshot!(output, @r"
     ≡1  ●0  ◎0  ◐0  ○1  ✕0
    ⓘ                                    — ▾
    project
    ┃ ○ claude
        Waiting for prompt…
    ");
}

#[test]
fn snapshot_single_agent_running_with_elapsed() {
    let mut pane = make_pane(AgentType::Claude, PaneStatus::Running);
    pane.started_at = Some(FIXED_NOW - 125); // 2m5s ago

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "dotfiles".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![make_repo_group("dotfiles", vec![pane])];
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @r"
     ≡1  ●1  ◎0  ◐0  ○0  ✕0
    ⓘ                        — ▾
    dotfiles
    ┃ ● claude              2m5s
    ");
}

#[test]
fn snapshot_long_session_name_truncated_keeps_elapsed_visible() {
    let mut pane = make_pane(AgentType::Claude, PaneStatus::Running);
    pane.session_name = "this-is-a-ridiculously-long-session-name-that-will-not-fit".into();
    pane.started_at = Some(FIXED_NOW - 125); // 2m5s ago

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "dotfiles".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![make_repo_group("dotfiles", vec![pane])];
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @r"
     ≡1  ●1  ◎0  ◐0  ○0  ✕0
    ⓘ                        — ▾
    dotfiles
    ┃ ● this-is-a-ridiculo… 2m5s
    ");
}

#[test]
fn running_spinner_different_frame() {
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
    state.spinner_frame = 0;

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @r"
     ≡1  ●1  ◎0  ◐0  ○0  ✕0
    ⓘ                        — ▾
    project
    ┃ ● claude
    ");
}

#[test]
fn snapshot_agent_with_prompt_ui() {
    let mut pane = make_pane(AgentType::Claude, PaneStatus::Idle);
    pane.prompt = "fix the bug".into();

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

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @"
     ≡1  ●0  ◎0  ◐0  ○1  ✕0
    ⓘ                        — ▾
    project
    ┃ ○ claude
        fix the bug
    ");
}

#[test]
fn snapshot_agent_with_japanese_prompt_ui() {
    let mut pane = make_pane(AgentType::Claude, PaneStatus::Running);
    pane.prompt = "これって今1時間経っているけど、起動して確認しても問題ない？".into();

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

    let output = render_to_string(&mut state, 28, 27);
    insta::assert_snapshot!(output, @"
     ≡1  ●1  ◎0  ◐0  ○0  ✕0
    ⓘ                        — ▾
    project
    ┃ ● claude
        こ れ っ て 今 1時 間 経 っ て い
        る け ど 、 起 動 し て 確 認 し …
    ");
}

#[test]
fn snapshot_two_agents_same_window_ui() {
    let pane1 = PaneInfo {
        pane_id: "%1".into(),
        pane_active: true,
        status: PaneStatus::Running,
        attention: PaneAttention::None,
        agent: AgentType::Claude,
        path: "/home/user/project".into(),
        launch_cwd: String::new(),
        current_command: String::new(),
        prompt: "fix the bug".into(),
        prompt_is_response: false,
        started_at: None,
        wait_reason: String::new(),
        permission_mode: tmux_agent_sidebar::tmux::PermissionMode::Default,
        subagents: vec![],
        pane_pid: None,
        worktree: WorktreeMetadata::default(),
        session_id: None,
        session_name: String::new(),
        tmux_session: String::new(),
        window_index: 0,
        window_name: String::new(),
        sidebar_spawned: false,
        bg_shell_cmd: None,
        bg_shell_pid: None,
        pane_title: String::new(),
        flagged: false,
    };
    let pane2 = PaneInfo {
        pane_id: "%2".into(),
        pane_active: false,
        status: PaneStatus::Idle,
        attention: PaneAttention::None,
        agent: AgentType::Codex,
        path: "/home/user/project".into(),
        launch_cwd: String::new(),
        current_command: String::new(),
        prompt: String::new(),
        prompt_is_response: false,
        started_at: None,
        wait_reason: String::new(),
        permission_mode: tmux_agent_sidebar::tmux::PermissionMode::Default,
        subagents: vec![],
        pane_pid: None,
        worktree: WorktreeMetadata::default(),
        session_id: None,
        session_name: String::new(),
        tmux_session: String::new(),
        window_index: 0,
        window_name: String::new(),
        sidebar_spawned: false,
        bg_shell_cmd: None,
        bg_shell_pid: None,
        pane_title: String::new(),
        flagged: false,
    };

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane1.clone(), pane2.clone()],
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", vec![pane1, pane2])];
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @"
     ≡2  ●1  ◎0  ◐0  ○1  ✕0
    ⓘ                        — ▾
    project
    ┃ ● claude
        fix the bug
      ○ codex
        Waiting for prompt…
    ");
}

#[test]
fn snapshot_two_windows_ui() {
    let pane1 = make_pane(AgentType::Claude, PaneStatus::Running);
    let mut pane2 = make_pane(AgentType::Codex, PaneStatus::Idle);
    pane2.pane_id = "%2".into();
    pane2.pane_active = false;

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![
            WindowInfo {
                window_id: "@1".into(),
                window_name: "project-a".into(),
                window_active: true,
                auto_rename: false,
                panes: vec![pane1.clone()],
            },
            WindowInfo {
                window_id: "@2".into(),
                window_name: "project-b".into(),
                window_active: false,
                auto_rename: false,
                panes: vec![pane2.clone()],
            },
        ],
    }]);
    // Two different windows → two repo groups
    let mut group1 = make_repo_group("project-a", vec![pane1]);
    group1.has_focus = true;
    let mut group2 = make_repo_group("project-b", vec![pane2]);
    group2.has_focus = false;
    state.repo_groups = vec![group1, group2];
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @"
     ≡2  ●1  ◎0  ◐0  ○1  ✕0
    ⓘ                        — ▾
    project-a
    ┃ ● claude
    project-b
      ○ codex
        Waiting for prompt…
    ");
}

#[test]
fn snapshot_multi_session_ui() {
    let pane1 = make_pane(AgentType::Claude, PaneStatus::Running);
    let mut pane2 = make_pane(AgentType::Codex, PaneStatus::Idle);
    pane2.pane_id = "%2".into();
    pane2.pane_active = false;

    let mut state = make_state(vec![
        SessionInfo {
            session_name: "main".into(),
            windows: vec![WindowInfo {
                window_id: "@1".into(),
                window_name: "dotfiles".into(),
                window_active: true,
                auto_rename: false,
                panes: vec![pane1.clone()],
            }],
        },
        SessionInfo {
            session_name: "work".into(),
            windows: vec![WindowInfo {
                window_id: "@2".into(),
                window_name: "api".into(),
                window_active: false,
                auto_rename: false,
                panes: vec![pane2.clone()],
            }],
        },
    ]);
    // Multi-session → two repo groups (sessions don't matter for rendering)
    let mut group1 = make_repo_group("dotfiles", vec![pane1]);
    group1.has_focus = true;
    let mut group2 = make_repo_group("api", vec![pane2]);
    group2.has_focus = false;
    state.repo_groups = vec![group1, group2];
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @"
     ≡2  ●1  ◎0  ◐0  ○1  ✕0
    ⓘ                        — ▾
    dotfiles
    ┃ ● claude
    api
      ○ codex
        Waiting for prompt…
    ");
}

#[test]
fn snapshot_wait_reason_ui() {
    let mut pane = make_pane(AgentType::Claude, PaneStatus::Waiting);
    pane.wait_reason = "permission_prompt".into();

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

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @"
     ≡1  ●0  ◎0  ◐1  ○0  ✕0
    ⓘ                        — ▾
    project
    ┃ ◐ claude
        permission required
    ");
}

#[test]
fn snapshot_auto_rename_window_title_ui() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Idle);

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "fish".into(),
            window_active: true,
            auto_rename: true,
            panes: vec![pane.clone()],
        }],
    }]);
    // auto_rename=true: box title comes from RepoGroup.name (path basename = "project")
    state.repo_groups = vec![make_repo_group("project", vec![pane])];
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @"
     ≡1  ●0  ◎0  ◐0  ○1  ✕0
    ⓘ                        — ▾
    project
    ┃ ○ claude
        Waiting for prompt…
    ");
}

// ─── proxy active bar (w-off cursor parked on a hidden process pane) ───

/// An on-screen non-agent pane in session "main", window 0 — the cursor
/// `compute_proxy_focus` resolves from `other_windows`.
fn process_cursor(pane_id: &str, command: &str) -> OtherWindow {
    OtherWindow {
        window_id: "@5".into(),
        window_index: 0,
        window_name: "editor".into(),
        window_active: true,
        session_name: "main".into(),
        session_attached: true,
        attention: PaneAttention::None,
        pane_id: pane_id.into(),
        pane_active: true,
        pane_pid: None,
        command: command.into(),
        command_args: None,
        path: "/home/user/project".into(),
        git_info: tmux_agent_sidebar::group::PaneGitInfo::default(),
        status: WindowStatus::Busy,
        pane_title: String::new(),
        flagged: false,
    }
}

/// Proxy focus test bed: two agents sharing window 0 with an nvim pane.
/// Returns the state with the cursor on the nvim pane id and the proxy
/// resolved to the window's first agent (what `resolve_proxy_focus`
/// computes per snapshot from these groups + buckets).
fn two_agents_plus_cursor_state(show_windows: bool) -> AppState {
    let mut claude = make_pane(AgentType::Claude, PaneStatus::Idle);
    claude.tmux_session = "main".into();
    claude.window_index = 0;
    let mut codex = make_pane(AgentType::Codex, PaneStatus::Idle);
    codex.pane_id = "%2".into();
    codex.tmux_session = "main".into();
    codex.window_index = 0;

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![],
        }],
    }]);
    state.global.show_windows = show_windows;
    state.repo_groups = vec![make_repo_group("project", vec![claude, codex])];
    // The cursor sits on the nvim pane sharing the agents' window: its
    // id matches no agent row, so the proxy is what lights the bar.
    state.focus_state.focused_pane_id = Some("%10".into());
    state.focus_state.proxy_focused_pane_id = Some("%1".into());
    state.other_windows.insert(
        "/home/user/project".into(),
        vec![process_cursor("%10", "nvim")],
    );
    state.rebuild_row_targets();
    state
}

#[test]
fn snapshot_proxy_active_bar_lights_cursor_window_agent() {
    // w off: the nvim window row is hidden, so the window's first agent
    // carries the active bar in its place; the second agent stays dark.
    let mut state = two_agents_plus_cursor_state(false);

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @"
     ≡2  ●0  ◎0  ◐0  ○2  ✕0
    ⓘ                        — ▾
    project
    ┃ ○ claude
        Waiting for prompt…
      ○ codex
        Waiting for prompt…
    ");
}

#[test]
fn snapshot_window_row_keeps_marker_when_windows_visible() {
    // w on: the nvim window row renders its own active marker; no agent
    // row may light up (the proxy stands down when window rows show).
    let mut state = two_agents_plus_cursor_state(true);

    let output = render_to_string(&mut state, 28, 25);
    insta::assert_snapshot!(output, @"
     ≡3  ●0  ◎0  ◐0  ○3  ✕0
    ⓘ                        — ▾
    project
      ○ claude
        Waiting for prompt…
      ○ codex
        Waiting for prompt…
    ┃ ◆ nvim
        main · 0:editor
    ");
}
