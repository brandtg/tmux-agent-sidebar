use std::collections::{HashMap, HashSet};

use crate::tmux;

pub(crate) fn cmd_toggle(args: &[String]) -> i32 {
    let mut create_only = false;
    let mut force_split = false;
    let mut positional = Vec::new();

    for arg in args {
        if arg == "--create-only" {
            create_only = true;
        } else if arg == "--force-split" {
            force_split = true;
        } else {
            positional.push(arg.as_str());
        }
    }

    let window_id = match positional.first() {
        Some(id) => *id,
        None => return 0,
    };
    let pane_path = positional.get(1).copied().unwrap_or("~");

    let sidebar_width = sidebar_width_cells(window_id);

    let sidebar_position = SidebarPosition::from_setting(&tmux::display_message(
        window_id,
        &format!("#{{{}}}", tmux::SIDEBAR_POSITION),
    ));

    // Mobile viewport: windows narrower than `@sidebar_popup_max_width`
    // open as a tmux popup instead of a split pane. Auto-create stays a
    // no-op here — an unprompted popup on every new window would be
    // hostile. `toggle-all` passes `--force-split` so the global sweep
    // still creates split panes on narrow windows: it is an explicit
    // "same state everywhere" action, and a silently skipped window is
    // exactly the inconsistency the sweep exists to prevent (a window
    // sized down by a small client at sweep time would otherwise stay
    // sidebar-less after growing back). Popups never touch pane layout,
    // which also keeps the layout-recalculation path (implicated in
    // tmux's evbuffer-underflow SIGSEGV during heavy pane output) out of
    // the mobile flow entirely. Resolved before the existing-sidebar
    // check because the toggle-off path needs it too: on a narrow
    // viewport the popup replaces a desktop split pane in the same
    // keystroke.
    let window_width: u32 = tmux::display_message(window_id, "#{window_width}")
        .parse()
        .unwrap_or(0);
    let popup_max_width = tmux::display_message(
        window_id,
        &format!("#{{{}}}", tmux::SIDEBAR_POPUP_MAX_WIDTH),
    );

    // Auto-create decision: `--create-only` runs from the new-window
    // hook and from the toggle-all sweep. A recorded mode
    // (`@sidebar_mode`, written globally by toggle-all) outranks
    // `@sidebar_auto_create`, so prefix+E means "sidebars always
    // on/off everywhere" — including windows created after the sweep,
    // not just the ones that existed at press time. Explicit
    // per-window `toggle` presses (no `--create-only`) never consult
    // the mode.
    if create_only {
        let mode = read_global_option(tmux::SIDEBAR_MODE);
        let auto_create =
            tmux::display_message(window_id, &format!("#{{{}}}", tmux::SIDEBAR_AUTO_CREATE));
        let decision = auto_create_decision(&mode, &auto_create);
        if create_only_skips(&decision, force_split) {
            return 0;
        }
        if let AutoCreateDecision::ForceSplit = decision {
            force_split = true;
        }
    }

    let use_popup = !force_split && should_use_popup(&popup_max_width, window_width);

    // Check for existing sidebar
    let existing_sidebar = find_window_sidebar(window_id);

    if let Some(sidebar_pane) = existing_sidebar {
        if create_only {
            return 0;
        }
        close_sidebar(&sidebar_pane, window_id);
        // A narrow viewport: mobile usually attaches to a session the
        // desktop already set up, so the first toggle press in a narrow
        // viewport lands on a live split-pane sidebar. Reopen it as the
        // popup right away instead of demanding a second press.
        if use_popup {
            record_last_window_width(window_id, window_width);
            return open_popup(window_id, pane_path);
        }
        return 0;
    }

    if use_popup {
        if create_only {
            return 0;
        }
        record_last_window_width(window_id, window_width);
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

    // Snapshot the layout before splitting so the close path can put
    // every pane back where it was (see restore_window_layout).
    save_window_layout(window_id);

    // Record the width the sidebar is created at BEFORE the split (which
    // fires the maintain-width hook): with the recording already
    // committed, the hook sees last == current and cannot insta-hide the
    // pane this command is about to create — relevant for the toggle-all
    // sweep, which force-splits windows a small client may already have
    // shrunk.
    record_last_window_width(window_id, window_width);

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
        // The `@pane_role` tag only lands after `split-window` returns, so
        // two concurrent creates (e.g. double-pressed keys, stacked hooks)
        // can both pass the `find_window_sidebar` check above and split.
        // Collapse any duplicate to the first-listed pane; a lone sidebar
        // makes this a cheap no-op scan.
        dedupe_window_sidebars();
    }

    // Restore focus
    if !active_pane.is_empty() {
        let _ = tmux::run_tmux(&["select-pane", "-t", &active_pane]);
    } else {
        let _ = tmux::run_tmux(&["select-pane", "-t", window_id, "-l"]);
    }

    0
}

