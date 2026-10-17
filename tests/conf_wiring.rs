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
fn maintain_width_is_registered_for_both_resize_events() {
    // Both events carry the same idempotent command; the registrations
    // must stay inside `if-shell` guards (window-layout-changed needs
    // tmux 3.2+, and an unguarded `set-hook` would error while sourcing
    // on older servers).
    let mut per_hook: BTreeMap<&str, Vec<&'static str>> = BTreeMap::new();
    for hook in ["window-layout-changed", "client-resized"] {
        let needle = format!(
            "set-hook -ga {hook} \"run-shell -b \\\"#{{@agent_sidebar_bin}}\\\" maintain-width \\\"#{{window_id}}\\\"\""
        );
        let lines = conf_lines_containing(&needle);
        assert_eq!(
            lines.len(),
            1,
            "expected exactly one {hook} registration for maintain-width, found {}: {lines:?}",
            lines.len()
        );
        assert!(
            lines[0].trim_start().starts_with("if-shell"),
            "{hook} maintain-width registration must stay guarded by an \
             if-shell tmux version check so sourcing never errors on \
             tmux < 3.2"
        );
        per_hook.insert(hook, lines);
    }
}

#[test]
fn maintain_width_registrations_have_matching_dedup_lines() {
    // The dedup block removes exactly our entries before re-registering;
    // a missing line means duplicate hooks accumulate on every reload.
    for hook in ["window-layout-changed", "client-resized"] {
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
