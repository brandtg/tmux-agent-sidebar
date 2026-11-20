//! End-to-end tests for the hook pipeline: a real `hook.sh` (staged into a
//! fake plugin dir next to the real binary, so its own binary-resolution
//! logic runs) receives agent payloads and writes pane options through a
//! real, scoped tmux server.
//!
//! Isolation follows the same pattern as capture_tests: a throwaway server
//! selected via `TMUX_TMPDIR`, killed by the fixture's Drop impl. Run
//! single-threaded
//! (`--test-threads=1`) because the tests mutate process-global env state.

use std::io::Write;
use std::process::{Command, Stdio};

/// One scoped server per test, with one pane whose id the test knows.
/// Drop kills the server while TMUX_TMPDIR still points at the throwaway
/// dir (Drop::drop runs before the fields vanish).
struct ScopedServer {
    _dir: tempfile::TempDir,
    pane: String,
}

impl Drop for ScopedServer {
    fn drop(&mut self) {
        // The fixture owns the server's whole lifecycle: kill it on drop or
        // every test run leaks a tmux server process (learned the hard way —
        // ten leaked `hookpipe` servers were found and killed by exact pid
        // after the first version of this file skipped this). `kill-server`
        // here is safe: TMUX is unset and TMUX_TMPDIR points at the
        // throwaway dir, so the command can only reach this server.
        let _ = Command::new("tmux").args(["kill-server"]).status();
    }
}

impl ScopedServer {
    fn spawn() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: these #[ignore]-style integration tests run under
        // `--test-threads=1` (see the Makefile's test-ignored target); all
        // tmux invocations below inherit this env so they land on the
        // throwaway server, never the user's live one.
        unsafe {
            std::env::set_var("TMUX_TMPDIR", dir.path());
            std::env::remove_var("TMUX");
            std::env::remove_var("TMUX_PANE");
        }
        let status = Command::new("tmux")
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "hookpipe",
                "-x",
                "80",
                "-y",
                "24",
            ])
            .status()
            .expect("tmux new-session");
        assert!(status.success(), "tmux new-session failed");
        let output = Command::new("tmux")
            .args(["list-panes", "-a", "-F", "#{pane_id}"])
            .output()
            .expect("tmux list-panes");
        let pane = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert!(pane.starts_with('%'), "unexpected pane id: {pane:?}");
        Self { _dir: dir, pane }
    }
}

/// Stage a plugin dir: `bin/tmux-agent-sidebar` (the real test binary) plus
/// `hook.sh`, exactly the layout hook.sh's own resolution prefers.
fn stage_plugin_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir(&bin_dir).unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_tmux-agent-sidebar"),
        bin_dir.join("tmux-agent-sidebar"),
    )
    .unwrap();
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/hook.sh"),
        dir.path().join("hook.sh"),
    )
    .unwrap();
    dir
}