/// First role-tagged sidebar pane in `window_id`, or `None`. Panes
/// without the `@pane_role = sidebar` tag are never returned. Shared by
/// the toggle paths and the maintain-width hook.
fn find_window_sidebar(window_id: &str) -> Option<String> {
    let pane_id_role_format = pane_id_role_format();
    let panes_output = tmux::run_tmux(&["list-panes", "-t", window_id, "-F", &pane_id_role_format])
        .unwrap_or_default();

    panes_output.lines().find_map(|line| {
        let parts: Vec<&str> = line.splitn(2, '|').collect();
        if parts.len() >= 2 && parts[1] == "sidebar" {
            Some(parts[0].to_string())
        } else {
            None
        }
    })
}

/// Kill the sidebar pane, then replay the layout snapshot captured at
/// open time. The restore must happen after the kill: while the sidebar
/// pane still exists the saved layout's cell count doesn't match the
/// window and `select-layout` would reject it.
fn close_sidebar(sidebar_pane: &str, window_id: &str) {
    let _ = tmux::run_tmux(&["kill-pane", "-t", sidebar_pane]);
    restore_window_layout(window_id);
}

/// Store the window's current `#{window_layout}` in a window option so
/// the close path can restore it. tmux redistributes a killed pane's
/// space to its layout-tree neighbors rather than undoing the original
/// split, so without this snapshot every toggle cycle shaves a few
/// columns off the panes next to the sidebar. Resizes made while the
/// sidebar is open are rolled back with the restore — the snapshot is
/// the whole geometry, not just the sidebar's slice.
fn save_window_layout(window_id: &str) {
    let layout = tmux::display_message(window_id, "#{window_layout}");
    if layout.is_empty() {
        return;
    }
    let _ = tmux::set_window_option(window_id, tmux::SIDEBAR_SAVED_LAYOUT, &layout);
}

/// Apply the saved pre-sidebar layout, then clear the snapshot. A
/// failed `select-layout` (panes opened or closed since the snapshot)
/// leaves the option in place: it is stale, but a fresh snapshot
/// overwrites it on the next open and a matching one still restores
/// correctly.
fn restore_window_layout(window_id: &str) {
    let layout = saved_window_layout(window_id);
    if layout.is_empty() {
        return;
    }
    if !saved_layout_matches_window(window_id, &layout) {
        // Stale-size snapshot: another client was the latest active one
        // when the sidebar opened, and the window has since changed size.
        // Replaying the layout would pin the panes at the snapshot's
        // dimensions inside the current window, leaving the dotted
        // dead-space border around them until the terminal is resized
        // again (and shrinking the window outright when no bigger client
        // is attached). Skip the restore; tmux's default redistribution
        // of the killed pane's space is size-safe.
        return;
    }
    let restored = tmux::run_tmux(&["select-layout", "-t", window_id, &layout]).is_some();
    if restored {
        let _ = tmux::run_tmux(&[
            "set",
            "-wu",
            "-t",
            window_id,
            "--",
            tmux::SIDEBAR_SAVED_LAYOUT,
        ]);
    }
}

fn saved_window_layout(window_id: &str) -> String {
    tmux::display_message(window_id, &format!("#{{{}}}", tmux::SIDEBAR_SAVED_LAYOUT))
}

/// Parse a `WxH` pair such as `180x53`.
fn parse_dimensions(s: &str) -> Option<(u32, u32)> {
    let (width, height) = s.split_once('x')?;
    Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
}

/// Dimensions a tmux layout string was captured at. Layouts serialize as
/// `checksum,WxH,cells...`, so the pair right after the checksum is the
/// window size the layout belongs to. Returns `None` for anything that
/// does not match that grammar.
fn layout_dimensions(layout: &str) -> Option<(u32, u32)> {
    parse_dimensions(layout.split(',').nth(1)?)
}

/// Pure core of [`saved_layout_matches_window`] so the comparison is
/// unit-testable without a tmux server.
fn dimensions_match(current: &str, layout: &str) -> bool {
    match (parse_dimensions(current), layout_dimensions(layout)) {
        (Some(current), Some(saved)) => current == saved,
        _ => false,
    }
}

/// Whether the saved layout can still be applied to `window_id` as it is
/// now. The snapshot is taken at open time, but the window can change
/// size while the sidebar is up (a smaller client attached, became the
/// latest active one, and detached again). A size mismatch means the
/// snapshot is stale and must not be replayed — see
/// [`restore_window_layout`].
fn saved_layout_matches_window(window_id: &str, layout: &str) -> bool {
    let current = tmux::display_message(window_id, "#{window_width}x#{window_height}");
    dimensions_match(&current, layout)
}

