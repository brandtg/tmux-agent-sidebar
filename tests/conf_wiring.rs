//! Drift detection between `agent-sidebar.conf` and the binary's own
//! subcommands.
//!
//! The conf registers tmux hooks that invoke CLI subcommands and removes
//! its own entries on every reload (the dedup block). If a registration
//! is renamed or removed without updating the dedup block — or the
//! subcommand it calls disappears from the dispatch — reloads accumulate
//! duplicate hooks (the exact bug the dedup block exists to prevent) or
//! hooks go silently dead. These tests pin the pairs together.

use std::collections::BTreeMap;

const CONF: &str = include_str!("../agent-sidebar.conf");
const CLI_DISPATCH: &str = include_str!("../src/cli/mod.rs");

/// Lines of the conf containing `needle`.
fn conf_lines_containing(needle: &str) -> Vec<&'static str> {
    CONF.lines().filter(|l| l.contains(needle)).collect()
}

#[test]
fn maintain_width_is_dispatched_by_the_binary() {
    // The conf calls `maintain-width`; if the subcommand is renamed the
    // hooks would keep invoking a dead command name.
    assert!(
        CLI_DISPATCH.contains("\"maintain-width\" => toggle::cmd_maintain_width(rest)"),
        "src/cli/mod.rs no longer dispatches `maintain-width`; update \
         agent-sidebar.conf to the new name"
    );
}

#[test]
fn maintain_width_is_registered_for_client_and_layout_events() {
    // All four events carry the same idempotent command, forwarding the
    // triggering client's width as the second argument; the
    // window-layout-changed / client-resized registrations must stay
    // inside `if-shell` guards (window-layout-changed needs tmux 3.2+,
    // and an unguarded `set-hook` would error while sourcing on older
    // servers). client-session-changed and after-select-window exist in
    // every supported tmux and are registered unguarded.
    //
    // The quoting is load-bearing: at fire time tmux re-parses the stored
    // command, so the `\"` span must stay open across `maintain-width` and
    // close only after `#{client_width}` — that way `run-shell` receives
    // the whole invocation as ONE argument. Closing the span after the
    // binary (`\"#{@agent_sidebar_bin}\"`) splits the hook into separate
    // arguments; tmux's run-shell then executes argv[0] (the bare binary,
    // headless TUI) which exits 1 on every layout change.
    let mut per_hook: BTreeMap<&str, Vec<&'static str>> = BTreeMap::new();
    for (hook, guarded) in [
        ("window-layout-changed", true),
        ("client-resized", true),
        ("client-session-changed", false),
        ("after-select-window", false),
    ] {
        let needle = format!(
            "set-hook -ga {hook} \"run-shell -b \\\"#{{@agent_sidebar_bin}} maintain-width #{{window_id}} #{{client_width}}\\\"\""
        );
        let lines = conf_lines_containing(&needle);
        assert_eq!(
            lines.len(),
            1,
            "expected exactly one {hook} registration for maintain-width, found {}: {lines:?}",
            lines.len()
        );
        if guarded {
            assert!(
                lines[0].trim_start().starts_with("if-shell"),
                "{hook} maintain-width registration must stay guarded by an \
                 if-shell tmux version check so sourcing never errors on \
                 tmux < 3.2"
            );
        } else {
            assert!(
                !lines[0].trim_start().starts_with("if-shell"),
                "{hook} exists in every supported tmux; no version guard \
                 expected on its maintain-width registration"
            );
        }
        per_hook.insert(hook, lines);
    }
}

#[test]
fn maintain_width_registrations_have_matching_dedup_lines() {
    // The dedup block removes exactly our entries before re-registering;
    // a missing line means duplicate hooks accumulate on every reload.
    for hook in [
        "window-layout-changed",
        "client-resized",
        "client-session-changed",
        "after-select-window",
    ] {
        let needle = format!(
            "show-hooks -g {hook} | awk \"/@agent_sidebar_bin/ && /maintain-width/ {{ print \\$1 }}\""
        );
        assert_eq!(
            conf_lines_containing(&needle).len(),
            1,
            "expected exactly one {hook} dedup line for maintain-width"
        );
    }
}

#[test]
fn sidebar_auto_hide_is_seeded_to_on() {
    // `maintain-width` treats an unset option as on, but seeding it keeps
    // the default visible via `tmux show -g @sidebar_auto_hide`.
    assert!(
        CONF.contains("if -F '#{==:#{@sidebar_auto_hide},}' 'set -g @sidebar_auto_hide on'"),
        "agent-sidebar.conf must seed @sidebar_auto_hide on"
    );
}

#[test]
fn sidebar_session_windows_is_seeded_to_off() {
    // The session-mode window level defaults to off; seeding it keeps the
    // default visible via `tmux show -g @sidebar_session_windows` (and
    // `apply_all` treats unset as off regardless).
    assert!(
        CONF.contains(
            "if -F '#{==:#{@sidebar_session_windows},}' 'set -g @sidebar_session_windows off'"
        ),
        "agent-sidebar.conf must seed @sidebar_session_windows off"
    );
}

#[test]
fn sidebar_worktree_windows_is_seeded_to_off() {
    // The repo-mode window level mirrors `session_windows`: defaults to
    // off, seeded so the default is visible via
    // `tmux show -g @sidebar_worktree_windows` (and `apply_all` treats
    // unset as off regardless).
    assert!(
        CONF.contains(
            "if -F '#{==:#{@sidebar_worktree_windows},}' 'set -g @sidebar_worktree_windows off'"
        ),
        "agent-sidebar.conf must seed @sidebar_worktree_windows off"
    );
}
