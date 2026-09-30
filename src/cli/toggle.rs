use std::collections::HashSet;

use crate::tmux;

pub(crate) fn cmd_toggle(args: &[String]) -> i32 {
    let mut create_only = false;
    let mut positional = Vec::new();

    for arg in args {
        if arg == "--create-only" {
            create_only = true;
        } else {
            positional.push(arg.as_str());
        }
    }

    let window_id = match positional.first() {
        Some(id) => *id,
        None => return 0,
    };
    let pane_path = positional.get(1).copied().unwrap_or("~");

    // Check sidebar width setting
    let sidebar_width_setting = {
        let s = tmux::display_message(window_id, &format!("#{{{}}}", tmux::SIDEBAR_WIDTH));
        if s.is_empty() { "30".to_string() } else { s }
    };

    let sidebar_width = if sidebar_width_setting.ends_with('%') {
        let window_width: u32 = tmux::display_message(window_id, "#{window_width}")
            .parse()
            .unwrap_or(0);
        let pct: u32 = sidebar_width_setting
            .trim_end_matches('%')
            .parse()
            .unwrap_or(15);
        if window_width > 0 && pct > 0 {
            let w = window_width * pct / 100;
            if w < 1 {
                "1".to_string()
            } else {
                w.to_string()
            }
        } else {
            sidebar_width_setting
        }
    } else {
        sidebar_width_setting
    };

    let sidebar_position = SidebarPosition::from_setting(&tmux::display_message(
        window_id,
        &format!("#{{{}}}", tmux::SIDEBAR_POSITION),
    ));

    // Check for existing sidebar
    let pane_id_role_format = pane_id_role_format();
    let panes_output = tmux::run_tmux(&["list-panes", "-t", window_id, "-F", &pane_id_role_format])
        .unwrap_or_default();

    let existing_sidebar = panes_output.lines().find_map(|line| {
        let parts: Vec<&str> = line.splitn(2, '|').collect();
        if parts.len() >= 2 && parts[1] == "sidebar" {
            Some(parts[0].to_string())
        } else {
            None
        }
    });

    if let Some(sidebar_pane) = existing_sidebar {
        if create_only {
            return 0;
        }
        let _ = tmux::run_tmux(&["kill-pane", "-t", &sidebar_pane]);
        return 0;
    }

    // Mobile viewport: windows narrower than `@sidebar_popup_max_width`
    // open as a tmux popup instead of a split pane. Auto-create stays a
    // no-op here — an unprompted popup on every new window would be
    // hostile, and `toggle-all` reuses `--create-only`, so narrow windows
    // are skipped there too. Popups never touch pane layout, which also
    // keeps the layout-recalculation path (implicated in tmux's
    // evbuffer-underflow SIGSEGV during heavy pane output) out of the
    // mobile flow entirely.
    let window_width: u32 = tmux::display_message(window_id, "#{window_width}")
        .parse()
        .unwrap_or(0);
    let popup_max_width = tmux::display_message(
        window_id,
        &format!("#{{{}}}", tmux::SIDEBAR_POPUP_MAX_WIDTH),
    );
    if should_use_popup(&popup_max_width, window_width) {
        if create_only {
            return 0;
        }
        return open_popup(window_id, pane_path);
    }

    let pane_geometry_output = tmux::run_tmux(&[
        "list-panes",
        "-t",
        window_id,
        "-F",
        "#{pane_left} #{pane_width} #{pane_id}",
    ])
    .unwrap_or_default();

    let target_pane = target_pane_for_position(&pane_geometry_output, sidebar_position)
        .unwrap_or_else(|| window_id.to_string());
    let split_flags = split_window_flags(sidebar_position);

    // Remember active pane
    let active_pane = tmux::display_message(window_id, "#{pane_id}");

    // Find our own binary path
    let self_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "tmux-agent-sidebar".to_string());

    // Create sidebar pane
    let sidebar_pane = tmux::run_tmux(&[
        "split-window",
        split_flags,
        "-l",
        &sidebar_width,
        "-t",
        &target_pane,
        "-c",
        pane_path,
        "-P",
        "-F",
        "#{pane_id}",
        &self_bin,
    ])
    .map(|s| s.trim().to_string())
    .unwrap_or_default();

    if !sidebar_pane.is_empty() {
        tmux::set_pane_option(&sidebar_pane, tmux::PANE_ROLE, "sidebar");
    }

    // Restore focus
    if !active_pane.is_empty() {
        let _ = tmux::run_tmux(&["select-pane", "-t", &active_pane]);
    } else {
        let _ = tmux::run_tmux(&["select-pane", "-t", window_id, "-l"]);
    }

    0
}

