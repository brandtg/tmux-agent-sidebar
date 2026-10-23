#[allow(dead_code, unused_imports)]
mod test_helpers;

use test_helpers::*;
use tmux_agent_sidebar::activity::FeedKind;
use tmux_agent_sidebar::group::PaneGitInfo;
use tmux_agent_sidebar::state::{BottomTab, FeedEntry, Focus};
use tmux_agent_sidebar::tmux::{AgentType, PaneStatus, SessionInfo, WindowInfo};

fn feed_entry(ts: &str, agent: AgentType, kind: FeedKind, label: &str) -> FeedEntry {
    // `epoch: 0` — these tests set `feed.entries` directly and never run
    // the merge sort; the display string is what the snapshots see.
    FeedEntry {
        timestamp: ts.into(),
        epoch: 0,
        agent,
        kind,
        label: label.into(),
    }
}

// ─── Bottom Tab Tests ──────────────────────────────────────────────

#[test]
fn test_next_bottom_tab() {
    let mut state = make_state(vec![]);
    assert_eq!(state.bottom_tab, BottomTab::Feed);
    state.next_bottom_tab();
    assert_eq!(state.bottom_tab, BottomTab::Repos);
    state.next_bottom_tab();
    assert_eq!(state.bottom_tab, BottomTab::Feed);
}

#[test]
fn test_scroll_bottom_dispatches() {
    let mut state = make_state(vec![]);

    state.feed.entries = vec![feed_entry(
        "10:01",
        AgentType::Claude,
        FeedKind::FileChange,
        "b",
    )];
    state.feed.scroll.total_lines = 6;
    state.feed.scroll.visible_height = 4;

    state.scrolls.repos.total_lines = 3;
    state.scrolls.repos.visible_height = 1;

    // Feed tab: scroll should affect the feed
    state.bottom_tab = BottomTab::Feed;
    state.scroll_bottom(1);
    assert_eq!(state.feed.scroll.offset, 1);
    assert_eq!(state.scrolls.repos.offset, 0);

    // Repos tab: scroll should affect repos
    state.bottom_tab = BottomTab::Repos;
    state.scroll_bottom(1);
    assert_eq!(state.scrolls.repos.offset, 1);
    assert_eq!(state.feed.scroll.offset, 1); // unchanged
}

// ─── Feed Tab ───────────────────────────────────────────────────────

#[test]
fn snapshot_feed_tab_ui() {
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

    state.bottom_tab = BottomTab::Feed;
    state.focus_state.focus = Focus::BottomPanel;
    state.focus_state.sidebar_focused = true;
    state.feed.entries = vec![
        feed_entry(
            "14:35",
            AgentType::Codex,
            FeedKind::Permission,
            "needs permission: Bash",
        ),
        feed_entry(
            "14:32",
            AgentType::Claude,
            FeedKind::FileChange,
            "src/ui/bottom/feed.rs",
        ),
        feed_entry(
            "14:30",
            AgentType::Claude,
            FeedKind::Task,
            "#2 Fix flaky test",
        ),
    ];

    let output = render_to_string(&mut state, 40, 24);
    insta::assert_snapshot!(output, @"
     ≡1  ●1  ◎0  ◐0  ○0  ✕0
    ⓘ                                    — ▾
    project
    ╭ Feed ·1 │ Repos ─────────────────────╮
    │14:35  codex    ⚠ needs permission:   │
    │Bash                                  │
    │14:32  claude   ✎ src/ui/bottom/feed.r│
    │s                                     │
    │14:30  claude   + #2 Fix flaky test   │
    ╰──────────────────────────────────────╯
    " );
}

#[test]
fn snapshot_feed_tab_shows_unseen_badge() {
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

    state.bottom_tab = BottomTab::Feed;
    state.focus_state.focus = Focus::BottomPanel;
    state.focus_state.sidebar_focused = true;
    // Two attention events unseen → " Feed ·2" in the title row.
    state.feed.entries = vec![
        feed_entry("14:35", AgentType::Codex, FeedKind::Failure, "exit 1"),
        feed_entry(
            "14:34",
            AgentType::Claude,
            FeedKind::Notification,
            "waiting for input",
        ),
    ];

    let output = render_to_string(&mut state, 40, 14);
    insta::assert_snapshot!(output, @r" ≡1  ●1  ◎0  ◐0  ○0  ✕0
╭ Feed ·2 │ Repos ─────────────────────╮
│14:35  codex    ✗ exit 1              │
│14:34  claude   ✳ waiting for input   │
╰──────────────────────────────────────╯");
}

#[test]
fn snapshot_feed_empty_centered_ui() {
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
    state.bottom_tab = BottomTab::Feed;
    // No entries — should show centered "No recent activity"

    let output = render_to_string(&mut state, 28, 26);
    insta::assert_snapshot!(output, @r" ≡1  ●0  ◎0  ◐0  ○1  ✕0
ⓘ                        — ▾
project
┃ ○ claude
    Waiting for prompt…
╭ Feed │ Repos ────────────╮
│    No recent activity    │
╰──────────────────────────╯");
}

// ─── Repos Tab ──────────────────────────────────────────────────────

#[test]
fn snapshot_repos_tab_ui() {
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
        name: "dotfiles".into(),
        has_focus: true,
        panes: vec![(
            pane,
            PaneGitInfo {
                repo_root: Some("/home/user/dotfiles".into()),
                branch: Some("main".into()),
                is_worktree: false,
                worktree_name: None,
                worktree_root: None,
                dirty_count: 3,
                ahead_behind: Some((2, 1)),
            },
        )],
    }];
    state.rebuild_row_targets();

    state.bottom_tab = BottomTab::Repos;
    state.focus_state.focus = Focus::BottomPanel;
    state.focus_state.sidebar_focused = true;

    let output = render_to_string(&mut state, 40, 24);
    insta::assert_snapshot!(output, @"
     ≡1  ●1  ◎0  ◐0  ○0  ✕0
    ⓘ                                    — ▾
    dotfiles                               +
    ╭ Feed │ Repos ────────────────────────╮
    │dotfiles                              │
    │claude   main                 ↑2 ↓1 ~3│
    ╰──────────────────────────────────────╯
    ");
}

#[test]
fn snapshot_repos_clean_row_has_no_right_cluster() {
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
            PaneGitInfo {
                repo_root: Some("/home/user/project".into()),
                branch: Some("feature/feed".into()),
                is_worktree: false,
                worktree_name: None,
                worktree_root: None,
                dirty_count: 0,
                ahead_behind: None,
            },
        )],
    }];
    state.rebuild_row_targets();

    state.bottom_tab = BottomTab::Repos;
    state.focus_state.focus = Focus::BottomPanel;
    state.focus_state.sidebar_focused = true;

    let output = render_to_string(&mut state, 40, 14);
    insta::assert_snapshot!(output, @"
     ≡1  ●1  ◎0  ◐0  ○0  ✕0
    ╭ Feed │ Repos ────────────────────────╮
    │project                               │
    │claude   feature/feed                 │
    ╰──────────────────────────────────────╯
    ");
}