/// Restore the saved layout after the TUI's own pane is gone. The TUI
/// closes by exiting its process, so the restore cannot run inline —
/// the pane (and this process) is still alive when the command would
/// execute, and the saved layout's cell count wouldn't match. A
/// server-side background `run-shell` survives the pane and applies
/// the layout a moment later; `&&` keeps the snapshot when the restore
/// is skipped (e.g. the user re-opened the sidebar before the job
/// fired, so the fresh snapshot must survive).
pub(crate) fn schedule_restore_after_pane_exit(pane_id: &str) {
    let window_id = tmux::display_message(pane_id, "#{window_id}");
    if window_id.is_empty() {
        return;
    }
    let layout = saved_window_layout(&window_id);
    if layout.is_empty() {
        return;
    }
    let command = scheduled_restore_command(&window_id, &layout);
    let _ = tmux::run_tmux(&["run-shell", "-b", &command]);
}

/// Shell command behind [`schedule_restore_after_pane_exit`]. The
/// layout string only ever contains tmux's own layout grammar
/// (digits, `x`, `,`, `[`, `]`, `{`, `}`, checksum hex), so single-quote
/// wrapping is safe.
fn scheduled_restore_command(window_id: &str, layout: &str) -> String {
    let restore = format!(
        "tmux select-layout -t {window_id} '{layout}' && tmux set -wu -t {window_id} -- {}",
        tmux::SIDEBAR_SAVED_LAYOUT
    );
    // Guard with the snapshot's own dimensions: by the time the job fires
    // the window may have changed size (see restore_window_layout). A
    // failed guard skips both the restore and the clear, which also
    // preserves a snapshot written by a concurrent re-open — same
    // contract as the inline path's failed `select-layout`.
    match layout_dimensions(layout) {
        Some((width, height)) => format!(
            "sleep 0.3 && [ \"$(tmux display-message -t {window_id} -p '#{{window_width}}x#{{window_height}}')\" = \"{width}x{height}\" ] && {restore}"
        ),
        // Unparseable layout: keep the historical unconditional restore.
        // `select-layout` still rejects it if the cell count drifted.
        None => format!("sleep 0.3 && {restore}"),
    }
}

/// What `toggle --create-only` (new-window hook, toggle-all sweep)
/// should do, given the recorded system-level mode (`@sidebar_mode`)
/// and the auto-create setting (`@sidebar_auto_create`). Extracted as a
/// pure function so the precedence rule is directly unit-testable.
#[derive(Debug, Eq, PartialEq)]
enum AutoCreateDecision {
    /// Do not create a sidebar.
    Skip,
    /// Create a split-pane sidebar, ignoring the popup threshold.
    ForceSplit,
    /// Create with normal popup-threshold semantics.
    Default,
}

fn auto_create_decision(mode: &str, auto_create: &str) -> AutoCreateDecision {
    match mode.trim() {
        "open" => AutoCreateDecision::ForceSplit,
        "closed" => AutoCreateDecision::Skip,
        _ => {
            if auto_create.trim().eq_ignore_ascii_case("off") {
                AutoCreateDecision::Skip
            } else {
                AutoCreateDecision::Default
            }
        }
    }
}

/// Whether `--create-only` should return without creating a sidebar for
/// `decision`. A `Skip` (recorded `closed` mode, or auto-create off) is
/// overridden when `--force-split` was passed: the toggle-all sweep is
/// an explicit "create now" instruction, and a recorded `closed` mode
/// must not veto it. Extracted as a pure function so the override rule
/// is directly unit-testable.
fn create_only_skips(decision: &AutoCreateDecision, force_split: bool) -> bool {
    matches!(decision, AutoCreateDecision::Skip) && !force_split
}

pub(crate) fn cmd_toggle_all(_args: &[String]) -> i32 {
    let pane_id_role_format = pane_id_role_format();
    let has_sidebar = tmux::run_tmux(&["list-panes", "-a", "-F", &pane_id_role_format])
        .map(|output| any_sidebar_pane(&output))
        .unwrap_or(false);

    if has_sidebar {
        close_all_sidebars();
    } else {
        let _ = tmux::run_tmux(&["set", "-g", tmux::SIDEBAR_MODE, "open"]);
        let all_windows = tmux::run_tmux(&[
            "list-panes",
            "-a",
            "-F",
            "#{window_id}|#{pane_current_path}",
        ])
        .unwrap_or_default();
        for (window_id, pane_path) in unique_window_paths(&all_windows) {
            let args = vec![
                "--create-only".to_string(),
                "--force-split".to_string(),
                window_id,
                pane_path,
            ];
            cmd_toggle(&args);
        }
        // Two overlapping sweeps (e.g. a double-pressed key) both observe
        // "no sidebar" before either creates one and both split every
        // window, leaving two sidebar panes side by side. The create path
        // cannot distinguish its own pane from a concurrent one, so
        // collapse any duplicates afterwards; the kept pane is the first
        // in tmux's own listing order.
        dedupe_window_sidebars();
    }

    0
}

