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
     ≡3  ●1  ◎0  ◐0  ○2  ✕0
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
fn snapshot_other_windows_visible_under_matching_status_filter() {
    // Window panes participate in the status filters: the running task
    // shows up under `running` (the "what's going on" view) while the
    // nvim editor stays out — it has no agent status to match.
    let mut state = state_with_windows(true);
    state.global.status_filter = StatusFilter::Running;
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 34, 40);
    insta::assert_snapshot!(output, @"
     ≡3  ●1  ◎0  ◐0  ○2  ✕0
    ⓘ                              — ▾
    project
      ◆ cargo test  cargo
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

// ─── finished / failed task tracking ───────────────────────────────

/// Drive a window pane through task → shell transitions via
/// `apply_session_snapshot`, the same path the live refresh uses.
fn tick_window(
    state: &mut tmux_agent_sidebar::state::AppState,
    command: &str,
    last_cmd: &str,
    last_exit: Option<i64>,
) {
    let other = tmux_agent_sidebar::tmux::OtherPane {
        session_name: "main".into(),
        window_id: "@5".into(),
        window_index: 0,
        window_name: "tests".into(),
        window_active: false,
        pane_id: "%10".into(),
        pane_active: true,
        path: "/home/user/project".into(),
        command: command.into(),
        pane_pid: None,
        last_cmd: if last_cmd.is_empty() {
            None
        } else {
            Some(last_cmd.into())
        },
        last_exit,
    };
    let sessions = vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![make_pane(AgentType::Claude, PaneStatus::Idle)],
        }],
    }];
    state.repo_groups = vec![make_repo_group(
        "project",
        vec![make_pane(AgentType::Claude, PaneStatus::Idle)],
    )];
    state.global.repo_filter = tmux_agent_sidebar::state::RepoFilter::All;
    state.apply_session_snapshot(sessions, vec![other], Vec::new());
    state.rebuild_row_targets();
}

use tmux_agent_sidebar::state::WindowFinished;

#[test]
fn window_task_finishes_and_flashes_until_focused() {
    let state = &mut tmux_agent_sidebar::state::AppState::new("%99".into());
    state.global.show_windows = true;

    // No integration installed: task observed while running.
    tick_window(state, "cargo", "", None);
    assert!(
        state
            .pane_state("%10")
            .unwrap()
            .window_task_command
            .is_some(),
        "first sight of a task starts the tracking"
    );

    // Task drops back to the shell → finished, unknown exit code.
    state.now += 20; // past TASK_MIN_RUN_SECS
    tick_window(state, "zsh", "", None);
    let runtime = state.pane_state("%10").unwrap();
    assert_eq!(
        runtime.window_finished.as_ref().map(|f| f.command.as_str()),
        Some("cargo"),
        "task → shell transition confirms the finish even without integration"
    );
    assert_eq!(runtime.window_finished.as_ref().unwrap().exit_code, None);

    // The finished pane surfaces in the running tab…
    state.global.status_filter = StatusFilter::Running;
    state.rebuild_row_targets();
    let running_rows: Vec<_> = state
        .layout
        .pane_row_targets
        .iter()
        .map(|t| (t.pane_id.as_str(), t.is_window))
        .collect();
    assert_eq!(running_rows, vec![("%10", true)]);

    // …until the pane gains focus, which consumes the flag. The flash
    // grace is expired by ageing the finish first.
    state.global.status_filter = StatusFilter::All;
    state.rebuild_row_targets();
    if let Some(finished) = state.pane_state_mut("%10").window_finished.as_mut() {
        finished.finished_at = state.now.saturating_sub(6);
    }
    state.focus_state.sidebar_focused = false;
    state.focus_state.window_active = true;
    state.focus_state.focused_pane_id = Some("%10".into());
    state.mark_focused_pane_seen(true);
    tick_window(state, "zsh", "", None);
    assert!(
        state
            .pane_state("%10")
            .is_none_or(|s| s.window_finished.is_none()),
        "focusing the pane clears the finished flag"
    );
}

#[test]
fn window_task_nonzero_exit_is_failed_and_shows_in_error_tab() {
    let state = &mut tmux_agent_sidebar::state::AppState::new("%99".into());
    state.global.show_windows = true;

    // Integration active: the shell recorded the command and its exit code.
    tick_window(state, "cargo", "cargo", Some(0));
    state.now += 20; // past TASK_MIN_RUN_SECS
    tick_window(state, "cargo", "cargo", Some(1));
    tick_window(state, "zsh", "cargo", Some(1));

    let runtime = state.pane_state("%10").unwrap();
    let finished = runtime.window_finished.clone().unwrap();
    assert_eq!(finished.command, "cargo");
    assert_eq!(finished.exit_code, Some(1));
    assert_eq!(finished.finished_at, state.now);

    state.rebuild_row_targets();
    assert_eq!(
        state.global.status_filter,
        StatusFilter::All,
        "sanity: all filter is preserved"
    );

    // Error tab shows the failed run with its pane id.
    state.global.status_filter = StatusFilter::Error;
    state.rebuild_row_targets();
    let error_rows: Vec<&str> = state
        .layout
        .pane_row_targets
        .iter()
        .map(|t| t.pane_id.as_str())
        .collect();
    assert_eq!(error_rows, vec!["%10"]);

    // The header counts it into error.
    let (_, _, _, _, _, error) = state.status_counts();
    assert_eq!(error, 1);
}

