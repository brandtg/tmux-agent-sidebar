//! Drift detection for tmux-agent-sidebar.tmux.
//!
//! The entry script owns the agent-side auto-wiring that must re-run on
//! every plugin load: the OpenCode bridge symlink and the Antigravity
//! (agy) plugin bundle. If a block is removed or renamed, the agent's
//! hooks silently stop being wired on new installs — these tests pin the
//! blocks to the file so the removal has to be an explicit test edit.

const ENTRY_SCRIPT: &str = include_str!("../tmux-agent-sidebar.tmux");

#[test]
fn antigravity_plugin_wiring_survives_edits() {
    // The bundle is generated from `setup antigravity` (single source of
    // truth: AntigravityAdapter::HOOK_REGISTRATIONS), written into the
    // cache dir, and symlinked into agy's global plugin directory — the
    // path the upstream docs give for manual plugin installs.
    assert!(
        ENTRY_SCRIPT.contains("\"$SIDEBAR_BINARY\" setup antigravity"),
        "entry script no longer generates the agy hooks bundle via `setup \
         antigravity`; Antigravity panes will silently lose hook wiring on \
         fresh installs"
    );
    assert!(
        ENTRY_SCRIPT.contains("config/plugins"),
        "entry script no longer links the agy plugin into \
         ~/.gemini/config/plugins"
    );
    assert!(
        ENTRY_SCRIPT.contains("AGY_PLUGIN_LINK"),
        "entry script agy block lost the symlink variable"
    );
}

#[test]
fn agy_wiring_runs_before_conf_source() {
    // The wiring must execute on every plugin load, including before the
    // conf is sourced — a block appended after `tmux source-file` would
    // only run when that call returns (never on early exits).
    let wiring = ENTRY_SCRIPT
        .find("setup antigravity")
        .expect("agy wiring block missing");
    let source_file = ENTRY_SCRIPT
        .find("tmux source-file")
        .expect("entry script no longer sources agent-sidebar.conf");
    assert!(
        wiring < source_file,
        "agy wiring block must run before `tmux source-file`"
    );
}