/// The deliberate `toggle-all` close: record nothing, close every
/// role-tagged sidebar pane across all sessions and windows (restoring
/// each window's layout via the normal close path), and pin the global
/// `closed` mode so windows created while the sweep runs stay
/// sidebar-less too. "Off everywhere" is sticky until the next
/// `toggle-all` open.
fn close_all_sidebars() {
    let _ = tmux::run_tmux(&["set", "-g", tmux::SIDEBAR_MODE, "closed"]);
    let all_panes =
        tmux::run_tmux(&["list-panes", "-a", "-F", &pane_window_role_format()]).unwrap_or_default();
    for line in all_panes.lines() {
        if let Some((sidebar_pane, window_id)) = parse_sidebar_close_target(line) {
            close_sidebar(&sidebar_pane, &window_id);
        }
    }
}

fn read_global_option(key: &str) -> String {
    read_global_option_value(key).unwrap_or_default()
}

fn read_global_option_value(key: &str) -> Option<String> {
    tmux::run_tmux(&["show", "-gv", key])
}

/// Resolve the owning session id (`$N`) of `window_id`. `None` when the
/// window no longer exists or tmux fails to answer — callers skip their
/// session-scoped bookkeeping rather than guess.
fn window_session_id(window_id: &str) -> Option<String> {
    let session_id = tmux::display_message(window_id, "#{session_id}");
    if session_id.is_empty() {
        None
    } else {
        Some(session_id)
    }
}

/// Kill every sidebar pane beyond the first in its window, restoring
/// each removed pane's window layout via the normal close path, and
/// pin the surviving sidebar back to the configured width. The restore
/// replays a snapshot taken by whichever overlapping sweep saved last;
/// when it cannot apply (stale cell count), tmux hands the killed
/// pane's space to its neighbors and the survivor drifts wide — the
/// resize makes the outcome deterministic even without the user's
/// layout-change width hook.
fn dedupe_window_sidebars() {
    let all_panes =
        tmux::run_tmux(&["list-panes", "-a", "-F", &pane_window_role_format()]).unwrap_or_default();
    for (survivor_pane, duplicate_pane, window_id) in duplicate_sidebar_targets(&all_panes) {
        close_sidebar(&duplicate_pane, &window_id);
        let target = sidebar_width_cells(&window_id);
        if !target.is_empty() {
            let _ = tmux::run_tmux(&["resize-pane", "-t", &survivor_pane, "-x", &target]);
        }
    }
}

// ─── maintain-width (auto-hide a split sidebar on a shrink) ─────────

/// Auto-hide entry point (`maintain-width <window_id>`), wired to the
/// tmux `window-layout-changed` and `client-resized` hooks. Responsive
/// behavior, media-query style: the layout is a pure function of the
/// current viewport. When a window with a split-pane sidebar shrinks
/// below the popup threshold — a phone client attached, or an existing
/// small client shrank the window — the split pane would degenerate
/// into a sliver, so the window's own sidebar closes (its saved layout
/// replays via the normal close path). No session state, no markers, no
/// restore waves: a window that grows back simply has no sidebar until
/// the user opens one (the toggle/summon keys, or auto-create for new
/// windows). Deliberately cheap and idempotent under the dual-hook
/// double fire: both instances read the same width and close the same
/// pane, and the second kill/restore is a no-op.
pub(crate) fn cmd_maintain_width(args: &[String]) -> i32 {
    let Some(window_id) = args.first().map(String::as_str) else {
        return 0;
    };

    // `@sidebar_auto_hide` gates everything. Unset/empty means on (the
    // conf seeds it, but hand-rolled configs may not); anything but an
    // explicit `on` switches the feature off.
    let auto_hide = tmux::display_message(window_id, &format!("#{{{}}}", tmux::SIDEBAR_AUTO_HIDE));
    if !auto_hide_enabled(&auto_hide) {
        return 0;
    }

    let current_width: u32 = tmux::display_message(window_id, "#{window_width}")
        .parse()
        .unwrap_or(0);
    if current_width == 0 {
        // Window gone (or tmux too busy to answer): nothing to measure,
        // and a 0 recording would poison the next crossing decision.
        return 0;
    }

    let sidebar_pane = find_window_sidebar(window_id);
    let last_width = tmux::display_message(
        window_id,
        &format!("#{{{}}}", tmux::SIDEBAR_LAST_WINDOW_WIDTH),
    )
    .trim()
    .parse()
    .ok();
    let threshold = popup_max_width_from_setting(&tmux::display_message(
        window_id,
        &format!("#{{{}}}", tmux::SIDEBAR_POPUP_MAX_WIDTH),
    ));

    // Record the observation before acting: the close below changes the
    // layout, which re-fires this hook, and the recording turns that
    // second run into a no-op (last == current, no crossing). It also
    // keeps the last-seen width tracking the latest observation in every
    // case, hidden sidebar or not.
    record_last_window_width(window_id, current_width);

    if should_auto_hide(last_width, current_width, threshold, sidebar_pane.is_some()) {
        // Narrow viewport detected: hide this window's sidebar so the
        // real panes keep the whole viewport. The close replays the
        // saved pre-sidebar layout; the user brings the sidebar back
        // with the normal toggle/summon keys when a wide viewport is
        // driving the window again.
        close_sidebar(
            &sidebar_pane.expect("checked by should_auto_hide"),
            window_id,
        );
    }

    0
}

