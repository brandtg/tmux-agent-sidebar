//! End-to-end tests for the toggle path: the real binary creates and closes
//! the sidebar split pane on a real, scoped tmux server. This is the TUI's
//! outermost integration seam — entry-point arg parsing, the tmux probe
//! chain, split-window creation, role tagging, and teardown.
//!
//! Same isolation rules as the other pipeline tests: throwaway server via
//! `TMUX_TMPDIR`, serial execution. Popups are not covered here — the
//! narrow-viewport popup path hosts a blocking TUI in `display-popup -E`,
//! which has no non-interactive invocation from a test.

use std::process::Command;

struct ScopedServer {
    _dir: tempfile::TempDir,
    window: String,
    pane: String,
}

impl Drop for ScopedServer {
    fn drop(&mut self) {
        // Same fixture-ownership rule as hook_pipeline_tests: the scoped
        // server dies with this struct, or every run leaks a server
        // process. Safe because TMUX is unset and TMUX_TMPDIR is the
        // throwaway dir.
        let _ = Command::new("tmux").args(["kill-server"]).status();
    }
}

impl ScopedServer {
    /// Wide window: above the popup threshold (default 100), so toggle takes
    /// the split-pane path.
    fn spawn() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: integration tests run under `--test-threads=1` (the
        // Makefile's test-ignored target); all tmux calls below inherit
        // this env and land on the throwaway server.
        unsafe {
            std::env::set_var("TMUX_TMPDIR", dir.path());
            std::env::remove_var("TMUX");
            std::env::remove_var("TMUX_PANE");
        }
        //
        // `-f /dev/null` matters: a tmux server sources ~/.tmux.conf at
        // startup, TPM plugins included, so a "scoped" server otherwise
        // inherits the user's whole tmux environment (hooks, plugin
        // binaries, @sidebar_* settings). With an empty config the test
        // seeds exactly the options it needs — deterministic on this
        // machine and on a CI runner with no ~/.tmux.conf at all.
        let status = Command::new("tmux")
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "togglee2e",
                "-x",
                "160",
                "-y",
                "40",
            ])
            .status()
            .expect("tmux new-session");
        assert!(status.success(), "tmux new-session failed");
        // Seed the global settings the toggle path probes, mirroring the
        // plugin conf's defaults (@sidebar_auto_create is user-settable;
        // the conf seeds "on" when unset).
        for (key, value) in [
            ("@sidebar_auto_create", "on"),
            ("@sidebar_width", "44"),
            ("@sidebar_position", "left"),
            ("@sidebar_popup_max_width", "100"),
        ] {
            let status = Command::new("tmux")
                .args(["set", "-g", key, value])
                .status()
                .expect("tmux set");
            assert!(status.success(), "seeding {key} failed");
        }
        let window = Command::new("tmux")
            .args(["list-windows", "-a", "-F", "#{window_id}"])
            .output()
            .unwrap();
        let window = String::from_utf8_lossy(&window.stdout).trim().to_string();
        assert!(window.starts_with('@'), "unexpected window id: {window:?}");
        let pane = Command::new("tmux")
            .args(["list-panes", "-a", "-F", "#{pane_id}"])
            .output()
            .unwrap();
        let pane = String::from_utf8_lossy(&pane.stdout).trim().to_string();
        Self {
            _dir: dir,
            window,
            pane,
        }
    }

    fn pane_count(&self) -> usize {
        let output = Command::new("tmux")
            .args(["list-panes", "-t", &self.window, "-F", "#{pane_id}"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).lines().count()
    }

    fn sidebar_pane(&self) -> Option<String> {
        let output = Command::new("tmux")
            .args([
                "list-panes",
                "-t",
                &self.window,
                "-F",
                "#{pane_id}|#{@pane_role}",
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find(|line| line.ends_with("|sidebar"))
            .map(|line| line.trim_end_matches("|sidebar").to_string())
    }
}

fn run_toggle(args: &[&str]) -> i32 {
    let status = Command::new(env!("CARGO_BIN_EXE_tmux-agent-sidebar"))
        .args(args)
        .env(
            "TMUX_TMPDIR",
            std::env::var("TMUX_TMPDIR").unwrap_or_default(),
        )
        .status()
        .expect("run binary");
    status.code().unwrap_or(-1)
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn toggle_creates_and_closes_the_sidebar_split() {
    let server = ScopedServer::spawn();
    let pane_path = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            &server.pane,
            "#{pane_current_path}",
        ])
        .output()
        .unwrap();
    let pane_path = String::from_utf8_lossy(&pane_path.stdout)
        .trim()
        .to_string();

    let code = run_toggle(&["toggle", &server.window, &pane_path, "200"]);
    assert_eq!(code, 0, "toggle create must exit 0");
    assert_eq!(server.pane_count(), 2, "sidebar split must appear");
    let sidebar = server
        .sidebar_pane()
        .expect("a pane tagged @pane_role=sidebar");
    assert_ne!(sidebar, server.pane, "the sidebar must be its own pane");

    let code = run_toggle(&["toggle", &server.window, &pane_path, "200"]);
    assert_eq!(code, 0, "toggle close must exit 0");
    assert_eq!(server.pane_count(), 1, "sidebar split must be gone");
    assert!(
        server.sidebar_pane().is_none(),
        "role tag must leave with the pane"
    );
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn toggle_create_only_renders_without_opening() {
    let server = ScopedServer::spawn();
    let pane_path = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            &server.pane,
            "#{pane_current_path}",
        ])
        .output()
        .unwrap();
    let pane_path = String::from_utf8_lossy(&pane_path.stdout)
        .trim()
        .to_string();

    // The auto-create hook path (--create-only) must not steal focus from
    // the pane the user is working in: after creation, the original pane
    // is still the active one in its window.
    let code = run_toggle(&["toggle", "--create-only", &server.window, &pane_path]);
    assert_eq!(code, 0, "--create-only must exit 0");
    let sidebar = server.sidebar_pane().expect("sidebar created");
    let active = Command::new("tmux")
        .args(["display-message", "-p", "-t", &server.window, "#{pane_id}"])
        .output()
        .unwrap();
    let active = String::from_utf8_lossy(&active.stdout).trim().to_string();
    assert_eq!(
        active, server.pane,
        "--create-only must leave focus on the original pane"
    );
    assert_ne!(sidebar, server.pane);
}