pub(crate) fn cmd_toggle_all(_args: &[String]) -> i32 {
    let pane_id_role_format = pane_id_role_format();
    let has_sidebar = tmux::run_tmux(&["list-panes", "-a", "-F", &pane_id_role_format])
        .map(|output| any_sidebar_pane(&output))
        .unwrap_or(false);

    if has_sidebar {
        let all_panes =
            tmux::run_tmux(&["list-panes", "-a", "-F", &pane_id_role_format]).unwrap_or_default();
        for line in all_panes.lines() {
            let parts: Vec<&str> = line.splitn(2, '|').collect();
            if parts.len() >= 2 && parts[1] == "sidebar" {
                let _ = tmux::run_tmux(&["kill-pane", "-t", parts[0]]);
            }
        }
    } else {
        let all_windows = tmux::run_tmux(&[
            "list-panes",
            "-a",
            "-F",
            "#{window_id}|#{pane_current_path}",
        ])
        .unwrap_or_default();
        for (window_id, pane_path) in unique_window_paths(&all_windows) {
            let args = vec!["--create-only".to_string(), window_id, pane_path];
            cmd_toggle(&args);
        }
    }

    0
}

fn any_sidebar_pane(output: &str) -> bool {
    output.lines().any(|line| {
        let parts: Vec<&str> = line.splitn(2, '|').collect();
        parts.len() >= 2 && parts[1] == "sidebar"
    })
}

fn unique_window_paths(output: &str) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut windows = Vec::new();

    for line in output.lines() {
        let Some((window_id, pane_path)) = line.split_once('|') else {
            continue;
        };
        if seen.insert(window_id.to_string()) {
            windows.push((window_id.to_string(), pane_path.to_string()));
        }
    }

    windows
}

/// Which side of the window the sidebar pane is created on, driven by
/// the `@sidebar_position` tmux option.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SidebarPosition {
    Left,
    Right,
}

impl SidebarPosition {
    /// Parse the raw `@sidebar_position` option value. Only an explicit
    /// (case-insensitive, whitespace-tolerant) `right` selects the right
    /// side; everything else — including unset, empty, or invalid values
    /// — falls back to the historical default of `left`, so a typo never
    /// moves the sidebar somewhere unexpected.
    fn from_setting(setting: &str) -> Self {
        if setting.trim().eq_ignore_ascii_case("right") {
            Self::Right
        } else {
            Self::Left
        }
    }
}

/// Horizontal placement of one pane, parsed from a
/// `#{pane_left} #{pane_width} #{pane_id}` formatted `list-panes` line.
#[derive(Debug, Eq, PartialEq)]
struct PaneGeometry {
    left: u32,
    width: u32,
    pane_id: String,
}

/// Parse a single `list-panes` output line into a [`PaneGeometry`].
/// Returns `None` for malformed lines so callers can simply skip them.
fn parse_pane_geometry(line: &str) -> Option<PaneGeometry> {
    let mut parts = line.split_whitespace();
    let left = parts.next()?.parse().ok()?;
    let width = parts.next()?.parse().ok()?;
    let pane_id = parts.next()?.to_string();
    Some(PaneGeometry {
        left,
        width,
        pane_id,
    })
}

/// Pick the pane the sidebar splits from: the leftmost pane for a left
/// sidebar, or the pane with the largest right edge (`left + width`) for
/// a right sidebar, so the new pane always lands at the window's outer
/// edge. Returns `None` when no line of `output` parses as geometry.
fn target_pane_for_position(output: &str, position: SidebarPosition) -> Option<String> {
    let panes = output.lines().filter_map(parse_pane_geometry);
    match position {
        SidebarPosition::Left => panes.min_by_key(|pane| pane.left),
        SidebarPosition::Right => panes.max_by_key(|pane| pane.left.saturating_add(pane.width)),
    }
    .map(|pane| pane.pane_id)
}