/// Run the staged hook.sh: `hook.sh <agent> <event>` with `payload` on
/// stdin and `TMUX_PANE` targeting `pane` on the scoped server. Returns
/// (exit code, combined output).
fn run_hook(
    plugin_dir: &std::path::Path,
    pane: &str,
    agent: &str,
    event: &str,
    payload: &str,
) -> (i32, String) {
    let mut child = Command::new("bash")
        .arg(plugin_dir.join("hook.sh"))
        .args([agent, event])
        .env("TMUX_PANE", pane)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hook.sh");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

/// Run the staged hook.sh without `TMUX_PANE` — the environment a
/// Claude `bg-pty-host` continuation fork or async subagent host gives
/// its hooks. The scoped server is still reached via the inherited
/// `TMUX_TMPDIR`.
fn run_hook_paneless(
    plugin_dir: &std::path::Path,
    agent: &str,
    event: &str,
    payload: &str,
) -> (i32, String) {
    let mut child = Command::new("bash")
        .arg(plugin_dir.join("hook.sh"))
        .args([agent, event])
        .env_remove("TMUX_PANE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hook.sh");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

/// Seed a pane option on the scoped server.
fn set_pane_option(pane: &str, option: &str, value: &str) {
    let status = Command::new("tmux")
        .args(["set-option", "-p", "-t", pane, option, value])
        .status()
        .expect("tmux set-option");
    assert!(status.success(), "tmux set-option {option} failed");
}

/// Read a pane option from the scoped server.
fn pane_option(pane: &str, option: &str) -> String {
    let output = Command::new("tmux")
        .args(["show", "-p", "-v", "-t", pane, option])
        .output()
        .expect("tmux show");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn hook_sh_claude_session_start_writes_agent_identity() {
    let server = ScopedServer::spawn();
    let plugin = stage_plugin_dir();

    let payload = r#"{"session_id":"e2e-claude","transcript_path":"/tmp/none.jsonl","hook_event_name":"SessionStart","source":"startup"}"#;
    // hooks.json maps Claude's upstream "SessionStart" trigger to the CLI
    // event name "session-start" — this test drives the real contract.
    let (code, out) = run_hook(
        plugin.path(),
        &server.pane,
        "claude",
        "session-start",
        payload,
    );
    assert_eq!(code, 0, "hook.sh must always exit 0: {out}");

    assert_eq!(pane_option(&server.pane, "@pane_agent"), "claude");
    assert_eq!(pane_option(&server.pane, "@pane_session_id"), "e2e-claude");
    assert!(
        !pane_option(&server.pane, "@pane_status").is_empty(),
        "SessionStart must set a status"
    );
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn hook_sh_claude_user_prompt_submit_records_prompt() {
    let server = ScopedServer::spawn();
    let plugin = stage_plugin_dir();

    let (code, out) = run_hook(
        plugin.path(),
        &server.pane,
        "claude",
        "user-prompt-submit",
        r#"{"session_id":"e2e-claude","prompt":"list the failing tests"}"#,
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(pane_option(&server.pane, "@pane_agent"), "claude");
    let status = pane_option(&server.pane, "@pane_status");
    assert!(
        !status.is_empty() && status != "idle",
        "a submitted prompt must move the pane out of idle, got {status:?}"
    );
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn hook_sh_codex_session_start_writes_agent_identity() {
    let server = ScopedServer::spawn();
    let plugin = stage_plugin_dir();

    let (code, out) = run_hook(
        plugin.path(),
        &server.pane,
        "codex",
        "session-start",
        r#"{"cwd":"/tmp","session_id":"e2e-codex"}"#,
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(pane_option(&server.pane, "@pane_agent"), "codex");
    assert_eq!(pane_option(&server.pane, "@pane_session_id"), "e2e-codex");
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn hook_sh_opencode_session_start_writes_agent_identity() {
    let server = ScopedServer::spawn();
    let plugin = stage_plugin_dir();

    let (code, out) = run_hook(
        plugin.path(),
        &server.pane,
        "opencode",
        "session-start",
        r#"{"session_id":"e2e-oc","source":"startup"}"#,
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(pane_option(&server.pane, "@pane_agent"), "opencode");
    assert_eq!(pane_option(&server.pane, "@pane_session_id"), "e2e-oc");
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn hook_sh_drops_malformed_payloads_without_touching_the_pane() {
    let server = ScopedServer::spawn();
    let plugin = stage_plugin_dir();

    let (code, out) = run_hook(
        plugin.path(),
        &server.pane,
        "claude",
        "SessionStart",
        "{\"session_id\": truncated",
    );
    assert_eq!(code, 0, "malformed payloads must still exit 0: {out}");
    assert!(
        pane_option(&server.pane, "@pane_agent").is_empty(),
        "a dropped event must not write agent identity"
    );
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn hook_sh_exits_silently_without_tmux_pane() {
    let plugin = stage_plugin_dir();
    // The scoped server exists (its env is set), but the hook fire carries
    // no TMUX_PANE — exactly what happens when a hook escapes its pane.
    let output = Command::new("bash")
        .arg(plugin.path().join("hook.sh"))
        .args(["claude", "SessionStart"])
        .env_remove("TMUX_PANE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
        .wait_with_output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stdout.is_empty() && output.stderr.is_empty(),
        "a pane-less hook must be silent: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn hook_sh_routes_paneless_event_via_session_binding() {
    // Claude's bg-pty-host continuation forks and async subagent hosts
    // scrub TMUX_PANE from their hooks' environment. A payload whose
    // session id matches the pane's `@pane_session_id` binding must
    // still reach the pane instead of being dropped.
    let server = ScopedServer::spawn();
    let plugin = stage_plugin_dir();
    set_pane_option(&server.pane, "@pane_agent", "claude");
    set_pane_option(&server.pane, "@pane_session_id", "e2e-fork");
    set_pane_option(&server.pane, "@pane_status", "waiting");

    let (code, out) = run_hook_paneless(
        plugin.path(),
        "claude",
        "tool-start",
        r#"{"session_id":"e2e-fork","tool_name":"Bash","tool_input":{"command":"ls"}}"#,
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        pane_option(&server.pane, "@pane_status"),
        "running",
        "a pane-less tool event routed by session binding must promote the pane"
    );
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn hook_sh_routes_paneless_subagent_event_without_rebinding() {
    // A subagent host's session id is registered on the parent pane's
    // `@pane_subagents` list. Events from the child must route to the
    // parent pane, and the route is a guess: the parent's session
    // binding must survive untouched.
    let server = ScopedServer::spawn();
    let plugin = stage_plugin_dir();
    set_pane_option(&server.pane, "@pane_agent", "claude");
    set_pane_option(&server.pane, "@pane_session_id", "e2e-parent");
    set_pane_option(&server.pane, "@pane_subagents", ",Explore:child-1");
    set_pane_option(&server.pane, "@pane_status", "waiting");

    let (code, out) = run_hook_paneless(
        plugin.path(),
        "claude",
        "tool-start",
        r#"{"session_id":"child-1","tool_name":"Bash","tool_input":{"command":"ls"}}"#,
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        pane_option(&server.pane, "@pane_status"),
        "running",
        "a pane-less subagent event must promote the parent pane"
    );
    assert_eq!(
        pane_option(&server.pane, "@pane_session_id"),
        "e2e-parent",
        "a guessed subagent route must not rewrite the parent's session binding"
    );
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn hook_sh_drops_paneless_event_that_matches_no_pane() {
    // Fail-closed: a session id no pane claims (and no subagent entry
    // registers) must not land on an arbitrary pane.
    let server = ScopedServer::spawn();
    let plugin = stage_plugin_dir();
    set_pane_option(&server.pane, "@pane_agent", "claude");
    set_pane_option(&server.pane, "@pane_session_id", "e2e-bound");

    let (code, out) = run_hook_paneless(
        plugin.path(),
        "claude",
        "tool-start",
        r#"{"session_id":"e2e-stranger","tool_name":"Bash","tool_input":{"command":"ls"}}"#,
    );
    assert_eq!(code, 0, "a dropped event must still exit 0: {out}");
    assert!(
        pane_option(&server.pane, "@pane_status").is_empty(),
        "an unmatched pane-less event must not touch the pane"
    );
}
