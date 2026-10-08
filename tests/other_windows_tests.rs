#[allow(dead_code, unused_imports)]
mod test_helpers;

use test_helpers::*;
use tmux_agent_sidebar::group::{OtherWindow, PaneGitInfo};
use tmux_agent_sidebar::state::StatusFilter;
use tmux_agent_sidebar::tmux::{AgentType, PaneStatus, SessionInfo, WindowInfo, WindowStatus};

fn other_window(pane_id: &str, name: &str, command: &str, status: WindowStatus) -> OtherWindow {
    OtherWindow {
        window_id: "@5".into(),
        window_index: 0,
        window_name: name.into(),
        window_active: false,
        session_name: "main".into(),
        pane_id: pane_id.into(),
        pane_active: true,
        pane_pid: None,
        command: command.into(),
        path: "/home/user/project".into(),
        git_info: PaneGitInfo::default(),
        status,
    }
}

fn state_with_windows(show: bool) -> tmux_agent_sidebar::state::AppState {
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
    state.global.show_windows = show;
    state.other_windows.insert(
        "/home/user/project".into(),
        vec![
            other_window("%10", "cargo test", "cargo", WindowStatus::Task),
            other_window("%11", "nvim", "nvim", WindowStatus::Busy),
        ],
    );
    state.rebuild_row_targets();
    state
}

#[test]
fn snapshot_other_windows_rendered() {
    let mut state = state_with_windows(true);

    let output = render_to_string(&mut state, 34, 40);
    insta::assert_snapshot!(output, @"
     ≡1  ●0  ◎0  ◐0  ○1  ✕0
    ⓘ                              — ▾
    project
    ┃ ○ claude
        Waiting for prompt…
      ◆ cargo test  cargo
      ◆ nvim
    ╭ Activity │ Git ────────────────╮
    │         No activity yet        │
    ╰────────────────────────────────╯
    ");
}

#[test]
fn snapshot_other_windows_hidden_when_off() {
    let mut state = state_with_windows(false);

    let output = render_to_string(&mut state, 34, 40);
    insta::assert_snapshot!(output, @"
     ≡1  ●0  ◎0  ◐0  ○1  ✕0
    ⓘ                              — ▾
    project
    ┃ ○ claude
        Waiting for prompt…
    ╭ Activity │ Git ────────────────╮
    │         No activity yet        │
    ╰────────────────────────────────╯
    ");
}

#[test]
fn snapshot_other_windows_hidden_under_status_filter() {
    let mut state = state_with_windows(true);
    state.global.status_filter = StatusFilter::Running;
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 34, 40);
    insta::assert_snapshot!(output, @"
     ≡1  ●0  ◎0  ◐0  ○1  ✕0
    ⓘ                              — ▾
    ╭ Activity │ Git ────────────────╮
    │         No activity yet        │
    ╰────────────────────────────────╯
    ");
}

#[test]
fn window_rows_participate_in_selection_targets() {
    let mut state = state_with_windows(true);
    // Rendering populates `line_to_row`; targets were built by
    // `state_with_windows` via `rebuild_row_targets`.
    let _ = render_to_string(&mut state, 34, 40);

    let targets = &state.layout.pane_row_targets;
    assert_eq!(targets.len(), 3, "one agent pane plus two windows");
    assert!(!targets[0].is_window, "agent row comes first");
    assert!(targets[1].is_window, "window rows follow the agent row");
    assert!(targets[2].is_window);
    assert_eq!(targets[1].pane_id, "%10");
    assert_eq!(targets[2].pane_id, "%11");

    // Every selectable row must actually render, and the render order must
    // line up with the target order (the two are built by separate code
    // paths that have to agree).
    let mut seen = vec![false; targets.len()];
    for mapping in &state.layout.line_to_row {
        if let Some(row) = mapping {
            assert!(*row < targets.len(), "line maps to an out-of-range row");
            seen[*row] = true;
        }
    }
    assert!(
        seen.iter().all(|&hit| hit),
        "every selection target must be rendered: {seen:?}"
    );
}

#[test]
fn activating_a_window_row_sets_focus_without_a_pane_info() {
    let mut state = state_with_windows(true);
    state.global.selected_pane_row = 1; // first window row

    assert!(
        state.selected_pane().is_none(),
        "a window row has no agent PaneInfo"
    );

    state.activate_selected_pane();

    assert_eq!(
        state.focus_state.focused_pane_id.as_deref(),
        Some("%10"),
        "jumping to a window focuses its active pane"
    );
}