/// `split-window` flags for each placement: `-hfb` inserts the new pane
/// before the target (left of it), `-hf` after it (right of it). Both
/// `f` variants span the full window height.
fn split_window_flags(position: SidebarPosition) -> &'static str {
    match position {
        SidebarPosition::Left => "-hfb",
        SidebarPosition::Right => "-hf",
    }
}

/// Parse the raw `@sidebar_popup_max_width` option value. Unset, empty,
/// or invalid values fall back to the default of 100 columns; `0`
/// explicitly disables popup mode so the split-pane sidebar is always
/// used regardless of window width.
fn popup_max_width_from_setting(setting: &str) -> u32 {
    const DEFAULT_POPUP_MAX_WIDTH: u32 = 100;
    setting
        .trim()
        .parse::<u32>()
        .unwrap_or(DEFAULT_POPUP_MAX_WIDTH)
}

/// Decide between the popup and split-pane sidebar for a window.
/// Popup mode applies when the window is strictly narrower than the
/// configured maximum. A zero window width (query failure) or a zero
/// configured maximum both keep the classic split-pane behaviour.
fn should_use_popup(max_width_setting: &str, window_width: u32) -> bool {
    let max_width = popup_max_width_from_setting(max_width_setting);
    max_width > 0 && window_width > 0 && window_width < max_width
}

/// Open the sidebar as a tmux popup instead of a split pane. `-E`
/// tears the popup down when the sidebar exits, and tmux's default
/// Escape/C-c handling dismisses it earlier — either close kills the
/// TUI process. `-e` flags the TUI so it hides the bottom panel and
/// skips sidebar-pane-specific tmux queries that cannot resolve against
/// the popup's pseudo-pane.
///
/// This call blocks until the popup is dismissed — `display-popup`'s
/// invoking process owns the popup for its lifetime. That is the
/// standard keybinding pattern (the `run-shell` worker runs async, so
/// the client stays responsive), but it means invoking `toggle`
/// directly from a shell waits interactively, like any popup program.
///
/// No `-C`: when a popup is already open on the client, tmux treats a
/// re-invocation as a "modify" and ignores the command, so the toggle
/// key is a no-op while the popup is up (verified on tmux 3.7c, where
/// `-C` also suppresses the new command when no popup exists).
/// Build the `display-popup` argv for the mobile popup sidebar. When
/// `active_pane` is non-empty, `TMUX_PANE` is pinned to it: tmux does not
/// set `TMUX_PANE` for popup children at all (a popup has no real pane;
/// verified on tmux 3.7c), so without the override the TUI's startup guard
/// in `main.rs` exits 1 immediately and `-E` tears the popup down before it
/// is ever drawn. Anchoring to the window's active pane also lets
/// pane-addressed queries (focus tracking, git polling, worktree spawn)
/// resolve inside the popup.
fn popup_command(
    window_id: &str,
    start_directory: &str,
    self_bin: &str,
    active_pane: &str,
) -> Vec<String> {
    let mut args: Vec<String> = [
        "display-popup",
        "-E",
        "-w",
        "90%",
        "-h",
        "90%",
        "-d",
        start_directory,
        "-e",
        "SIDEBAR_POPUP=1",
    ]
    .iter()
    .copied()
    .map(String::from)
    .collect();
    if !active_pane.is_empty() {
        args.push("-e".to_string());
        args.push(format!("TMUX_PANE={active_pane}"));
    }
    args.push("-t".to_string());
    args.push(window_id.to_string());
    args.push(self_bin.to_string());
    args
}

fn open_popup(window_id: &str, start_directory: &str) -> i32 {
    let self_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "tmux-agent-sidebar".to_string());

    let active_pane = tmux::display_message(window_id, "#{pane_id}");
    let args = popup_command(window_id, start_directory, &self_bin, &active_pane);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let opened = tmux::run_tmux(&arg_refs).is_some();

    if opened { 0 } else { 1 }
}