#[test]
fn window_task_chain_must_not_flash_between_commands() {
    // With shell integration present, the brief shell foreground inside
    // a `a && b` chain (the sidebar's poll catching the gap) must not
    // confirm a finish for `a`: the preexec record already names `b`.
    let state = &mut tmux_agent_sidebar::state::AppState::new("%99".into());
    state.global.show_windows = true;

    tick_window(state, "cargo", "cargo", Some(0)); // task starts (preexec wrote cargo)
    state.now += 20; // past TASK_MIN_RUN_SECS
    tick_window(state, "zsh", "cargo", Some(0)); // real finish, confirmed

    let state = &mut tmux_agent_sidebar::state::AppState::new("%99".into());
    state.global.show_windows = true;

    tick_window(state, "cargo", "cargo", Some(0)); // cargo starts
    // Poll lands in the gap before `cargo` was confirmed? No: the gap
    // between cargo && pytest — last_cmd now names pytest.
    tick_window(state, "zsh", "pytest", Some(0));
    assert!(
        state
            .pane_state("%10")
            .is_none_or(|s| s.window_finished.is_none()),
        "a mismatched recorded command must not confirm the tracked task"
    );
}

#[test]
fn window_finish_survives_focus_during_min_flash_grace() {
    // A task that finishes while its pane is already focused (the user is
    // watching it) must still flash for the minimum-flash grace before a
    // pane-focus tick consumes the flag — otherwise the row snaps to idle
    // with never a visible pulse.
    let state = &mut tmux_agent_sidebar::state::AppState::new("%99".into());
    state.global.show_windows = true;
    tick_window(state, "cargo", "cargo", Some(0));
    state.now += 20; // past TASK_MIN_RUN_SECS
    tick_window(state, "zsh", "cargo", Some(0));
    assert!(
        state
            .pane_state("%10")
            .is_some_and(|s| s.window_finished.is_some())
    );

    // Focused pane, within the grace: the flag survives.
    state.focus_state.sidebar_focused = false;
    state.focus_state.window_active = true;
    state.focus_state.focused_pane_id = Some("%10".into());
    state.mark_focused_pane_seen(true);
    assert!(
        state
            .pane_state("%10")
            .is_some_and(|s| s.window_finished.is_some()),
        "finish must survive the grace even while the pane is focused"
    );

    // Simulate the grace expiring: age the finish past the constant.
    state.pane_state_mut("%10").window_finished = Some(WindowFinished {
        command: "cargo".into(),
        exit_code: Some(0),
        finished_at: state.now.saturating_sub(6),
    });
    state.mark_focused_pane_seen(true);
    assert!(
        state
            .pane_state("%10")
            .is_none_or(|s| s.window_finished.is_none()),
        "after the grace, focusing the pane still consumes the finish flag"
    );
}

#[test]
fn window_task_under_min_runtime_ends_silently() {
    // A quick command (`ls`, and the task-classified child processes a zsh
    // prompt renderer spawns when redrawing) observed for one tick drops
    // back to the shell without ever earning the completion flash: it
    // completes while the user is still looking at the pane.
    let state = &mut tmux_agent_sidebar::state::AppState::new("%99".into());
    state.global.show_windows = true;

    tick_window(state, "ls", "", None);
    tick_window(state, "zsh", "", None);

    let runtime = state.pane_state("%10");
    assert!(
        runtime.is_none_or(|s| s.window_finished.is_none() && s.window_task_command.is_none()),
        "a sub-second task must end with no finish flag"
    );

    // The same command run long must flash, proving the gate is
    // runtime-based rather than command-specific.
    state.now += 20; // past TASK_MIN_RUN_SECS
    tick_window(state, "ls", "ls", Some(0));
    state.now += 20;
    tick_window(state, "zsh", "ls", Some(0));
    assert!(
        state
            .pane_state("%10")
            .is_some_and(|s| s.window_finished.is_some()),
        "the same command past the minimum runtime earns the flash"
    );
}

#[test]
fn window_pane_that_never_ran_a_command_is_idle_only() {
    // Plain shells must never flash: they belong to the idle tab.
    let state = &mut tmux_agent_sidebar::state::AppState::new("%99".into());
    state.global.show_windows = true;
    tick_window(state, "zsh", "", None);

    let runtime = state.pane_state("%10");
    assert!(
        runtime.is_none_or(|s| s.window_finished.is_none() && s.window_task_command.is_none()),
        "no task was ever tracked"
    );

    state.global.status_filter = StatusFilter::Idle;
    state.rebuild_row_targets();
    let idle_rows: Vec<&str> = state
        .layout
        .pane_row_targets
        .iter()
        .map(|t| t.pane_id.as_str())
        .collect();
    // The idle agent pane (%1) matches too; the window pane rides along.
    assert_eq!(idle_rows, vec!["%1", "%10"]);
}