#[test]
fn snapshot_repos_long_branch_flows_with_inline_stats() {
    // A branch too long to share the row with the stats wraps across the
    // full panel width; the stats ride inline after the final chunk.
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
            PaneGitInfo {
                repo_root: Some("/home/user/project".into()),
                branch: Some("feature/sidebar/really-long-branch".into()),
                is_worktree: false,
                worktree_name: None,
                worktree_root: None,
                dirty_count: 3,
                ahead_behind: Some((2, 1)),
            },
        )],
    }];
    state.rebuild_row_targets();

    state.bottom_tab = BottomTab::Repos;
    state.focus_state.focus = Focus::BottomPanel;
    state.focus_state.sidebar_focused = true;

    let output = render_to_string(&mut state, 40, 24);
    insta::assert_snapshot!(output, @"
     ≡1  ●1  ◎0  ◐0  ○0  ✕0
    ⓘ                                    — ▾
    project                                +
    ╭ Feed │ Repos ────────────────────────╮
    │project                               │
    │claude   feature/sidebar/really-long-b│
    │ranch ↑2 ↓1 ~3                        │
    ╰──────────────────────────────────────╯
    ");
}

#[test]
fn snapshot_repos_empty_centered_ui() {
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
    state.bottom_tab = BottomTab::Repos;
    // Pane has no resolvable git metadata → no rows → centered empty state.

    let output = render_to_string(&mut state, 28, 26);
    insta::assert_snapshot!(output, @r" ≡1  ●0  ◎0  ◐0  ○1  ✕0
ⓘ                        — ▾
project
┃ ○ claude
    Waiting for prompt…
╭ Feed │ Repos ────────────╮
│     No agents running    │
╰──────────────────────────╯");
}

// ─── Panel chrome ───────────────────────────────────────────────────

#[test]
fn snapshot_bottom_panel_minimized_shows_only_header() {
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

    state.bottom_tab = BottomTab::Feed;
    state.focus_state.focus = Focus::BottomPanel;
    state.focus_state.sidebar_focused = true;
    state.feed.entries = vec![feed_entry(
        "14:35",
        AgentType::Codex,
        FeedKind::Failure,
        "exit 1",
    )];
    state.bottom_minimized = true;

    let output = render_to_string(&mut state, 40, 24);
    insta::assert_snapshot!(output, @r" ≡1  ●1  ◎0  ◐0  ○0  ✕0
ⓘ                                    — ▾
project
┃ ● claude
╭ Feed ·1 │ Repos ─────────────────────╮");
}

#[test]
fn snapshot_tab_bar_renders_both_labels() {
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

    state.feed.entries = vec![feed_entry(
        "10:32",
        AgentType::Claude,
        FeedKind::FileChange,
        "test",
    )];

    let output = render_to_string(&mut state, 40, 14);
    insta::assert_snapshot!(output, @"
     ≡1  ●0  ◎0  ◐0  ○1  ✕0
    ╭ Feed │ Repos ────────────────────────╮
    │10:32  claude   ✎ test                │
    ╰──────────────────────────────────────╯
    ");
}

#[test]
fn test_pet_enabled_preserves_bottom_panel_border() {
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
    state.pet_enabled = true;

    insta::assert_snapshot!(render_to_string(&mut state, 40, 30), @"
     ≡1  ●0  ◎0  ◐0  ○1  ✕0
    ⓘ                                    — ▾
    project
    ┃ ○ claude
        Waiting for prompt…
      ▄ ▄
     ▄▀▀▀▄                             ████
      ▀ ▀                           ██ █  █
    ╭ Feed │ Repos ────────────────────────╮
    │          No recent activity          │
    ╰──────────────────────────────────────╯
    ");
}