/// Decide whether `cmd_auto_close` should kill the window, given the raw
/// outputs of the tmux queries it performs. Extracted as a pure function
/// so the guard logic is directly unit-testable without a running tmux
/// server.
///
/// - `list_panes_output`: `Some(stdout)` from `list-panes -F <pane role format>`,
///   or `None` if the tmux call failed.
/// - `session_windows`: parsed value of `#{session_windows}`, or `None`
///   if the tmux call failed or the value was unparseable.
/// - `session_attached`: parsed value of `#{session_attached}`, or `None`
///   if the tmux call failed or the value was unparseable.
fn should_kill_window(
    list_panes_output: Option<&str>,
    session_windows: Option<u32>,
    session_attached: Option<u32>,
) -> bool {
    // `list-panes` failed or returned nothing: the window is either gone
    // already or tmux is too busy to answer. Do NOT treat "no output"
    // as "no non-sidebar panes" — that would let us kill a live window
    // whose query happened to race with another tmux command.
    let Some(output) = list_panes_output else {
        return false;
    };
    if output.trim().is_empty() {
        return false;
    }

    let non_sidebar = output.lines().filter(|line| *line != "sidebar").count();
    if non_sidebar != 0 {
        return false;
    }

    let Some(windows) = session_windows else {
        return false;
    };

    // Last window in the session: killing it destroys the session and
    // drops every attached client. One attached client is fine — that
    // matches normal tmux `exit` behaviour on the last pane. Two or
    // more means a shared session (e.g. several terminal tabs attached
    // to `main`) where we cannot tell which clients are "wanted", so
    // preserve the sidebar instead. A missing `session_attached` errs
    // on the side of preservation.
    match windows {
        0 => false,
        1 => matches!(session_attached, Some(n) if n <= 1),
        _ => true,
    }
}

pub(crate) fn cmd_auto_close(args: &[String]) -> i32 {
    let window_id = match args.first() {
        Some(id) => id.as_str(),
        None => return 0,
    };

    let pane_role_format = format!("#{{{}}}", tmux::PANE_ROLE);
    let list_panes_output =
        tmux::run_tmux(&["list-panes", "-t", window_id, "-F", &pane_role_format]);

    let session_windows = tmux::run_tmux(&[
        "display-message",
        "-t",
        window_id,
        "-p",
        "#{session_windows}",
    ])
    .and_then(|s| s.trim().parse().ok());

    let session_attached = tmux::run_tmux(&[
        "display-message",
        "-t",
        window_id,
        "-p",
        "#{session_attached}",
    ])
    .and_then(|s| s.trim().parse().ok());

    if should_kill_window(
        list_panes_output.as_deref(),
        session_windows,
        session_attached,
    ) {
        let _ = tmux::run_tmux(&["kill-window", "-t", window_id]);
    }

    0
}