/// Parse `@sidebar_auto_hide`. Empty (unset) means on — the conf seeds
/// `on`, but a hand-rolled config may omit the option — and only an
/// explicit, case-insensitive `on` enables the feature; anything else
/// (`off`, garbage) disables it.
fn auto_hide_enabled(setting: &str) -> bool {
    let setting = setting.trim();
    setting.is_empty() || setting.eq_ignore_ascii_case("on")
}

/// Pure decision core of [`cmd_maintain_width`]: the sweep fires only on
/// a *downward crossing* of the popup threshold — the window was at
/// least `threshold` wide when last observed and is now strictly below
/// it — while a role-tagged split-pane sidebar exists in the crossing
/// window. The sidebar gate keeps the (server-wide) response
/// conservative: a window shrinking below the threshold without a
/// sidebar is just as likely a desktop user resizing a spare terminal,
/// and tearing down every sidebar on the server off that signal alone
/// would be hostile.
///
/// Why crossing detection: `toggle-all --force-split` deliberately
/// creates split panes on narrow windows, and this command fires on
/// every layout change — a plain "narrow && sidebar exists" rule would
/// kill the sweep's sidebar the moment the layout settles. At creation
/// time the recorded width equals the current one, so no crossing is
/// seen. It also debounces the hook: only an actual threshold crossing
/// acts, not every firing while the window stays narrow.
///
/// A missing last width (nothing recorded yet) never hides — no crossing
/// is provable. A zero threshold (`@sidebar_popup_max_width` = 0, popup
/// mode disabled) and a zero current width (failed `#{window_width}`
/// query) suppress the hide as well.
fn should_auto_hide(
    last_width: Option<u32>,
    current: u32,
    threshold: u32,
    has_sidebar: bool,
) -> bool {
    match last_width {
        Some(last) => {
            threshold > 0 && current > 0 && has_sidebar && last >= threshold && current < threshold
        }
        None => false,
    }
}

/// Store `width` in the window-scoped [`SIDEBAR_LAST_WINDOW_WIDTH`]
/// option — the maintain-width hook's memory of the last width it
/// observed. Written on every hook firing and whenever a toggle opens a
/// split sidebar or a popup, so each crossing decision is measured from
/// the latest observation and a stale recording can never insta-hide a
/// freshly created sidebar.
fn record_last_window_width(window_id: &str, width: u32) {
    if width == 0 {
        // A failed `#{window_width}` query must not be recorded: the
        // next crossing would be measured from a phantom zero.
        return;
    }
    let _ = tmux::set_window_option(
        window_id,
        tmux::SIDEBAR_LAST_WINDOW_WIDTH,
        &tmux::sanitize_option_value(&width.to_string()),
    );
}

/// Resolve `@sidebar_width` into absolute cells for `window_id`,
/// converting a percentage against the window's current width. Falls
/// back to the raw setting (or 30 cells when unset) when the window
/// width cannot be queried.
fn sidebar_width_cells(window_id: &str) -> String {
    let setting = tmux::display_message(window_id, &format!("#{{{}}}", tmux::SIDEBAR_WIDTH));
    let setting = if setting.is_empty() { "30" } else { &setting };
    if !setting.ends_with('%') {
        return setting.to_string();
    }
    let window_width: u32 = tmux::display_message(window_id, "#{window_width}")
        .parse()
        .unwrap_or(0);
    let pct: u32 = setting.trim_end_matches('%').parse().unwrap_or(15);
    if window_width == 0 || pct == 0 {
        return setting.to_string();
    }
    (window_width * pct / 100).max(1).to_string()
}

