//! End-to-end tests for the sidebar-aware builtin layouts: the real
//! binary rebuilds tmux's builtin arrangements (prefix+M-1..M-5) with
//! the sidebar pane pinned to its edge and width, re-pins it from the
//! `--keep` hook, and restores the recorded base builtin when the
//! sidebar closes. Same integration seam as toggle_pipeline_tests —
//! real scoped tmux server, real binary, real geometry.
//!
//! Same isolation rules as the other pipeline tests: throwaway server
//! via `TMUX_TMPDIR`, serial execution (`make test-ignored`). The
//! smart-layout paths need tmux 3.8+ (JSON layout trees); on older
//! tmux the binary correctly falls back to the stock command, so the
//! tests asserting that geometry skip via [`json_layouts_available`].

use std::process::Command;

struct ScopedServer {
    _dir: tempfile::TempDir,
    window: String,
    pane: String,
}

impl Drop for ScopedServer {
    fn drop(&mut self) {
        // Same fixture-ownership rule as the other pipeline tests: the
        // scoped server dies with this struct, or every run leaks a
        // server process. Safe because TMUX is unset and TMUX_TMPDIR is
        // the throwaway dir.
        let _ = Command::new("tmux").args(["kill-server"]).status();
    }
}

impl ScopedServer {
    /// Wide window: above the popup threshold (default 100), so toggle
    /// takes the split-pane path. Two base panes, seeded with the
    /// options the toggle/layout paths probe.
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
        // `-f /dev/null` matters: a tmux server sources ~/.tmux.conf at
        // startup, TPM plugins included. With an empty config the test
        // seeds exactly the options it needs.
        let status = Command::new("tmux")
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "layoutse2e",
                "-x",
                "160",
                "-y",
                "40",
                "sleep",
                "300",
            ])
            .status()
            .expect("tmux new-session");
        assert!(status.success(), "tmux new-session failed");
        for (key, value) in [
            ("@sidebar_auto_create", "on"),
            ("@sidebar_width", "30"),
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
        // A second base pane so the builtins have something to arrange.
        let status = Command::new("tmux")
            .args([
                "split-window",
                "-v",
                "-t",
                &pane,
                "-p",
                "50",
                "sleep",
                "300",
            ])
            .status()
            .expect("tmux split-window");
        assert!(status.success(), "tmux split-window failed");
        Self {
            _dir: dir,
            window,
            pane,
        }
    }

    /// `(pane_id, left, top, width, height)` for every pane in the
    /// window, in tmux's own listing order.
    fn geometry(&self) -> Vec<(String, u32, u32, u32, u32)> {
        let output = Command::new("tmux")
            .args([
                "list-panes",
                "-t",
                &self.window,
                "-F",
                "#{pane_id} #{pane_left} #{pane_top} #{pane_width} #{pane_height}",
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                (
                    parts[0].to_string(),
                    parts[1].parse().unwrap(),
                    parts[2].parse().unwrap(),
                    parts[3].parse().unwrap(),
                    parts[4].parse().unwrap(),
                )
            })
            .collect()
    }

    fn pane_geometry(&self, pane_id: &str) -> (u32, u32, u32, u32) {
        self.geometry()
            .iter()
            .find(|(id, ..)| id == pane_id)
            .map(|(_, l, t, w, h)| (*l, *t, *w, *h))
            .unwrap_or_else(|| panic!("pane {pane_id} not in window"))
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

    fn window_option(&self, key: &str) -> String {
        let output = Command::new("tmux")
            .args(["display-message", "-t", &self.window, "-p", key])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

fn run_layouts(args: &[&str]) -> i32 {
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

fn run_toggle(args: &[&str]) -> i32 {
    run_layouts(args)
}

/// Whether the local tmux parses JSON layout trees (`{"V":2,"L":...}`,
/// tmux 3.8+). Mirrors the binary's own gate (`version_supports_json_
/// layouts` in cli::layouts): on older tmux the sidebar-aware paths
/// fall back to the stock command, so tests asserting smart-path
/// geometry must skip.
fn json_layouts_available() -> bool {
    let output = Command::new("tmux").arg("-V").output().unwrap();
    let version = String::from_utf8_lossy(&output.stdout);
    let digits = version
        .trim()
        .trim_start_matches(|c: char| !c.is_ascii_digit());
    let mut parts = digits.split('.');
    let Some(major) = parts.next().and_then(|p| p.parse::<u32>().ok()) else {
        return false;
    };
    let minor: Option<u32> = parts.next().and_then(|p| {
        p.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok()
    });
    match minor {
        Some(minor) => major > 3 || (major == 3 && minor >= 8),
        None => major > 3,
    }
}

/// Skip guard for tests whose assertions only hold on tmux 3.8+: the
/// smart-layout paths fall back to the stock command on older tmux, so
/// the geometry those tests assert never materializes there. Call as
/// `if skip_without_json_layouts() { return; }` at the top of a test.
fn skip_without_json_layouts() -> bool {
    if json_layouts_available() {
        return false;
    }
    let version = Command::new("tmux").arg("-V").output().unwrap();
    eprintln!(
        "skipping: {} cannot parse JSON layout trees (sidebar-aware \
         layouts need tmux 3.8+; the binary falls back to the stock \
         command)",
        String::from_utf8_lossy(&version.stdout).trim()
    );
    true
}

/// Create the sidebar split via the auto-create path (`--create-only`),
/// mirroring what the after-new-window hook does.
fn create_sidebar(server: &ScopedServer) -> String {
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
    let code = run_toggle(&["toggle", "--create-only", &server.window, &pane_path]);
    assert_eq!(code, 0, "--create-only must exit 0");
    server
        .sidebar_pane()
        .expect("sidebar pane tagged @pane_role=sidebar")
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn layouts_even_vertical_keeps_the_sidebar_at_the_left_edge() {
    if skip_without_json_layouts() {
        return;
    }
    let server = ScopedServer::spawn();
    let sidebar = create_sidebar(&server);

    let code = run_layouts(&["layouts", "even-vertical", &server.window]);
    assert_eq!(code, 0, "layouts must exit 0");
    assert_eq!(
        server.window_option("#{@sidebar_layout_base}"),
        "even-vertical",
        "the applied builtin is recorded as the window's base"
    );

    // Sidebar pinned: left edge, configured width, full height.
    assert_eq!(server.pane_geometry(&sidebar), (0, 0, 30, 40));
    // The two base panes share the remaining 129 columns, stacked
    // evenly (spread(40, 2) = 20, 19) starting after the sidebar and
    // its border.
    let (l, _t, w, h) = server.pane_geometry(&server.pane);
    assert_eq!((l, w), (31, 129), "base pane column geometry");
    assert_eq!(h, 20, "first stacked pane height");
    let other = server
        .geometry()
        .iter()
        .map(|(id, ..)| id.clone())
        .find(|id| *id != server.pane && *id != sidebar)
        .expect("second base pane");
    let (l, t, w, h) = server.pane_geometry(&other);
    assert_eq!((l, t, w, h), (31, 21, 129, 19), "second stacked pane");
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn layouts_main_vertical_gives_the_main_pane_its_width_next_to_the_sidebar() {
    if skip_without_json_layouts() {
        return;
    }
    let server = ScopedServer::spawn();
    let sidebar = create_sidebar(&server);

    let code = run_layouts(&["layouts", "main-vertical", &server.window]);
    assert_eq!(code, 0);

    assert_eq!(server.pane_geometry(&sidebar), (0, 0, 30, 40));
    // Main pane (the window's first pane) at main-pane-width 80, the
    // other pane in the remaining strip.
    let (l, t, w, h) = server.pane_geometry(&server.pane);
    assert_eq!((l, t, w, h), (31, 0, 80, 40), "main pane geometry");
    let other = server
        .geometry()
        .iter()
        .map(|(id, ..)| id.clone())
        .find(|id| *id != server.pane && *id != sidebar)
        .expect("second base pane");
    assert_eq!(server.pane_geometry(&other), (112, 0, 48, 40));
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn keep_hook_repins_a_drifted_sidebar_width() {
    if skip_without_json_layouts() {
        return;
    }
    let server = ScopedServer::spawn();
    let sidebar = create_sidebar(&server);
    let code = run_layouts(&["layouts", "even-vertical", &server.window]);
    assert_eq!(code, 0);

    // Simulate a layout change that redistributed the sidebar's
    // columns (e.g. a pane close next to it).
    let status = Command::new("tmux")
        .args(["resize-pane", "-t", &sidebar, "-x", "50"])
        .status()
        .expect("tmux resize-pane");
    assert!(status.success());
    assert_eq!(server.pane_geometry(&sidebar).2, 50);

    let code = run_layouts(&["layouts", "--keep", &server.window]);
    assert_eq!(code, 0, "--keep must exit 0");
    assert_eq!(
        server.pane_geometry(&sidebar),
        (0, 0, 30, 40),
        "the keep hook re-pins the sidebar to the configured width"
    );
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn keep_hook_is_inert_without_a_recorded_base() {
    let server = ScopedServer::spawn();
    let sidebar = create_sidebar(&server);
    // No `layouts` call: no base recorded. Drag the sidebar wide.
    let status = Command::new("tmux")
        .args(["resize-pane", "-t", &sidebar, "-x", "50"])
        .status()
        .expect("tmux resize-pane");
    assert!(status.success());

    let code = run_layouts(&["layouts", "--keep", &server.window]);
    assert_eq!(code, 0);
    assert_eq!(
        server.pane_geometry(&sidebar).2,
        50,
        "without a recorded base the keep hook must not touch geometry"
    );
}

#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn closing_the_sidebar_restores_the_recorded_base_builtin() {
    if skip_without_json_layouts() {
        return;
    }
    let server = ScopedServer::spawn();
    create_sidebar(&server);
    let code = run_layouts(&["layouts", "even-vertical", &server.window]);
    assert_eq!(code, 0);

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
    assert_eq!(code, 0, "toggle close must exit 0");
    assert!(server.sidebar_pane().is_none(), "sidebar gone");

    // The base builtin (even-vertical over the two base panes) is
    // rebuilt at the full window size — not the pre-sidebar snapshot.
    let (l, t, w, h) = server.pane_geometry(&server.pane);
    assert_eq!((l, t, w, h), (0, 0, 160, 20), "first pane after restore");
    let other = server
        .geometry()
        .iter()
        .map(|(id, ..)| id.clone())
        .find(|id| *id != server.pane)
        .expect("second base pane");
    assert_eq!(
        server.pane_geometry(&other),
        (0, 21, 160, 19),
        "second pane after restore"
    );
}
#[test]
#[ignore = "requires local tmux; mutates process-global env (run via make test-ignored)"]
fn layouts_without_a_sidebar_applies_the_plain_builtin() {
    let server = ScopedServer::spawn();
    let code = run_layouts(&["layouts", "even-horizontal", &server.window]);
    assert_eq!(code, 0);

    // Two base panes side by side: spread(160, 2) = 80, 79.
    let (l, t, w, h) = server.pane_geometry(&server.pane);
    assert_eq!((l, t, w, h), (0, 0, 80, 40));
    let other = server
        .geometry()
        .iter()
        .map(|(id, ..)| id.clone())
        .find(|id| *id != server.pane)
        .expect("second base pane");
    assert_eq!(server.pane_geometry(&other), (81, 0, 79, 40));
}