fn pane_id_role_format() -> String {
    format!("#{{pane_id}}|#{{{}}}", tmux::PANE_ROLE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_sidebar_pane_detects_sidebar_anywhere() {
        let output = "%1|pane\n%2|sidebar\n%3|pane";
        assert!(any_sidebar_pane(output));
    }

    #[test]
    fn any_sidebar_pane_returns_false_without_sidebar() {
        let output = "%1|pane\n%2|main";
        assert!(!any_sidebar_pane(output));
    }

    #[test]
    fn unique_window_paths_deduplicates_windows_and_keeps_spaces() {
        let output = "%1|/Users/me/My Project\n%1|/Users/me/My Project\n%2|/tmp/another project";
        assert_eq!(
            unique_window_paths(output),
            vec![
                ("%1".to_string(), "/Users/me/My Project".to_string()),
                ("%2".to_string(), "/tmp/another project".to_string()),
            ]
        );
    }

    #[test]
    fn unique_window_paths_skips_malformed_lines() {
        let output = "bad-line\n%1|/tmp";
        assert_eq!(
            unique_window_paths(output),
            vec![("%1".to_string(), "/tmp".to_string())]
        );
    }

    // ─── sidebar placement ───────────────────────────────────────────

    #[test]
    fn sidebar_position_parses_right_only() {
        assert_eq!(
            SidebarPosition::from_setting("right"),
            SidebarPosition::Right
        );
        assert_eq!(
            SidebarPosition::from_setting(" RIGHT "),
            SidebarPosition::Right
        );
        assert_eq!(SidebarPosition::from_setting("left"), SidebarPosition::Left);
        assert_eq!(SidebarPosition::from_setting(""), SidebarPosition::Left);
        assert_eq!(
            SidebarPosition::from_setting("invalid"),
            SidebarPosition::Left
        );
    }

    #[test]
    fn target_pane_for_left_position_uses_leftmost_pane() {
        let output = "40 80 %3\n0 20 %1\n20 20 %2";

        assert_eq!(
            target_pane_for_position(output, SidebarPosition::Left),
            Some("%1".to_string())
        );
    }

    #[test]
    fn target_pane_for_right_position_uses_largest_right_edge() {
        let output = "0 20 %1\n20 20 %2\n40 80 %3";

        assert_eq!(
            target_pane_for_position(output, SidebarPosition::Right),
            Some("%3".to_string())
        );
    }

    #[test]
    fn target_pane_for_position_skips_malformed_lines() {
        let output = "bad-line\n0 nope %1\n12 30 %2";

        assert_eq!(
            target_pane_for_position(output, SidebarPosition::Left),
            Some("%2".to_string())
        );
        assert_eq!(target_pane_for_position("", SidebarPosition::Right), None);
    }

    #[test]
    fn split_window_flags_match_tmux_side_semantics() {
        assert_eq!(split_window_flags(SidebarPosition::Left), "-hfb");
        assert_eq!(split_window_flags(SidebarPosition::Right), "-hf");
    }

    // ─── popup mode (mobile viewport) ─────────────────────────────────

    #[test]
    fn popup_max_width_defaults_on_missing_or_invalid_setting() {
        assert_eq!(popup_max_width_from_setting(""), 100);
        assert_eq!(popup_max_width_from_setting("   "), 100);
        assert_eq!(popup_max_width_from_setting("abc"), 100);
        assert_eq!(popup_max_width_from_setting("-5"), 100);
    }

    #[test]
    fn popup_max_width_parses_explicit_values() {
        assert_eq!(popup_max_width_from_setting("120"), 120);
        assert_eq!(popup_max_width_from_setting(" 80 "), 80);
        // Explicit zero disables popup mode.
        assert_eq!(popup_max_width_from_setting("0"), 0);
    }

    #[test]
    fn should_use_popup_true_below_default_threshold() {
        // Missing option → default 100 → narrow windows use the popup.
        assert!(should_use_popup("", 80));
        assert!(should_use_popup("", 99));
    }

    #[test]
    fn should_use_popup_false_at_or_above_threshold() {
        assert!(!should_use_popup("", 100));
        assert!(!should_use_popup("", 120));
    }

    #[test]
    fn should_use_popup_respects_custom_threshold() {
        assert!(should_use_popup("60", 59));
        assert!(!should_use_popup("60", 60));
    }

    #[test]
    fn should_use_popup_zero_threshold_disables_popup_mode() {
        assert!(!should_use_popup("0", 40));
    }

    #[test]
    fn should_use_popup_zero_window_width_keeps_split_pane() {
        // A failed `#{window_width}` query must not silently switch the
        // user's desktop sidebar into popup mode.
        assert!(!should_use_popup("", 0));
    }

    #[test]
    fn popup_command_pins_active_pane_as_tmux_pane() {
        // Without this override the popup'd TUI never starts: display-popup
        // leaves TMUX_PANE unset, so main.rs's startup guard exits 1 and -E
        // closes the popup instantly (the "mobile toggle flashes and exits 1"
        // regression).
        let args = popup_command("@5", "~", "/bin/tmux-agent-sidebar", "%9");
        let expected = [
            "display-popup",
            "-E",
            "-w",
            "90%",
            "-h",
            "90%",
            "-d",
            "~",
            "-e",
            "SIDEBAR_POPUP=1",
            "-e",
            "TMUX_PANE=%9",
            "-t",
            "@5",
            "/bin/tmux-agent-sidebar",
        ];
        assert_eq!(args, expected);
    }

    #[test]
    fn popup_command_skips_tmux_pane_when_unresolved() {
        // Pane resolution failing means the window is already gone; the
        // launch itself will fail on the target, so do not pass a bogus
        // empty TMUX_PANE=.
        let args = popup_command("@5", "~", "/bin/tmux-agent-sidebar", "");
        assert!(
            !args.iter().any(|arg| arg.starts_with("TMUX_PANE=")),
            "unexpected TMUX_PANE override: {args:?}"
        );
        assert_eq!(
            args.last().map(String::as_str),
            Some("/bin/tmux-agent-sidebar")
        );
    }

    // ─── should_kill_window ───────────────────────────────────────────

    #[test]
    fn should_kill_window_kills_when_only_sidebar_and_other_windows_exist() {
        // Classic intended path: sidebar alone in a window, session has
        // other windows to fall back on. Attached-client count is
        // irrelevant because killing this window does not end the
        // session.
        assert!(should_kill_window(Some("sidebar"), Some(2), None));
        assert!(should_kill_window(Some("sidebar"), Some(2), Some(0)));
        assert!(should_kill_window(Some("sidebar"), Some(2), Some(5)));
    }

    #[test]
    fn should_kill_window_skips_when_non_sidebar_pane_remains() {
        // Another pane with `@pane_role` explicitly set to something
        // non-sidebar (e.g. a spawn-marked pane) keeps the window alive.
        assert!(!should_kill_window(Some("sidebar\npane"), Some(5), Some(1)));
        // `@pane_role` unset renders as an empty line — that pane is
        // a regular user pane, not a sidebar, so the window must stay.
        // The real tmux output for [sidebar pane, regular pane] is
        // "sidebar\n\n" (sidebar's role, then the regular pane's empty
        // role followed by the final record separator).
        assert!(!should_kill_window(Some("sidebar\n\n"), Some(5), Some(1)));
        assert!(!should_kill_window(Some("\nsidebar\n"), Some(5), Some(1)));
    }

    #[test]
    fn should_kill_window_skips_when_list_panes_failed() {
        // `list-panes` failure must never be treated as "window is empty" —
        // that used to let a busy-tmux race kill a live window.
        assert!(!should_kill_window(None, Some(5), Some(1)));
    }

    #[test]
    fn should_kill_window_skips_when_list_panes_empty() {
        // Whitespace-only output (e.g. window already gone) must not
        // trigger a kill either.
        assert!(!should_kill_window(Some(""), Some(5), Some(1)));
        assert!(!should_kill_window(Some("   \n"), Some(5), Some(1)));
    }

    #[test]
    fn should_kill_window_kills_last_window_when_single_client_attached() {
        // One client attached to a single-window session: destroying
        // the session only detaches the same client that just kept the
        // session alive, which matches tmux's standard `exit` behaviour
        // on the last pane — the user expects the sidebar to go with it.
        assert!(should_kill_window(Some("sidebar"), Some(1), Some(1)));
    }

    #[test]
    fn should_kill_window_kills_last_window_when_detached() {
        // No clients attached: killing the session harms no one, and
        // a stranded sidebar in a detached session is pointless anyway.
        assert!(should_kill_window(Some("sidebar"), Some(1), Some(0)));
    }

    #[test]
    fn should_kill_window_preserves_last_window_when_multiple_clients_attached() {
        // Core regression guard (0dc6e99): killing the last window of
        // a session drops every attached client. With multiple terminal
        // tabs sharing a single `main` session, that manifested as every
        // tab dying at once. Keep the sidebar stranded rather than nuke
        // the session.
        assert!(!should_kill_window(Some("sidebar"), Some(1), Some(2)));
        assert!(!should_kill_window(Some("sidebar"), Some(1), Some(7)));
    }

    #[test]
    fn should_kill_window_preserves_last_window_when_attached_query_failed() {
        // Without knowing how many clients are attached we cannot prove
        // the kill is safe. Better a lingering sidebar pane than a
        // mass-disconnect.
        assert!(!should_kill_window(Some("sidebar"), Some(1), None));
    }

    #[test]
    fn should_kill_window_skips_when_session_windows_query_failed() {
        // If we cannot prove the session has other windows, err on the
        // side of preservation. Better to leave a lingering sidebar
        // pane than to destroy a live workspace.
        assert!(!should_kill_window(Some("sidebar"), None, Some(1)));
        assert!(!should_kill_window(Some("sidebar"), Some(0), Some(1)));
    }
}