/// Given `pane_window_role_format()` output, pick the sidebar panes to
/// close: every sidebar pane that is not the first sidebar of its
/// window. Returns `(survivor, duplicate, window)` triples — the
/// survivor is the first sidebar in tmux's own listing order. Extracted
/// as a pure function so the keep-first rule is directly unit-testable.
fn duplicate_sidebar_targets(output: &str) -> Vec<(String, String, String)> {
    let mut first: HashMap<String, String> = HashMap::new();
    let mut duplicates = Vec::new();

    for line in output.lines() {
        if let Some((sidebar_pane, window_id)) = parse_sidebar_close_target(line) {
            match first.get(&window_id) {
                None => {
                    first.insert(window_id, sidebar_pane);
                }
                Some(survivor) => {
                    duplicates.push((survivor.clone(), sidebar_pane, window_id));
                }
            }
        }
    }

    duplicates
}

fn any_sidebar_pane(output: &str) -> bool {
    output.lines().any(|line| {
        let parts: Vec<&str> = line.splitn(2, '|').collect();
        parts.len() >= 2 && parts[1] == "sidebar"
    })
}

/// Parse one `pane_window_role_format()` line into the
/// `(sidebar_pane, window_id)` pair to close. Returns `None` for
/// malformed lines and non-sidebar roles so regular panes are never
/// killed by the close sweep.
fn parse_sidebar_close_target(line: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = line.splitn(3, '|').collect();
    if parts.len() >= 3 && parts[2] == "sidebar" {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
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
pub(crate) fn popup_max_width_from_setting(setting: &str) -> u32 {
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

/// Kill every split-pane sidebar still running in `window_id`'s session.
///
/// Mobile normally attaches to a session the desktop already set up, so
/// entering the popup view can start with desktop sidebar panes still
/// live — in other windows, or in the popup's own window when the toggle
/// path did not just close one. Left running, they keep polling tmux and
/// racing the popup over shared global options while wasting the narrow
/// viewport's columns. Each kill replays that window's saved layout via
/// the normal close path; panes without the sidebar role are never
/// touched.
fn stop_session_sidebar_panes(window_id: &str) {
    let Some(session_id) = window_session_id(window_id) else {
        return;
    };
    let output = tmux::run_tmux(&[
        "list-panes",
        "-s",
        "-t",
        &session_id,
        "-F",
        &pane_window_role_format(),
    ])
    .unwrap_or_default();

    for line in output.lines() {
        if let Some((sidebar_pane, pane_window)) = parse_sidebar_close_target(line) {
            close_sidebar(&sidebar_pane, &pane_window);
        }
    }
}

fn open_popup(window_id: &str, start_directory: &str) -> i32 {
    // The popup replaces the desktop split-pane sidebars of the session:
    // stop the ones still running here before drawing the popup. Runs
    // before the anchor pane resolves so `TMUX_PANE` is never pinned to a
    // pane about to be killed.
    stop_session_sidebar_panes(window_id);

    let self_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "tmux-agent-sidebar".to_string());

    let active_pane = tmux::display_message(window_id, "#{pane_id}");
    let args = popup_command(window_id, start_directory, &self_bin, &active_pane);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    // display-popup blocks for the popup's whole lifetime by design; the
    // unbounded wait must not hit the render-path tmux deadline, or the
    // killed client makes this toggle exit 1 (a spurious "returned 1"
    // error on the user's status line) while the popup itself survives.
    let opened = tmux::run_tmux_blocking(&arg_refs).is_some();

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

/// Close-path `list-panes` format: pane id, window id, role. None of
/// the three ever contains `|` (pane/window ids are `%N`/`@N`, and
/// option values are sanitized), so `splitn(3, '|')` is exact.
fn pane_window_role_format() -> String {
    format!("#{{pane_id}}|#{{window_id}}|#{{{}}}", tmux::PANE_ROLE)
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

    // ─── layout save/restore ──────────────────────────────────────────

    #[test]
    fn saved_layout_option_format_round_trips_through_display_message() {
        // `saved_window_layout` interpolates the option name into a
        // format string: `#{@sidebar_saved_layout}` must expand the
        // window-scoped user option (verified by the tmux format
        // grammar — `#{...}` wraps the name verbatim).
        assert_eq!(tmux::SIDEBAR_SAVED_LAYOUT, "@sidebar_saved_layout");
        assert_eq!(
            format!("#{{{}}}", tmux::SIDEBAR_SAVED_LAYOUT),
            "#{@sidebar_saved_layout}"
        );
    }

    #[test]
    fn scheduled_restore_command_targets_window_and_quotes_layout() {
        let command = scheduled_restore_command("@5", "b2c4,237x58,0,0[237x58,0,0,0]");
        assert_eq!(
            command,
            "sleep 0.3 && [ \"$(tmux display-message -t @5 -p '#{window_width}x#{window_height}')\" = \"237x58\" ] \
             && tmux select-layout -t @5 'b2c4,237x58,0,0[237x58,0,0,0]' \
             && tmux set -wu -t @5 -- @sidebar_saved_layout"
        );
    }

    #[test]
    fn scheduled_restore_command_without_dimensions_restores_unconditionally() {
        // A layout that defies tmux's own serialization grammar has no
        // size to guard with; the historical behavior (restore, and let
        // `select-layout` reject a drifted cell count) applies.
        let command = scheduled_restore_command("@5", "not-a-layout");
        assert_eq!(
            command,
            "sleep 0.3 && tmux select-layout -t @5 'not-a-layout' \
             && tmux set -wu -t @5 -- @sidebar_saved_layout"
        );
    }

    // ─── stale layout size guard ──────────────────────────────────────

    #[test]
    fn layout_dimensions_parses_size_after_checksum() {
        assert_eq!(
            layout_dimensions("b2c4,237x58,0,0[237x58,0,0,0]"),
            Some((237, 58))
        );
        // Nested cells (two panes) keep the same checksum,WxH prefix.
        assert_eq!(
            layout_dimensions("cebf,180x53,0,0{33x53,0,0,16,146x53,34,0,5}"),
            Some((180, 53))
        );
    }

    #[test]
    fn layout_dimensions_rejects_malformed_strings() {
        assert_eq!(layout_dimensions(""), None);
        assert_eq!(layout_dimensions("onlyonefield"), None);
        assert_eq!(layout_dimensions("chk,notdims,0,0"), None);
        assert_eq!(layout_dimensions("chk,180xnope,0,0"), None);
    }

    #[test]
    fn dimensions_match_requires_the_snapshot_size() {
        assert!(dimensions_match("226x62", "ba1d,226x62,0,0,0"));
        // The regression this guards: window grew past the snapshot after
        // a small client detached, and replaying pinned the panes at the
        // stale 180x52 cells inside the larger window.
        assert!(!dimensions_match("226x62", "b67d,180x52,0,0,0"));
        // Unresolvable current size (window gone / tmux hiccup) never
        // restores.
        assert!(!dimensions_match("", "ba1d,226x62,0,0,0"));
        assert!(!dimensions_match("226x62", "garbage"));
    }

    #[test]
    fn parse_sidebar_close_target_extracts_pane_and_window() {
        assert_eq!(
            parse_sidebar_close_target("%9|@5|sidebar"),
            Some(("%9".to_string(), "@5".to_string()))
        );
    }

    #[test]
    fn parse_sidebar_close_target_skips_regular_panes_and_malformed_lines() {
        assert_eq!(parse_sidebar_close_target("%1|@5|pane"), None);
        // `@pane_role` unset renders as an empty third field.
        assert_eq!(parse_sidebar_close_target("%1|@5|"), None);
        assert_eq!(parse_sidebar_close_target("%1|sidebar"), None);
        assert_eq!(parse_sidebar_close_target(""), None);
    }

    #[test]
    fn pane_window_role_format_has_three_fields() {
        assert_eq!(
            pane_window_role_format(),
            "#{pane_id}|#{window_id}|#{@pane_role}"
        );
    }

    // ─── auto-create decision (mode vs auto_create precedence) ───────

    #[test]
    fn auto_create_mode_open_forces_split_and_outranks_auto_create() {
        assert_eq!(
            auto_create_decision("open", "off"),
            AutoCreateDecision::ForceSplit
        );
        assert_eq!(
            auto_create_decision("open", "on"),
            AutoCreateDecision::ForceSplit
        );
        assert_eq!(
            auto_create_decision(" open ", "off"),
            AutoCreateDecision::ForceSplit
        );
    }

    #[test]
    fn auto_create_mode_closed_skips_and_outranks_auto_create() {
        assert_eq!(
            auto_create_decision("closed", "on"),
            AutoCreateDecision::Skip
        );
        assert_eq!(
            auto_create_decision("closed", "off"),
            AutoCreateDecision::Skip
        );
    }

    #[test]
    fn auto_create_unset_mode_falls_back_to_auto_create() {
        assert_eq!(auto_create_decision("", "on"), AutoCreateDecision::Default);
        assert_eq!(
            auto_create_decision("", ""),
            AutoCreateDecision::Default,
            "conf seeds @sidebar_auto_create; unset still defaults to create"
        );
        assert_eq!(auto_create_decision("", "off"), AutoCreateDecision::Skip);
        assert_eq!(
            auto_create_decision("garbage", "OFF"),
            AutoCreateDecision::Skip
        );
    }

    // ─── toggle-all dedupe ────────────────────────────────────────────

    #[test]
    fn duplicate_sidebar_targets_keeps_first_sidebar_per_window() {
        let output = "%1|@0|sidebar\n%2|@0|sidebar\n%3|@1|sidebar";
        assert_eq!(
            duplicate_sidebar_targets(output),
            vec![("%1".to_string(), "%2".to_string(), "@0".to_string())]
        );
    }

    #[test]
    fn duplicate_sidebar_targets_ignores_regular_panes_and_malformed_lines() {
        // Regular panes and malformed lines never count as the "first"
        // sidebar of a window, so they neither dedupe nor shield a
        // duplicate: %4 is still a duplicate of %2 on @0.
        let output = "%1|@0|pane\n%2|@0|sidebar\n%1|@0|\nbad\n%4|@0|sidebar";
        assert_eq!(
            duplicate_sidebar_targets(output),
            vec![("%2".to_string(), "%4".to_string(), "@0".to_string())]
        );
    }

    #[test]
    fn duplicate_sidebar_targets_empty_when_single_sidebar_per_window() {
        let output = "%1|@0|sidebar\n%2|@1|sidebar";
        assert!(duplicate_sidebar_targets(output).is_empty());
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

    // ─── maintain-width (auto-hide on shrink) ─────────────────────────

    #[test]
    fn auto_hide_fires_on_downward_crossing_with_sidebar() {
        assert!(should_auto_hide(Some(120), 99, 100, true));
        // Last seen exactly at the threshold counts as "wide": a
        // split-pane sidebar is correct AT the threshold, only strictly
        // below it is popup territory.
        assert!(should_auto_hide(Some(100), 99, 100, true));
    }

    #[test]
    fn auto_hide_never_fires_without_a_downward_crossing() {
        // Staying below the threshold — the common case, since the hook
        // fires on every layout change while the window stays narrow.
        assert!(!should_auto_hide(Some(80), 79, 100, true));
        assert!(!should_auto_hide(Some(80), 80, 100, true));
        // Growing back above the threshold.
        assert!(!should_auto_hide(Some(80), 150, 100, true));
        // No movement at all.
        assert!(!should_auto_hide(Some(100), 100, 100, true));
    }

    #[test]
    fn auto_hide_requires_a_sidebar_pane() {
        // A crossing on a sidebar-less window only records the width.
        assert!(!should_auto_hide(Some(120), 80, 100, false));
    }

    #[test]
    fn auto_hide_skips_when_no_last_width_recorded() {
        // Nothing recorded yet (fresh install, option cleared): no
        // crossing is provable, so never hide.
        assert!(!should_auto_hide(None, 80, 100, true));
    }

    #[test]
    fn auto_hide_skips_on_query_failure_or_disabled_popup() {
        // A failed `#{window_width}` query (parsed as 0) must not hide a
        // live sidebar on a transient tmux hiccup.
        assert!(!should_auto_hide(Some(120), 0, 100, true));
        // `@sidebar_popup_max_width` = 0 disables popup mode outright:
        // there is no threshold to cross and no popup the next toggle
        // could open, so the split pane stays.
        assert!(!should_auto_hide(Some(120), 80, 0, true));
    }

    #[test]
    fn auto_hide_threshold_defaults_from_popup_max_width() {
        // The threshold is resolved through popup_max_width_from_setting,
        // so an unset/invalid `@sidebar_popup_max_width` falls back to
        // the default of 100 and the crossing compares against that.
        let threshold = popup_max_width_from_setting("");
        assert!(should_auto_hide(Some(100), 99, threshold, true));
        assert!(!should_auto_hide(Some(99), 98, threshold, true));
        let zero = popup_max_width_from_setting("0");
        assert!(!should_auto_hide(Some(50), 40, zero, true));
    }

    #[test]
    fn auto_hide_setting_defaults_on_and_rejects_non_on() {
        assert!(auto_hide_enabled(""), "unset option means default on");
        assert!(auto_hide_enabled("on"));
        assert!(
            auto_hide_enabled("  ON  "),
            "case-insensitive, trim-tolerant"
        );
        assert!(!auto_hide_enabled("off"));
        assert!(!auto_hide_enabled("0"));
        assert!(!auto_hide_enabled("garbage"));
    }

    // ─── responsive auto-hide is stateless ───────────────────────────

    #[test]
    fn create_only_skip_is_overridden_by_force_split() {
        // The toggle-all sweep passes `--force-split`: a recorded
        // `closed` mode must not veto an explicit "create now".
        assert!(create_only_skips(&AutoCreateDecision::Skip, false));
        assert!(!create_only_skips(&AutoCreateDecision::Skip, true));
        assert!(!create_only_skips(&AutoCreateDecision::ForceSplit, false));
        assert!(!create_only_skips(&AutoCreateDecision::Default, true));
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
