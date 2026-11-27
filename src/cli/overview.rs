//! Experimental `overview` subcommand: a fullscreen grid of live agent panes.
//!
//! Design notes live in docs/overview-grid-spike.md. The short version: the
//! grid never attaches to the agent panes (attaching couples terminal sizes
//! — a tmux window has exactly one size shared by every session linking it,
//! so a smaller viewer resizes the real agent). Instead each cell mirrors a
//! pane by polling `capture-pane -p -e` and painting the captured styled
//! grid into a ratatui buffer, reusing the vte parser from the `capture`
//! subcommand. The mirror itself is read-only, with one documented,
//! always-restored exception: entering focus mode (Enter from the grid, or
//! a click-zoom) temporarily zooms the source pane's own window
//! (`resize-pane -Z`), because `capture-pane` has no size override — while
//! the pane shares its window with the 40-column sidebar, content at the
//! popup's width does not exist until the pane itself is wider. The zoom
//! rides a drop guard held by the run loop, so every exit path (Esc
//! collapse, Enter/f into the real pane, q, Ctrl-C, the ✕ click, `?` error
//! propagation) restores the pre-zoom layout; a window that arrived
//! already zoomed is never touched (that zoom is the user's state), a pane
//! alone in its window skips it (nothing to gain), and a restore whose
//! pane or window died is a silent no-op plus a debug-trace line. A
//! capture still narrower than the popup (a source that cannot reach the
//! popup's width even zoomed — multi-client setups) centers so the
//! residual margin splits evenly.
//!
//! Presentation rules that make the grid an instrument instead of a
//! wallpaper:
//!
//! - Bottom-anchored capture: agent TUIs draw their current state at the
//!   bottom of the screen, so a cell shorter than its source pane keeps the
//!   LAST rows (the live part) and drops the top.
//! - Ordering: sidebar order is the default — cells sort exactly like the
//!   sidebar's rows (repo/session groups alphabetically, then
//!   session → window → pane), so the two views share one mental map and
//!   cells don't jump when an agent starts working. `s` toggles the
//!   status-first order: waiting/error cells lead, get loud borders and a
//!   wait-reason strip; idle cells fade to grey. A header row summarizes
//!   the counts.
//! - Enter zooms the selected cell fullscreen (still a capture; the one
//!   write is the temporary window-zoom described above, always restored);
//!   Esc collapses back to the grid.
//! - Interaction ladder (docs/overview-grid-spike.md): `p` prompts the
//!   target pane live — from the grid (selected cell) or zoomed, the zoom
//!   state never changes. Every key press is forwarded to the pane (tmux
//!   key names via `send-keys`, bracketed pastes via `paste-buffer`), so
//!   the agent's own UI — slash menus, completions, permission answers,
//!   Esc interrupts — runs natively inside the mirrored cell, and the
//!   prompted pane's capture refreshes at a fast cadence to keep up.
//!   Ctrl-g leaves (never Esc: that must reach the agent as its interrupt
//!   key); a click leaves too. Forwards are refused unless the pane's
//!   foreground is still the agent — input must never reach a shell.
//!   `f` — or Enter again while zoomed — closes the popup and focuses the
//!   real pane via `select_pane`. The target is the zoomed cell when
//!   zoomed, else the selected cell.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::{
    event::{
        self, EnableBracketedPaste, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use crate::cli::capture::ansi::{self, StyledCell, palette_rgb};
use crate::cli::capture::tmux_probe::capture_pane;
use crate::group::{
    PaneGitInfo, group_panes_by_repo, group_panes_by_session, resolve_pane_git_info,
};
use crate::state::GroupBy;
use crate::tmux::{self, AgentType, PaneInfo, PaneStatus, SessionInfo};
use crate::ui::colors::ColorTheme;

const DEFAULT_INTERVAL_MS: u64 = 500;
/// Drive mode's capture cadence for the driven pane — the rest of the grid
/// keeps [`DEFAULT_INTERVAL_MS`]. An agent menu must render live while its
/// keys are being forwarded, and the spike measured ~1 ms per capture, so
/// one pane at ~16 Hz is negligible.
const DRIVE_INTERVAL_MS: u64 = 60;
const DRIVE_INTERVAL: Duration = Duration::from_millis(DRIVE_INTERVAL_MS);
/// Minimum cell size the layout tries to preserve by growing columns
/// (smaller cells) until every row fits the terminal height.
const MIN_CELL_WIDTH: u16 = 40;
const MIN_CELL_HEIGHT: u16 = 8;

/// Backgrounds at or above this relative luminance count as "light" —
/// a light-themed TUI drawing on the dark grid canvas. 0.6 keeps dark
/// theme panels (236 and below) intact while catching the whole light
/// pink/grey family (216-231, 244+).
const LIGHT_BG_LUMINANCE: f32 = 0.6;
/// Foregrounds below this luminance are "dark text" and get lifted when
/// their background is stripped, or they would sit invisible on the dark
/// canvas.
const DARK_FG_LUMINANCE: f32 = 0.45;
/// Foreground used for lifted dark text: light grey, readable on dark.
const LIFTED_FG: u8 = 250;

/// One agent pane in the grid: its metadata plus the last captured screen.
struct OverviewPane {
    info: PaneInfo,
    /// Captured source screen trimmed to the pane's own dimensions.
    /// `None` when the capture failed (pane gone, tmux error, ...).
    grid: Option<Vec<Vec<StyledCell>>>,
    err: Option<String>,
}

/// Prompt mode (`p`): every key press is forwarded to the target pane, so
/// the agent's own UI — slash menus, completions, permission answers, Esc
/// interrupts — runs natively inside the mirrored cell. The overview stops
/// being an editor here and becomes the input side of a terminal: the
/// agent owns the line, the draft, the autocomplete. The cell keeps its
/// size — this works from the grid (selected cell) and zoomed alike, no
/// zoom change. This struct only remembers where the keys go, at what
/// resolution to re-capture the pane, and what to say when a forward is
/// refused.
struct DriveInput {
    pane_id: String,
    /// `agent · repo` label on the bar, the cell-title vocabulary — the
    /// bar names the pane every key will hit.
    label: String,
    /// The pane's dimensions, cached at entry and refreshed on slow
    /// ticks; the fast capture parses against them.
    dims: Option<(u16, u16)>,
    /// Last refused or failed forward, shown on the bar; the next key
    /// clears it.
    err: Option<String>,
}

/// What a key press while prompting asks the caller to do.
enum DriveAction {
    /// Forward the key to the prompted pane.
    Forward,
    /// Leave prompt mode. Ctrl-g, deliberately not Esc: Esc is the
    /// agents' interrupt key and must be forwarded, so the leave chord
    /// has to be one no agent binds.
    Leave,
}

/// Prompt mode's whole key logic: one chord leaves, everything else
/// forwards.
fn handle_drive_key(key: &KeyEvent) -> DriveAction {
    if key.code == KeyCode::Char('g') && key.modifiers.contains(KeyModifiers::CONTROL) {
        DriveAction::Leave
    } else {
        DriveAction::Forward
    }
}

/// How long after a forwarded Enter a second Enter is treated as the
/// same paste run's next line break rather than a deliberate submit.
const PASTE_ENTER_DEBOUNCE: Duration = Duration::from_millis(20);

/// Translate one key press into what drive mode sends, debouncing the
/// paste-run Enters (audit 2026-11-25 L-cli-3). Without `Event::Paste`
/// support in the terminal chain, a multi-line paste arrives as a key
/// run and every Enter in it would submit a line into the agent —
/// Enters closer together than any human repeats them are the paste
/// run's line breaks, so the first is forwarded and the rest are
/// dropped: the visible, recoverable failure (lines left unsubmitted
/// in the agent's input) instead of one accidental send per line.
/// `last_enter_forward` is the event loop's running state; any
/// non-Enter key resets it, since typing between Enters is a human.
fn forward_for_drive(
    key: &KeyEvent,
    last_enter_forward: &mut Option<Instant>,
) -> Option<ForwardedKey> {
    if key.code == KeyCode::Enter {
        let paste_run = last_enter_forward.is_some_and(|at| at.elapsed() < PASTE_ENTER_DEBOUNCE);
        *last_enter_forward = Some(Instant::now());
        if paste_run {
            return None;
        }
    } else {
        *last_enter_forward = None;
    }
    forward_key_name(key)
}

/// One forwarded key press: a tmux key name (sent with key-name lookup,
/// so `Enter` arrives as the Return key and `C-c` as the interrupt byte)
/// or literal text (sent with `-l`, so characters are never reinterpreted
/// as key names).
#[derive(Debug)]
enum ForwardedKey {
    Named(String),
    Text(String),
}

/// Translate one key press into what drive mode sends. `None` drops the
/// key: an unmappable name must never fall back to literal text, because
/// `send-keys` types an unrecognized name as characters. Chorded
/// characters keep only Ctrl and Alt — a Shift'ed character already
/// carries its shift in the character itself — while a special key's
/// Shift is part of the key (S-Up selects, in TUIs that select with the
/// keyboard).
fn forward_key_name(key: &KeyEvent) -> Option<ForwardedKey> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // Plain characters (shifted ones included) are literal text, never
    // key names — key-name lookup would put them up for debate.
    if let KeyCode::Char(c) = key.code
        && !ctrl
        && !alt
    {
        return Some(ForwardedKey::Text(c.to_string()));
    }
    // The base name, and whether Shift joins it. tmux's vocabulary as
    // verified against tmux 3.8.
    let (base, shift_joins) = match key.code {
        // A space names "Space" — `C- ` is not a name.
        KeyCode::Char(c) => (
            if c == ' ' {
                "Space".to_string()
            } else {
                c.to_string()
            },
            false,
        ),
        KeyCode::Enter => ("Enter".to_string(), true),
        KeyCode::Esc => ("Escape".to_string(), true),
        KeyCode::Tab => ("Tab".to_string(), true),
        KeyCode::BackTab => ("BTab".to_string(), true),
        KeyCode::Backspace => ("BSpace".to_string(), true),
        KeyCode::Delete => ("DC".to_string(), true),
        KeyCode::Insert => ("IC".to_string(), true),
        KeyCode::Home => ("Home".to_string(), true),
        KeyCode::End => ("End".to_string(), true),
        KeyCode::PageUp => ("PPage".to_string(), true),
        KeyCode::PageDown => ("NPage".to_string(), true),
        KeyCode::Up => ("Up".to_string(), true),
        KeyCode::Down => ("Down".to_string(), true),
        KeyCode::Left => ("Left".to_string(), true),
        KeyCode::Right => ("Right".to_string(), true),
        KeyCode::F(n) if (1..=12).contains(&n) => (format!("F{n}"), true),
        // Modifier-only presses, media keys, and anything without an
        // agent-visible meaning: dropped, never typed as characters.
        _ => return None,
    };
    // Prefix order C-, M-, S- is what tmux parses (C-M-S-Home verified).
    let mut name = String::new();
    if ctrl {
        name.push_str("C-");
    }
    if alt {
        name.push_str("M-");
    }
    if shift && shift_joins {
        name.push_str("S-");
    }
    name.push_str(&base);
    Some(ForwardedKey::Named(name))
}

/// The per-key gate, stricter than [`prompt_refusal`]: keys flow
/// continuously here, so the up-to-one-refresh-stale listing field is not
/// trusted — the pane's foreground is queried fresh per key. `Some`
/// refuses and the reason goes on the bar; the prompt stays open (the
/// next key re-decides), so the reason stays visible until the user
/// leaves with Ctrl-g or the slow tick closes a prompt whose pane is
/// truly gone.
fn drive_refusal(panes: &[OverviewPane], pane_id: &str) -> Option<String> {
    if !panes.iter().any(|p| p.info.pane_id == pane_id) {
        return Some("pane is gone — nothing sent".to_string());
    }
    let cmd = tmux::display_message(pane_id, "#{pane_current_command}");
    let cmd = cmd.trim();
    if tmux::is_shell_command(cmd) {
        return Some(format!("no agent in the foreground ({cmd}) — nothing sent"));
    }
    None
}

/// Refuse to start prompting a pane whose text would land in a shell
/// instead of an agent. Panes deliberately survive in the listing after
/// their agent exits — hook-clear races with detection (query.rs keeps
/// claude/opencode panes alive on a shell foreground) — and keys sent
/// into a bare shell execute the prompt as a command line. `None` means
/// the pane is live under an agent and prompting may start. The listing
/// field can be up to one refresh stale; the per-key gate
/// ([`drive_refusal`]) re-decides with a live query from the first
/// forwarded key on.
fn prompt_refusal(panes: &[OverviewPane], pane_id: &str) -> Option<String> {
    match panes.iter().find(|p| p.info.pane_id == pane_id) {
        None => Some("pane is gone — nothing sent".to_string()),
        Some(p) if tmux::is_shell_command(&p.info.current_command) => Some(format!(
            "no agent in the foreground ({}) — nothing sent",
            p.info.current_command
        )),
        Some(_) => None,
    }
}

/// The pane the keyboard interactions act on: the zoomed cell while reading
/// one agent fullscreen, else the selected grid cell. The id is verified
/// against the current listing, so a pane that vanished between ticks is no
/// longer a target.
fn target_pane<'a>(
    panes: &'a [OverviewPane],
    zoomed: Option<&str>,
    selected: Option<&str>,
) -> Option<&'a OverviewPane> {
    let id = zoomed.or(selected)?;
    panes.iter().find(|p| p.info.pane_id == id)
}

/// Focus mode's temporary window-zoom (audit 2026-11-25 M-cli-5). The
/// zoomed view mirrors the capture 1:1, and `capture-pane` has no size
/// override — content at wider widths does not exist until the pane itself
/// is wider. While the source pane shares its window with siblings (the
/// plugin's standard layout: a 40-column sidebar beside the agent), the
/// capture can never fill the popup. So entering focus mode zooms the
/// pane's own window (`resize-pane -Z`): the pane and its pty grow to the
/// window's full size, the agent reflows on the pty SIGWINCH, and the next
/// capture fills the popup. The zoom is held as this guard and restored on
/// drop, so every exit path — Esc collapse, Enter/f into the real pane, q,
/// Ctrl-C, the ✕ click, `?` error propagation — puts the pre-zoom layout
/// back.
struct WindowZoom {
    pane_id: String,
}

impl WindowZoom {
    /// Take the temporary zoom for `pane_id`, given the
    /// `#{window_panes}|#{window_zoomed_flag}` probe. `None` skips: a
    /// window already zoomed on arrival is the user's state, never ours to
    /// touch; a pane alone in its window has nothing to gain; a failed
    /// probe (pane gone) has nothing to zoom.
    fn take(pane_id: &str, probe: &str) -> Option<Self> {
        if !may_take_window_zoom(probe) {
            return None;
        }
        tmux::run_tmux_capture(&["resize-pane", "-Z", "-t", pane_id]).ok()?;
        Some(Self {
            pane_id: pane_id.to_string(),
        })
    }

    /// Lift the temporary zoom — flag-gated, so a zoom someone else
    /// already lifted is never re-toggled (`resize-pane -Z` would zoom
    /// again) and a pane that died while zoomed degrades to a silent
    /// no-op (the probe fails). A restore that still fails (the window
    /// died) lands in the debug trace; a crash mid-zoom leaves the pane
    /// zoomed exactly like a user zoom — recoverable with prefix-z, the
    /// agent unaffected.
    fn restore(&self) {
        if !zoom_flag_set(&tmux::display_message(
            &self.pane_id,
            "#{window_zoomed_flag}",
        )) {
            return;
        }
        if tmux::run_tmux_capture(&["resize-pane", "-Z", "-t", &self.pane_id]).is_err() {
            crate::debug::log(&format!(
                "overview: window-zoom restore failed for {} (pane or window gone)",
                self.pane_id
            ));
        }
    }
}

impl Drop for WindowZoom {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Whether focus mode may take its temporary window-zoom, from a
/// `#{window_panes}|#{window_zoomed_flag}` probe: only a window that
/// shares the pane with siblings and arrived unzoomed.
fn may_take_window_zoom(probe: &str) -> bool {
    let Some((count, flag)) = probe.trim().split_once('|') else {
        return false;
    };
    count.trim().parse::<u16>().is_ok_and(|n| n > 1) && flag.trim() == "0"
}

/// Whether a `#{window_zoomed_flag}` probe says the window is still
/// zoomed. An empty probe (pane gone) reads as not zoomed.
fn zoom_flag_set(probe: &str) -> bool {
    probe.trim() == "1"
}

/// Enter focus mode's temporary window-zoom and re-capture immediately at
/// the new geometry (audit 2026-11-25 M-cli-5). Returns the guard to hold
/// for the zoom's lifetime; `None` when the ownership rules skipped the
/// zoom (already-zoomed window, lone pane, dead pane) and the view stays
/// the plain 1:1 mirror at the pane's own width.
fn enter_focus_zoom(
    pane: &mut OverviewPane,
    last_dims: &mut HashMap<String, (u16, u16)>,
) -> Option<WindowZoom> {
    let probe = tmux::display_message(&pane.info.pane_id, "#{window_panes}|#{window_zoomed_flag}");
    let zoom = WindowZoom::take(&pane.info.pane_id, &probe)?;
    // The pane (and its pty) just grew to the window's full size — the
    // agent reflows on the pty SIGWINCH and the next tick would pick the
    // new geometry up, but the first frame should already be wide:
    // re-capture now and record the dims so drive entry reads them too.
    let dims = pane_dimensions_for(&pane.info.pane_id);
    if let Ok(grid) = capture_grid(&pane.info, dims) {
        pane.grid = Some(grid);
        pane.err = None;
    }
    if let Some(dims) = dims {
        last_dims.insert(pane.info.pane_id.clone(), dims);
    }
    Some(zoom)
}

/// One pane's geometry from one display-message — the focus path's
/// targeted counterpart to [`pane_dimensions`], which rescans the whole
/// server. `None` when the probe fails (pane gone between listing and
/// capture; [`capture_grid`] falls back to a common size).
fn pane_dimensions_for(pane_id: &str) -> Option<(u16, u16)> {
    let out = tmux::display_message(pane_id, "#{pane_width}|#{pane_height}");
    let (width, height) = out.trim().split_once('|')?;
    Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
}

pub fn cmd_overview(args: &[String]) -> i32 {
    let mut popup = false;
    let mut popup_client: Option<String> = None;
    let mut interval = Duration::from_millis(DEFAULT_INTERVAL_MS);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--popup" => popup = true,
            "--popup-client" => {
                let value = iter.next().map(|s| s.to_string()).unwrap_or_default();
                // A missing value or a following flag (a value starting
                // with `--`) is a caller mistake: silently consuming
                // `--interval-ms 500` as the client name drops the flag
                // and targets a client that does not exist (audit
                // 2026-11-25 L-cli-4).
                if value.is_empty() || value.starts_with("--") {
                    eprintln!("overview: --popup-client requires a client name");
                    return 2;
                }
                popup_client = Some(value);
            }
            "--interval-ms" => match iter.next().and_then(|v| v.parse::<u64>().ok()) {
                Some(ms) if ms >= 100 => interval = Duration::from_millis(ms),
                _ => {
                    eprintln!("overview: --interval-ms requires a number >= 100");
                    return 2;
                }
            },
            other => {
                eprintln!("overview: unknown argument '{other}'");
                return 2;
            }
        }
    }

    if popup {
        return match spawn_popup(popup_client) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("overview: {e}");
                1
            }
        };
    }

    match run_tui(interval) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("overview: {e}");
            1
        }
    }
}

/// Open the overview in a fullscreen tmux popup. The popup runs this same
/// binary's TUI mode; with `-E` the popup closes as soon as the TUI exits,
/// so quitting with q/Esc tears the whole container down.
///
/// `client` targets the popup at a specific tmux client — the keybinding
/// passes `#{client_name}` at keypress time, because a run-shell child has
/// no current client of its own. Without it, tmux resolves the client from
/// `TMUX_PANE` (the manual `--popup` invocation from any pane shell).
fn spawn_popup(client: Option<String>) -> Result<(), String> {
    if client.is_none() && std::env::var("TMUX_PANE").unwrap_or_default().is_empty() {
        return Err("--popup must be run from inside tmux".into());
    }
    let exe = std::env::current_exe().map_err(|e| format!("resolve current exe: {e}"))?;
    let cwd = std::env::current_dir().map_err(|e| format!("resolve cwd: {e}"))?;
    // Through the shared exec path so the IPC gate applies (audit
    // 2026-11-19 C3). `run_tmux_blocking` matches this call's contract:
    // display-popup's invoking process owns the popup for its lifetime,
    // so the command intentionally blocks until it closes.
    let mut args: Vec<String> = vec![
        "display-popup".into(),
        "-E".into(),
        "-w".into(),
        "100%".into(),
        "-h".into(),
        "100%".into(),
        "-d".into(),
        cwd.to_string_lossy().to_string(),
    ];
    if let Some(client) = client {
        args.push("-t".into());
        args.push(client);
    }
    // Two positionals: tmux execs them directly, no `sh -c` (tmux
    // 3.8) — the raw path is safe, quoting would corrupt argv[0].
    args.push(exe.to_string_lossy().to_string());
    args.push("overview".into());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if tmux::run_tmux_blocking(&args).is_some() {
        Ok(())
    } else {
        Err("tmux display-popup failed".to_string())
    }
}

/// Spawn the overview popup without blocking the caller: the sidebar TUI
/// fires this on its `o` key, and a detached tmux client owns the popup's
/// lifetime (reaped on a side thread). The popup is anchored to the
/// sidebar's own window and pane — tmux does not set `TMUX_PANE` for popup
/// children at all, so it is pinned explicitly (same pattern as the
/// sidebar's own popup mode).
pub(crate) fn spawn_overview_popup_detached() -> Result<(), String> {
    let pane = std::env::var("TMUX_PANE").unwrap_or_default();
    if pane.is_empty() {
        return Err("TMUX_PANE not set".into());
    }
    let window = tmux::run_tmux(&["display-message", "-p", "-t", &pane, "#{window_id}"])
        .ok_or("could not resolve the sidebar's window")?;
    let window = window.trim().to_string();
    let exe = std::env::current_exe().map_err(|e| format!("resolve current exe: {e}"))?;
    let cwd = std::env::current_dir().map_err(|e| format!("resolve cwd: {e}"))?;
    // Through the shared exec path so the IPC gate applies (audit
    // 2026-11-19 C3). The popup's lifetime still belongs to a detached
    // thread: run_tmux_blocking inside the thread blocks on the popup,
    // while the sidebar's event loop carries on (same shape as the old
    // spawn + wait).
    let args: Vec<String> = vec![
        "display-popup".into(),
        "-E".into(),
        "-w".into(),
        "100%".into(),
        "-h".into(),
        "100%".into(),
        "-d".into(),
        cwd.to_string_lossy().to_string(),
        "-e".into(),
        format!("TMUX_PANE={pane}"),
        "-t".into(),
        window,
        // Two positionals: tmux execs them directly, no `sh -c` (tmux
        // 3.8) — the raw path is safe, quoting would corrupt argv[0].
        exe.to_string_lossy().to_string(),
        "overview".into(),
    ];
    std::thread::spawn(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let _ = tmux::run_tmux_blocking(&args);
    });
    Ok(())
}

fn run_tui(interval: Duration) -> io::Result<i32> {
    // One tmux spawn for the user's palette; the overview renders with the
    // same status/agent colors as the sidebar.
    let theme = ColorTheme::from_tmux();
    enable_raw_mode()?;
    let result = (|| -> io::Result<i32> {
        // Bracketed paste is part of the drive contract: without it a
        // terminal paste arrives as a key run (submitting multi-line text
        // line by line), with it crossterm delivers Event::Paste blocks
        // that forward as one `paste-buffer` (see the Event::Paste arm).
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;

        let mut panes: Vec<OverviewPane> = Vec::new();
        // Selection and zoom track pane ids, not indexes: the grid is
        // status-sorted, so a refresh can reorder cells and an index would
        // silently move the selection onto a different agent.
        let mut selected: Option<String> = None;
        let mut zoomed: Option<String> = None;
        // Focus mode's temporary window-zoom (audit 2026-11-25 M-cli-5),
        // held across the loop so every exit path restores the pre-zoom
        // layout: Drop covers the returns and `?`s, while the Esc
        // collapse, the vanished-pane tidy-up, and the step-into-the-pane
        // paths restore eagerly.
        let mut window_zoom: Option<WindowZoom> = None;
        // Grid order: the sidebar's own row order by default, `s` to
        // status-first. The git cache backs the sidebar grouping.
        let mut sort_mode = SortMode::Sidebar;
        let mut git_cache: HashMap<String, PaneGitInfo> = HashMap::new();
        // The bottom bar: prompt mode (`p`) while it owns the keyboard
        // (see DriveInput).
        let mut drive: Option<DriveInput> = None;
        // Prompt mode's own refresh clock: the prompted pane's capture
        // refreshes at DRIVE_INTERVAL while everything else paces to
        // `interval`.
        let mut last_drive_refresh = Instant::now();
        // Running state for the paste-Enter debounce (audit 2026-11-25
        // L-cli-3): the instant the last Enter was forwarded, reset by
        // any non-Enter key.
        let mut last_enter_forward: Option<Instant> = None;
        // Refresh on the first iteration: the elapsed time starts "in the past".
        let mut last_refresh = Instant::now() - interval;
        // The listing + process snapshot run on a worker thread, the same
        // shape as the sidebar's snapshot worker: a full-system `ps` scan
        // armed by the listing used to run inline on the UI thread every
        // tick, and under ps contention it froze the grid for up to the
        // subprocess timeout (audit 2026-11-25 M-cli-1). At most one
        // request is outstanding; a failed listing keeps the previous
        // frame.
        let (snapshot_req_tx, snapshot_req_rx) = mpsc::channel::<()>();
        let (snapshot_res_tx, snapshot_res_rx) = mpsc::channel::<OverviewSnapshotRaw>();
        std::thread::spawn(move || {
            while snapshot_req_rx.recv().is_ok() {
                let panes_output = tmux::fetch_pane_listing();
                let process_snapshot = panes_output
                    .as_ref()
                    .and_then(|output| tmux::process_snapshot_for_panes(output));
                if snapshot_res_tx
                    .send(OverviewSnapshotRaw {
                        panes_output,
                        process_snapshot,
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        let mut snapshot_in_flight = false;
        // The pane geometry the last successful refresh captured at; the
        // drive paths read their dims from here instead of re-scanning the
        // whole server (audit 2026-11-25 M-cli-3).
        let mut last_dims: HashMap<String, (u16, u16)> = HashMap::new();

        loop {
            // Prompt mode first: the prompted pane's capture refreshes at
            // the fast cadence so the agent's own menus render live inside
            // the cell. A failed fast capture keeps the previous frame —
            // one tmux hiccup must not flash "(pane gone)" mid-keystroke.
            if let Some(input) = drive.as_ref()
                && last_drive_refresh.elapsed() >= DRIVE_INTERVAL
            {
                let (pane_id, dims) = (input.pane_id.clone(), input.dims);
                if let Some(pane) = panes.iter_mut().find(|p| p.info.pane_id == pane_id)
                    && let Ok(grid) = capture_grid(&pane.info, dims)
                {
                    pane.grid = Some(grid);
                    pane.err = None;
                }
                last_drive_refresh = Instant::now();
            }

            if last_refresh.elapsed() >= interval {
                // Request a refresh; the apply happens below when the
                // worker's raw listing lands. `last_refresh` advances at
                // request time so the cadence is independent of the
                // worker's latency.
                if !snapshot_in_flight && snapshot_req_tx.send(()).is_ok() {
                    snapshot_in_flight = true;
                }
                last_refresh = Instant::now();
            }

            // Apply a landed snapshot: parse, capture, sort — the same
            // work the inline refresh used to do, minus the ps scan.
            if let Ok(raw) = snapshot_res_rx.try_recv() {
                snapshot_in_flight = false;
                // A failed listing keeps the previous frame instead of
                // blanking the grid on one transient tmux hiccup.
                if let Some(listing) = &raw.panes_output {
                    let (next, dims) = build_overview_panes(
                        listing,
                        raw.process_snapshot,
                        sort_mode,
                        &mut git_cache,
                    );
                    panes = next;
                    last_dims = dims;
                    // A selection whose pane vanished clears entirely: the
                    // grid starts with nobody selected, and a selection only
                    // exists once the user moves.
                    if selected
                        .as_ref()
                        .is_some_and(|id| !panes.iter().any(|p| &p.info.pane_id == id))
                    {
                        selected = None;
                    }
                    if let Some(id) = zoomed.clone()
                        && !panes.iter().any(|p| p.info.pane_id == id)
                    {
                        zoomed = None;
                        // The zoomed pane left the listing: lift the
                        // temporary zoom — a no-op when the pane (and with
                        // it tmux's own zoom) died, the point when
                        // detection flickered while the pane lives.
                        if let Some(zoom) = window_zoom.take() {
                            zoom.restore();
                        }
                    }
                    // A prompt whose pane is no longer drivable closes itself:
                    // it must not sit on a pane a shell now owns — the per-key
                    // gate below is the hard guarantee, this is the tidy-up
                    // (the listing field can be up to one refresh stale).
                    let stale_prompt = match drive.as_ref() {
                        Some(input) => match panes.iter().find(|p| p.info.pane_id == input.pane_id)
                        {
                            None => true,
                            Some(p) => tmux::is_shell_command(&p.info.current_command),
                        },
                        None => false,
                    };
                    if stale_prompt {
                        drive = None;
                    }
                    // Refresh the prompted pane's cached dimensions from the
                    // geometry this refresh already collected, so a resized
                    // pane parses correctly again within a second (audit
                    // 2026-11-25 M-cli-3).
                    if let Some(input) = drive.as_mut() {
                        input.dims = last_dims.get(&input.pane_id).copied();
                    }
                    // The slow refresh re-captured every pane including the
                    // prompted one; its fast clock restarts from here.
                    if drive.is_some() {
                        last_drive_refresh = Instant::now();
                    }
                }
            }

            let selected_idx = selected
                .as_ref()
                .and_then(|id| panes.iter().position(|p| &p.info.pane_id == id));
            let zoom_idx = zoomed
                .as_ref()
                .and_then(|id| panes.iter().position(|p| &p.info.pane_id == id));
            terminal.draw(|f| match zoom_idx {
                Some(i) => draw_zoomed(f, &panes, i, drive.as_ref(), &theme),
                None => draw_grid(f, &panes, selected_idx, drive.as_ref(), sort_mode, &theme),
            })?;

            let driving = drive.is_some();
            let budget = if driving {
                // Redraws pace to the drive capture; keys wake the poll
                // instantly, so typing latency is unaffected.
                DRIVE_INTERVAL
            } else {
                interval
                    .checked_sub(last_refresh.elapsed())
                    .unwrap_or(Duration::from_millis(30))
                    .max(Duration::from_millis(30))
            };
            if event::poll(budget)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        // While prompting, the keys go to the agent.
                        let mut leave = false;
                        match drive.as_mut() {
                            Some(input) => match handle_drive_key(&key) {
                                DriveAction::Leave => leave = true,
                                DriveAction::Forward => {
                                    // Any keystroke retires a stale refusal;
                                    // the gate re-decides on every key.
                                    input.err = None;
                                    if let Some(reason) = drive_refusal(&panes, &input.pane_id) {
                                        input.err = Some(reason);
                                    } else if let Some(forwarded) =
                                        forward_for_drive(&key, &mut last_enter_forward)
                                    {
                                        if let Err(e) = match forwarded {
                                            ForwardedKey::Named(name) => {
                                                tmux::send_key(&input.pane_id, &name)
                                            }
                                            ForwardedKey::Text(text) => {
                                                tmux::send_text(&input.pane_id, &text)
                                            }
                                        } {
                                            input.err = Some(e);
                                        }
                                    }
                                }
                            },
                            None => {
                                match key.code {
                                    KeyCode::Char('q') => return Ok(0),
                                    KeyCode::Char('c')
                                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                    {
                                        return Ok(0);
                                    }
                                    // Zoom collapses first; Esc only quits from the grid.
                                    KeyCode::Esc => {
                                        if zoomed.take().is_none() {
                                            return Ok(0);
                                        }
                                        // Collapsing to the grid ends focus
                                        // mode: lift the temporary window-
                                        // zoom now — browsing the grid must
                                        // not keep the pane's window zoomed
                                        // underneath the popup.
                                        if let Some(zoom) = window_zoom.take() {
                                            zoom.restore();
                                        }
                                    }
                                    KeyCode::Enter => {
                                        // In the grid, Enter zooms. Already zoomed,
                                        // Enter steps into the real pane: read it
                                        // first, then work in it.
                                        if zoomed.is_some() {
                                            if let Some(pane) = target_pane(
                                                &panes,
                                                zoomed.as_deref(),
                                                selected.as_deref(),
                                            ) {
                                                // Landing in the real pane ends
                                                // focus mode: lift the temporary
                                                // zoom first, so the client
                                                // arrives at the normal layout
                                                // instead of watching it unzoom a
                                                // beat later.
                                                if let Some(zoom) = window_zoom.take() {
                                                    zoom.restore();
                                                }
                                                tmux::select_pane(&pane.info.pane_id);
                                                return Ok(0);
                                            }
                                        } else if let Some(idx) = selected_idx {
                                            if let Some(zoom) =
                                                enter_focus_zoom(&mut panes[idx], &mut last_dims)
                                            {
                                                window_zoom = Some(zoom);
                                            }
                                            zoomed = Some(panes[idx].info.pane_id.clone());
                                        }
                                    }
                                    KeyCode::Char('p') => {
                                        // Prompt: forward every key to the
                                        // target until ctrl-g. Works from the
                                        // grid (selected cell) and zoomed — the
                                        // zoom state never changes, the cell
                                        // keeps its size. Entry refuses a pane
                                        // the listing says is not an agent;
                                        // the per-key gate is the fresh
                                        // guarantee.
                                        if let Some(pane) = target_pane(
                                            &panes,
                                            zoomed.as_deref(),
                                            selected.as_deref(),
                                        ) && prompt_refusal(&panes, &pane.info.pane_id).is_none()
                                        {
                                            drive = Some(DriveInput {
                                                pane_id: pane.info.pane_id.clone(),
                                                label: prompt_label(&pane.info),
                                                // From the geometry the last refresh
                                                // collected — a full-server
                                                // `list-panes` scan just to learn one
                                                // pane's dims re-read data the retained
                                                // map already had (audit 2026-11-25
                                                // M-cli-3).
                                                dims: last_dims.get(&pane.info.pane_id).copied(),
                                                err: None,
                                            });
                                            // Fresh session, fresh paste-run window
                                            // (audit 2026-11-25 L-cli-3).
                                            last_enter_forward = None;
                                            // Capture immediately: the view may
                                            // be a slow tick stale, and prompting
                                            // types into what it shows.
                                            last_drive_refresh = Instant::now() - DRIVE_INTERVAL;
                                        }
                                    }
                                    KeyCode::Char('f') => {
                                        // Focus: close the popup and hand the
                                        // client to the agent's real pane.
                                        if let Some(pane) = target_pane(
                                            &panes,
                                            zoomed.as_deref(),
                                            selected.as_deref(),
                                        ) {
                                            // Same as the Enter-go path: lift
                                            // the temporary zoom before the
                                            // client lands.
                                            if let Some(zoom) = window_zoom.take() {
                                                zoom.restore();
                                            }
                                            tmux::select_pane(&pane.info.pane_id);
                                            return Ok(0);
                                        }
                                    }
                                    KeyCode::Char('s') => {
                                        sort_mode = sort_mode.toggled();
                                        // Re-order on the next tick instead of
                                        // waiting out the interval.
                                        last_refresh = Instant::now() - interval;
                                    }
                                    KeyCode::Right | KeyCode::Tab | KeyCode::Char('j') => {
                                        selected = step_selection(&panes, selected, 1);
                                    }
                                    KeyCode::Left | KeyCode::BackTab | KeyCode::Char('k') => {
                                        selected = step_selection(&panes, selected, -1);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        if leave {
                            drive = None;
                        }
                    }
                    Event::Paste(text) => {
                        // Bracketed paste routes to prompt mode: the whole
                        // block forwards as one `paste-buffer` — never a key
                        // run, or a multi-line paste would submit line by
                        // line. The gate applies: a paste is text too, and
                        // text must never reach a shell.
                        if let Some(input) = drive.as_mut() {
                            input.err = None;
                            if let Some(reason) = drive_refusal(&panes, &input.pane_id) {
                                input.err = Some(reason);
                            } else if let Err(e) = tmux::paste_to_pane(&input.pane_id, &text) {
                                input.err = Some(e);
                            }
                        }
                    }
                    Event::Mouse(mouse)
                        if mouse.kind == MouseEventKind::Down(MouseButton::Left) =>
                    {
                        // Prompting, a click is the mouse way out (ctrl-g is
                        // the keyboard one); the click itself is consumed —
                        // leaving a modal must not also select or quit
                        // through it.
                        if drive.is_some() {
                            drive = None;
                            continue;
                        }
                        let size = terminal.size()?;
                        if header_close_hit(mouse.row, mouse.column, size.width) {
                            return Ok(0);
                        }
                        // Grid-body click selects; clicking the already-
                        // selected cell zooms it (Esc collapses). While
                        // prompting owns the keyboard, body clicks stay
                        // inert.
                        if zoomed.is_none() {
                            let area = Rect::new(0, 0, size.width, size.height);
                            let body = grid_body(area, false);
                            if let Some(idx) = cell_at(panes.len(), body, mouse.row, mouse.column) {
                                let id = panes.get(idx).map(|p| p.info.pane_id.clone());
                                if let Some(id) = id {
                                    if selected.as_deref() == Some(id.as_str()) {
                                        // Click-zoom takes the same temporary
                                        // window-zoom as Enter.
                                        if let Some(zoom) =
                                            enter_focus_zoom(&mut panes[idx], &mut last_dims)
                                        {
                                            window_zoom = Some(zoom);
                                        }
                                        zoomed = Some(id);
                                    } else {
                                        selected = Some(id);
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    })();

    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste
    );
    result
}

/// The header row's interactive zones: the last two columns are the `✕`
/// close button, and the first [`COLLAPSE_BUTTON_WIDTH`] columns are the
/// `«` collapse button — the same spot as the sidebar's own `☰`, so the
/// trip back is where the trip in was. Both quit the popup.
fn header_close_hit(row: u16, col: u16, width: u16) -> bool {
    row == 0 && width >= 2 && (col < COLLAPSE_BUTTON_WIDTH || col >= width.saturating_sub(2))
}

/// Columns reserved at the header row's left edge for the `«` collapse
/// button — mirroring the sidebar header's `☰` zone.
const COLLAPSE_BUTTON_WIDTH: u16 = 3;

/// Move the selection `delta` cells with wraparound, over the status-sorted
/// pane list. Pane ids keep the selection anchored across refreshes.
fn step_selection(panes: &[OverviewPane], current: Option<String>, delta: i32) -> Option<String> {
    if panes.is_empty() {
        return None;
    }
    let n = panes.len() as i32;
    let idx = match current
        .as_ref()
        .and_then(|id| panes.iter().position(|p| &p.info.pane_id == id))
    {
        Some(i) => (i as i32 + delta).rem_euclid(n) as usize,
        // Nothing selected yet: Right picks the first cell, Left the last.
        None => {
            if delta >= 0 {
                0
            } else {
                n as usize - 1
            }
        }
    };
    Some(panes[idx].info.pane_id.clone())
}

/// The raw outputs of one overview snapshot pass, produced on the worker
/// thread (see `run_tui`). `panes_output` is `None` when the `list-panes`
/// spawn failed — the caller keeps its previous frame.
struct OverviewSnapshotRaw {
    panes_output: Option<String>,
    process_snapshot: Option<crate::process::ProcessSnapshot>,
}

/// One refresh cycle: the standard single `list-panes -a` sync for the agent
/// set, one geometry query, then one capture per agent pane. The listing and
/// the armed `ps` scan arrive from the snapshot worker; everything here runs
/// on the UI thread. Returns the built cells plus the pane-geometry map the
/// captures used, so the drive paths can read dims without re-scanning the
/// server (audit 2026-11-25 M-cli-3).
fn build_overview_panes(
    listing: &str,
    process_snapshot: Option<crate::process::ProcessSnapshot>,
    sort_mode: SortMode,
    git_cache: &mut HashMap<String, PaneGitInfo>,
) -> (Vec<OverviewPane>, HashMap<String, (u16, u16)>) {
    let snapshot = tmux::build_session_snapshot_with(listing, "%overview-none", process_snapshot);
    // Resolved per tick so flipping the sidebar's grouping mode (s in the
    // sidebar, or a tmux.conf change) lands on the next grid refresh.
    let group_by = GroupBy::from_label(
        tmux::run_tmux(&["show-options", "-g", "@sidebar_group_by"])
            .unwrap_or_default()
            .trim(),
    );

    let mut infos = Vec::new();
    let mut seen = HashSet::new();
    for session in &snapshot.sessions {
        for window in &session.windows {
            for info in &window.panes {
                if info.agent == AgentType::Unknown {
                    continue;
                }
                // Grouped sessions can surface the same pane twice; the
                // pane id is the dedup key.
                if seen.insert(info.pane_id.clone()) {
                    infos.push(info.clone());
                }
            }
        }
    }

    // Status-first: waiting/error cells lead the grid so "does anything
    // need me?" is answerable from the top-left. Stable sort keeps tmux
    // order within a class.
    let dims = pane_dimensions();
    let mut panes: Vec<OverviewPane> = infos
        .into_iter()
        .map(|info| {
            let captured = capture_grid(&info, dims.get(&info.pane_id).copied());
            OverviewPane {
                err: captured.as_ref().err().cloned(),
                grid: captured.ok(),
                info,
            }
        })
        .collect();
    match sort_mode {
        SortMode::Status => sort_for_grid(&mut panes),
        SortMode::Sidebar => sort_like_sidebar(&mut panes, &snapshot.sessions, group_by, git_cache),
    }
    (panes, dims)
}

/// Status-first cell order (see status_rank); stable within a class.
fn sort_for_grid(panes: &mut [OverviewPane]) {
    panes.sort_by_key(|p| status_rank(&p.info.status));
}

/// Which order the grid lays cells out in. Sidebar order is the default:
/// position is memory — the cell you just prompted stays where you left
/// it. Status-first is opt-in (`s`) for the "does anything need me?"
/// glance the spike designed around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortMode {
    Sidebar,
    Status,
}

impl SortMode {
    fn toggled(self) -> Self {
        match self {
            SortMode::Sidebar => SortMode::Status,
            SortMode::Status => SortMode::Sidebar,
        }
    }
}

/// Order the cells exactly like the sidebar's agent rows: reuse the
/// sidebar's own grouping (repo groups — or session groups under
/// `@sidebar_group_by session` — sorted by display name) and its within-
/// group key (session name, then window index, stable so panes keep tmux
/// line order inside a window). Panes the grouping never lists (removed
/// between the listing and now) fall back to their encounter order at the
/// end, so the grid never drops a cell over ordering alone.
///
/// Grouping needs the same git metadata the sidebar's background worker
/// caches; the overview has no worker, so the cache is filled here — one
/// resolve per new grouping anchor, then free.
fn sort_like_sidebar(
    panes: &mut [OverviewPane],
    sessions: &[SessionInfo],
    group_by: GroupBy,
    git_cache: &mut HashMap<String, PaneGitInfo>,
) {
    for pane in panes.iter() {
        let anchor = pane.info.grouping_anchor().to_string();
        if !git_cache.contains_key(&anchor) {
            git_cache.insert(anchor.clone(), resolve_pane_git_info(&anchor));
        }
    }
    let groups = match group_by {
        GroupBy::Repo => group_panes_by_repo(sessions, git_cache),
        GroupBy::Session => group_panes_by_session(sessions, &[], git_cache, false),
    };
    let mut rank: HashMap<String, usize> = HashMap::new();
    let mut next_rank = 0usize;
    for group in groups {
        let mut members: Vec<&PaneInfo> = group.panes.iter().map(|(info, _)| info).collect();
        // Same comparator as group::ordered_group_rows, minus the
        // non-agent window rows the grid never shows.
        members.sort_by_key(|p| (p.tmux_session.as_str(), p.window_index));
        for info in members {
            if !rank.contains_key(&info.pane_id) {
                rank.insert(info.pane_id.clone(), next_rank);
                next_rank += 1;
            }
        }
    }
    panes.sort_by_key(|p| rank.get(&p.info.pane_id).copied().unwrap_or(usize::MAX));
}

/// Sort priority for grid cells: the states that can demand the user's
/// attention lead, finished/idle states trail.
fn status_rank(status: &PaneStatus) -> u8 {
    match status {
        PaneStatus::Waiting => 0,
        PaneStatus::Error => 1,
        PaneStatus::Running => 2,
        PaneStatus::Background => 3,
        PaneStatus::Idle => 4,
        PaneStatus::Unknown => 5,
    }
}

/// `pane_id -> (width, height)` for every pane, in one `list-panes -a` call.
/// The capture parser needs the source pane's exact dimensions so its grid
/// matches the terminal the agent is actually drawing to.
fn pane_dimensions() -> HashMap<String, (u16, u16)> {
    let Some(out) = tmux::run_tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_id},#{pane_width},#{pane_height}",
    ]) else {
        return HashMap::new();
    };
    out.lines()
        .filter_map(|line| {
            let mut fields = line.split(',');
            let id = fields.next()?.to_string();
            let width = fields.next()?.parse().ok()?;
            let height = fields.next()?.parse().ok()?;
            Some((id, (width, height)))
        })
        .collect()
}

fn capture_grid(info: &PaneInfo, dims: Option<(u16, u16)>) -> Result<Vec<Vec<StyledCell>>, String> {
    let bytes = capture_pane(&info.pane_id)?;
    // If the pane vanished between the listing and the capture, fall back
    // to a common size; the next tick corrects it.
    let (cols, rows) = dims.unwrap_or((80, 24));
    let mut grid = ansi::parse_ansi(&bytes, cols, rows);
    normalize_captured_grid(&mut grid);
    Ok(grid)
}

/// Strip light backgrounds and lift dark foregrounds so every captured
/// cell reads on the dark grid canvas.
///
/// Agent TUIs bring their own themes: opencode's light-pink panels and
/// claude's white input bar arrive as explicit light backgrounds with dark
/// text — a pink-on-dark clash, and invisible text the moment the
/// background is gone. Two rules:
///
/// - a pane that paints mostly explicit light backgrounds is a
///   light-themed TUI: strip ALL of its backgrounds and lift dark or
///   absent foregrounds to light grey (already-light fg keeps its hue);
/// - otherwise strip only per-cell light backgrounds (claude's white
///   input bar) and lift that cell's dark fg; dark-theme cells pass
///   through untouched.
fn normalize_captured_grid(grid: &mut [Vec<StyledCell>]) {
    let light_pane = pane_is_light_theme(grid);
    for row in grid.iter_mut() {
        for cell in row.iter_mut() {
            let Some(bg) = cell.bg else { continue };
            let light_bg = indexed_luminance(bg) >= LIGHT_BG_LUMINANCE;
            if !light_pane && !light_bg {
                continue;
            }
            cell.bg = None;
            let lift = match cell.fg {
                None => true,
                Some(fg) => indexed_luminance(fg) < DARK_FG_LUMINANCE,
            };
            if lift {
                cell.fg = Some(LIFTED_FG);
            }
        }
    }
}

/// True when most of a captured screen paints an explicit light background
/// — the signature of a light-themed TUI whose dark text needs lifting
/// onto the dark canvas.
fn pane_is_light_theme(grid: &[Vec<StyledCell>]) -> bool {
    let mut light = 0usize;
    let mut explicit = 0usize;
    for row in grid {
        for cell in row {
            if let Some(bg) = cell.bg {
                explicit += 1;
                if indexed_luminance(bg) >= LIGHT_BG_LUMINANCE {
                    light += 1;
                }
            }
        }
    }
    explicit > 0 && light * 2 > explicit
}

/// Relative luminance (0-1) of an xterm-256 palette index.
fn indexed_luminance(color: u8) -> f32 {
    let (r, g, b) = palette_rgb(color);
    // Rec. 709 luminance weights on a 0-255 scale.
    (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
}

/// The grid body area: everything under the single-line header. An open
/// prompt bar claims one more row off the bottom.
fn grid_body(size: Rect, prompt_open: bool) -> Rect {
    let header = 1u16.min(size.height);
    let bar = u16::from(prompt_open).min(size.height.saturating_sub(header));
    Rect {
        x: size.x,
        y: size.y + header,
        width: size.width,
        height: size.height.saturating_sub(header + bar),
    }
}

/// Every cell's rect in the grid body, in pane order. One derivation is
/// shared by rendering and click hit-testing, so where a cell draws and
/// where its click lands can never drift apart.
fn cell_rects(n: usize, body: Rect) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let (cols, rows) = grid_dims(n, body);
    let row_rects = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Fill(1); rows as usize])
        .split(body);
    let mut rects = Vec::with_capacity(n);
    for row_rect in row_rects.iter() {
        let col_rects = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Fill(1); cols as usize])
            .split(*row_rect);
        rects.extend(col_rects.iter().copied());
    }
    rects.truncate(n);
    rects
}

/// The cell under a grid-mode mouse position, borders included — the
/// click-to-select hit test. None outside the grid body.
fn cell_at(n: usize, body: Rect, row: u16, col: u16) -> Option<usize> {
    cell_rects(n, body)
        .into_iter()
        .position(|rect| rect.contains(Position::new(col, row)))
}

fn draw_grid(
    f: &mut Frame,
    panes: &[OverviewPane],
    selected_idx: Option<usize>,
    drive: Option<&DriveInput>,
    sort_mode: SortMode,
    theme: &ColorTheme,
) {
    let area = f.area();
    if area.width < 4 || area.height < 3 {
        return;
    }

    if panes.is_empty() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .split(area);
        let empty = Paragraph::new("no agent panes")
            .alignment(ratatui::layout::Alignment::Center)
            .style(Style::default().fg(theme.border_inactive));
        f.render_widget(empty, chunks[1]);
        return;
    }

    let mut constraints = vec![Constraint::Length(1), Constraint::Fill(1)];
    if drive.is_some() {
        constraints.push(Constraint::Length(1));
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let width = area.width;
    // Prompting, the only key still ours is the leave chord — Esc and
    // everything else belong to the agent, including q.
    let hints = if drive.is_some() {
        DRIVE_HINTS
    } else {
        grid_hints(sort_mode)
    };
    let counts = header_line(panes, theme, hints, drive.is_none(), width);
    f.render_widget(Paragraph::new(counts), chunks[0]);

    let rects = cell_rects(panes.len(), chunks[1]);
    for (idx, cell_area) in rects.iter().enumerate() {
        draw_cell(
            f,
            *cell_area,
            &panes[idx],
            selected_idx == Some(idx),
            theme,
            false,
        );
    }
    if let Some(input) = drive {
        f.render_widget(Paragraph::new(drive_line(input, theme)), chunks[2]);
    }
}

/// Zoomed view: the selected pane's capture rendered into one fullscreen
/// cell. Same renderer, same rules — just no cropping pressure. While
/// prompting, the hints shrink to the one key that is still ours.
fn draw_zoomed(
    f: &mut Frame,
    panes: &[OverviewPane],
    idx: usize,
    drive: Option<&DriveInput>,
    theme: &ColorTheme,
) {
    let area = f.area();
    if area.width < 4 || area.height < 3 {
        return;
    }
    let mut constraints = vec![Constraint::Length(1), Constraint::Fill(1)];
    if drive.is_some() {
        constraints.push(Constraint::Length(1));
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let width = area.width;
    let hints = if drive.is_some() {
        DRIVE_HINTS
    } else {
        ZOOM_HINTS
    };
    let counts = header_line(panes, theme, hints, drive.is_none(), width);
    f.render_widget(Paragraph::new(counts), chunks[0]);
    // The zoomed cell centers a source narrower than the area — the
    // multi-client fallback where the pane's window cannot reach the
    // popup's width even zoomed.
    draw_cell(f, chunks[1], &panes[idx], true, theme, true);
    if let Some(input) = drive {
        f.render_widget(Paragraph::new(drive_line(input, theme)), chunks[2]);
    }
}

/// Pick columns/rows for `n` panes: as many columns as fit at the minimum
/// cell width, then grow columns (shrinking cells) until every row fits the
/// terminal height at the minimum cell height. The spike does not paginate;
/// with more agents than fit, cells just get small.
fn grid_dims(n: usize, area: Rect) -> (u16, u16) {
    let mut cols = ((area.width / MIN_CELL_WIDTH).max(1) as usize).min(n);
    let mut rows = n.div_ceil(cols);
    while rows > 1 && rows * MIN_CELL_HEIGHT as usize > area.height as usize && cols < n {
        cols += 1;
        rows = n.div_ceil(cols);
    }
    (cols as u16, rows as u16)
}

fn draw_cell(
    f: &mut Frame,
    area: Rect,
    pane: &OverviewPane,
    selected: bool,
    theme: &ColorTheme,
    centered: bool,
) {
    let status_color = theme.status_color(&pane.info.status);
    let mut border = match pane.info.status {
        // States that can demand the user's attention get the loud border;
        // finished/idle cells fade to grey so they stop competing.
        PaneStatus::Waiting | PaneStatus::Error => Style::default()
            .fg(status_color)
            .add_modifier(Modifier::BOLD),
        PaneStatus::Idle | PaneStatus::Unknown => Style::default().fg(theme.border_inactive),
        PaneStatus::Running | PaneStatus::Background => Style::default().fg(status_color),
    };
    if selected {
        // The selection must be unmistakable at grid scale: the theme's
        // accent color, not a bolder status border — bold on the grey idle
        // border was indistinguishable from unselected.
        border = Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD);
    }
    let block = Block::bordered()
        .title(title_line(&pane.info, theme))
        .border_style(border);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // One activity strip under the title; the captured screen fills the
    // rest, bottom-anchored.
    let (strip, content) = if inner.height > 1 {
        (
            Rect { height: 1, ..inner },
            Rect {
                y: inner.y + 1,
                height: inner.height - 1,
                ..inner
            },
        )
    } else {
        (inner, Rect { height: 0, ..inner })
    };
    let activity = activity_line(&pane.info, theme);
    f.render_widget(Paragraph::new(activity), strip);

    match &pane.grid {
        Some(grid) => paint_grid(f.buffer_mut(), content, grid, centered),
        None => {
            let msg = pane.err.as_deref().unwrap_or("(pane gone)");
            let failed = Paragraph::new(msg).style(Style::default().fg(theme.border_inactive));
            f.render_widget(failed, content);
        }
    }
}

/// Header key-hint segments for grid mode, in degradation order (the tail
/// drops first when the row narrows). The last segment is the sort toggle
/// pointing at the mode you do not have.
fn grid_hints(sort_mode: SortMode) -> &'static [&'static str] {
    match sort_mode {
        SortMode::Sidebar => &["tab move", "enter zoom", "p prompt", "f go", "s status"],
        SortMode::Status => &["tab move", "enter zoom", "p prompt", "f go", "s sidebar"],
    }
}

/// Header key-hint segments for zoomed mode.
const ZOOM_HINTS: &[&str] = &["esc grid", "enter go", "p prompt"];

/// Header key-hint segments while prompting: the only key still ours is
/// the leave chord — Esc and everything else belong to the agent.
const DRIVE_HINTS: &[&str] = &["ctrl-g leave"];

/// Header row: per-status counts (only non-zero classes) so "does anything
/// need me?" is one glance, plus the key hints and a clickable `✕` pinned
/// to the last two columns for closing with the mouse. `quit_hint` —
/// while prompting, q belongs to the agent (it forwards), so the "q quit"
/// hint would lie and must drop.
fn header_line(
    panes: &[OverviewPane],
    theme: &ColorTheme,
    optional_hints: &[&str],
    quit_hint: bool,
    width: u16,
) -> Line<'static> {
    let muted = Style::default().fg(theme.border_inactive);
    let mut spans = Vec::new();
    // The `«` collapse button leads the row — the same corner the
    // sidebar's `☰` lives in, so opening and closing share muscle memory.
    spans.push(Span::styled("«", muted));
    spans.push(Span::styled(
        " ".repeat(COLLAPSE_BUTTON_WIDTH as usize - 1),
        muted,
    ));
    for status in [
        PaneStatus::Waiting,
        PaneStatus::Error,
        PaneStatus::Running,
        PaneStatus::Background,
        PaneStatus::Idle,
        PaneStatus::Unknown,
    ] {
        let count = panes.iter().filter(|p| p.info.status == status).count();
        if count > 0 {
            spans.push(Span::styled(
                format!(" {}{count}", status.icon()),
                Style::default().fg(theme.status_color(&status)),
            ));
        }
    }
    // Key hints degrade by dropping whole segments when the row gets tight;
    // `q quit` is the one everyone needs (except while prompting — see the
    // doc), so it survives even when the rest is squeezed out, and the `✕`
    // keeps its pinned columns at any width.
    const REQUIRED: &str = "q quit";
    use unicode_width::UnicodeWidthStr as _;
    let used_prefix: usize = spans.iter().map(|s| s.width()).sum();
    let hint_budget = (width as usize)
        .saturating_sub(used_prefix)
        .saturating_sub(" ✕".width());
    let mut shown: Vec<&str> = Vec::new();
    let mut hints_width = if quit_hint { REQUIRED.width() } else { 0 };
    for hint in optional_hints {
        // Each extra segment costs its own width plus the " · " separator.
        let cost = hint.width() + 3;
        if hints_width + cost > hint_budget {
            break;
        }
        shown.push(hint);
        hints_width += cost;
    }
    if quit_hint {
        shown.push(REQUIRED);
    }
    spans.push(Span::styled(shown.join(" · "), muted));
    // The close button owns the row's last two columns in every state —
    // the same contract the mouse handler uses for its hit zone.
    let used: usize = spans.iter().map(|s| s.width()).sum();
    let close_col = width.saturating_sub(2) as usize;
    let pad = close_col.saturating_sub(used);
    spans.push(Span::styled(" ".repeat(pad), muted));
    spans.push(Span::styled(" ✕", muted));
    Line::from(spans)
}

/// Cell title: status icon + agent label + repo (with a ⎇ worktree marker
/// when the pane lives in one), matching the sidebar's row vocabulary.
fn title_line(info: &PaneInfo, theme: &ColorTheme) -> Line<'static> {
    let status = Style::default().fg(theme.status_color(&info.status));
    let agent = Style::default()
        .fg(theme.agent_color(&info.agent))
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(theme.border_inactive);

    let mut spans = vec![
        Span::styled(format!(" {} ", info.status.icon()), status),
        Span::styled(info.agent.label().to_string(), agent),
    ];
    let repo = repo_label(info);
    if !repo.is_empty() {
        spans.push(Span::styled(format!(" · {repo}"), muted));
    }
    Line::from(spans)
}

fn repo_label(info: &PaneInfo) -> String {
    let anchor = info.grouping_anchor();
    let base = anchor.rsplit('/').next().unwrap_or(anchor);
    if info.worktree.name.is_empty() {
        base.to_string()
    } else {
        format!("{base} ⎇ {}", info.worktree.name)
    }
}

/// `agent · repo` label for the prompt bar, from the same vocabulary as the
/// cell title so the bar names the pane the way the grid labels it.
fn prompt_label(info: &PaneInfo) -> String {
    let repo = repo_label(info);
    if repo.is_empty() {
        info.agent.label().to_string()
    } else {
        format!("{} · {repo}", info.agent.label())
    }
}

/// The prompt bar: names the pane every key is hitting and how to leave.
/// There is no draft to window around a cursor — the agent owns the input
/// line and its echo lands in the cell above — so the bar is a fixed
/// indicator; a refused or failed forward appends its reason in the error
/// color, and the next key re-decides.
fn drive_line(drive: &DriveInput, theme: &ColorTheme) -> Line<'static> {
    let label = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled(
        format!(" ▶ PROMPT {} · ctrl-g leave", drive.label),
        label,
    )];
    if let Some(err) = &drive.err {
        spans.push(Span::styled(
            format!("  {err}"),
            Style::default().fg(theme.status_error),
        ));
    }
    Line::from(spans)
}

/// One-line status under the title: the wait reason for waiting/error panes,
/// else the agent's last prompt/response line. Answers "what is this one
/// doing?" without reading the captured screen.
fn activity_line(info: &PaneInfo, theme: &ColorTheme) -> Line<'static> {
    let muted = Style::default().fg(theme.border_inactive);
    match info.status {
        PaneStatus::Waiting => {
            let reason = if info.wait_reason.is_empty() {
                "input".to_string()
            } else {
                info.wait_reason.clone()
            };
            return Line::from(Span::styled(
                format!(" Waiting · {reason}"),
                Style::default().fg(theme.status_waiting),
            ));
        }
        PaneStatus::Error => {
            let reason = if info.wait_reason.is_empty() {
                "failed".to_string()
            } else {
                info.wait_reason.clone()
            };
            return Line::from(Span::styled(
                format!(" Error · {reason}"),
                Style::default().fg(theme.status_error),
            ));
        }
        PaneStatus::Running | PaneStatus::Background | PaneStatus::Idle | PaneStatus::Unknown => {}
    }
    let text = info.prompt.lines().next().unwrap_or("").trim();
    let body = if text.is_empty() {
        status_word(&info.status).to_string()
    } else {
        format!("{text} …")
    };
    Line::from(Span::styled(format!(" {body}"), muted))
}

fn status_word(status: &PaneStatus) -> &'static str {
    match status {
        PaneStatus::Running => "Running",
        PaneStatus::Background => "Background",
        PaneStatus::Idle => "Idle",
        PaneStatus::Unknown => "",
        // Waiting/Error never reach here (handled above).
        PaneStatus::Waiting | PaneStatus::Error => "",
    }
}

/// Paint a captured styled grid into the cell's content area, bottom-anchored
/// and cropped: when the cell is shorter than the source pane, keep the LAST
/// rows (agent TUIs draw current state at the bottom) and drop the top. The
/// spike does not scale or pan; sources wider than the cell lose their right
/// side. `center` splits a source narrower than the area's residual margin
/// evenly instead of hugging the left edge — focus mode's fallback for
/// sources that cannot reach the popup's width even zoomed (multi-client
/// setups); grid cells keep the left edge, mirroring the pane's own
/// left-anchored geometry in its window.
fn paint_grid(buf: &mut Buffer, area: Rect, grid: &[Vec<StyledCell>], center: bool) {
    let skip = grid.len().saturating_sub(area.height as usize);
    for (y, row) in grid.iter().skip(skip).enumerate() {
        if y as u16 >= area.height {
            break;
        }
        // The parser pads every row to the source's width, so the offset
        // is uniform across rows.
        let x_off = if center {
            area.width.saturating_sub(row.len() as u16) / 2
        } else {
            0
        };
        for (x, cell) in row.iter().enumerate() {
            if x_off + x as u16 >= area.width {
                break;
            }
            let pos = Position::new(area.x + x_off + x as u16, area.y + y as u16);
            if let Some(target) = buf.cell_mut(pos) {
                target.set_char(cell.ch);
                target.set_style(sgr_style(cell));
            }
        }
    }
}

fn sgr_style(cell: &StyledCell) -> Style {
    let mut style = Style::default();
    if let Some(fg) = cell.fg {
        style = style.fg(Color::Indexed(fg));
    }
    if let Some(bg) = cell.bg {
        style = style.bg(Color::Indexed(bg));
    }
    let mut mods = Modifier::empty();
    if cell.bold {
        mods |= Modifier::BOLD;
    }
    if cell.faint {
        mods |= Modifier::DIM;
    }
    if cell.italic {
        mods |= Modifier::ITALIC;
    }
    if cell.reversed {
        mods |= Modifier::REVERSED;
    }
    if cell.underlined {
        mods |= Modifier::UNDERLINED;
    }
    if cell.strike {
        mods |= Modifier::CROSSED_OUT;
    }
    // SGR 53 (overline) has no ratatui modifier; only the HTML renderer
    // shows it.
    style.add_modifier(mods)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::{PaneAttention, PermissionMode, WorktreeMetadata};
    use ratatui::{backend::TestBackend, buffer::Buffer};

    fn make_pane(agent: AgentType, status: PaneStatus, pane_id: &str) -> PaneInfo {
        PaneInfo {
            pane_id: pane_id.into(),
            pane_active: false,
            status,
            attention: PaneAttention::None,
            agent,
            path: "/home/user/project".into(),
            launch_cwd: String::new(),
            current_command: String::new(),
            prompt: String::new(),
            prompt_is_response: false,
            started_at: None,
            wait_reason: String::new(),
            permission_mode: PermissionMode::Default,
            subagents: vec![],
            pane_pid: None,
            worktree: WorktreeMetadata::default(),
            session_id: None,
            session_name: String::new(),
            tmux_session: String::new(),
            window_index: 0,
            window_name: String::new(),
            sidebar_spawned: false,
            bg_shell_cmd: None,
            bg_shell_pid: None,
            pane_title: String::new(),
            flagged: false,
        }
    }

    /// Parse literal ANSI into a grid shaped like a real 24-row source pane
    /// with `content` on the last row (capture output is padded to the
    /// pane's full dimensions, and agent TUIs draw at the bottom).
    fn grid_padded(content: &str) -> Vec<Vec<StyledCell>> {
        let mut text = String::new();
        for _ in 0..23 {
            text.push('\n');
        }
        text.push_str(content);
        ansi::parse_ansi(text.as_bytes(), 40, 24)
    }

    fn render_frame(
        panes: &[OverviewPane],
        selected_idx: Option<usize>,
        drive: Option<&DriveInput>,
        sort_mode: SortMode,
        w: u16,
        h: u16,
    ) -> String {
        let theme = ColorTheme::default();
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_grid(f, panes, selected_idx, drive, sort_mode, &theme))
            .unwrap();
        buffer_to_string(terminal.backend().buffer())
    }

    fn render_zoom(
        panes: &[OverviewPane],
        idx: usize,
        drive: Option<&DriveInput>,
        w: u16,
        h: u16,
    ) -> String {
        let theme = ColorTheme::default();
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_zoomed(f, panes, idx, drive, &theme))
            .unwrap();
        buffer_to_string(terminal.backend().buffer())
    }

    fn buffer_to_string(buf: &Buffer) -> String {
        let area = buf.area;
        let mut lines = Vec::new();
        for y in area.y..area.y + area.height {
            let mut line = String::new();
            for x in area.x..area.x + area.width {
                line.push_str(buf[(x, y)].symbol());
            }
            lines.push(line.trim_end().to_string());
        }
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    #[test]
    fn sgr_style_maps_color_and_attributes() {
        let grid = ansi::parse_ansi(b"\x1b[1;31mX\x1b[0m plain", 8, 1);
        let bold_red = sgr_style(&grid[0][0]);
        assert_eq!(bold_red.fg, Some(Color::Indexed(1)));
        assert!(bold_red.add_modifier.contains(Modifier::BOLD));
        let plain = sgr_style(&grid[0][1]);
        assert_eq!(plain, Style::default());
    }

    #[test]
    fn grid_dims_fits_minimum_height_by_growing_columns() {
        let wide = Rect::new(0, 0, 200, 50);
        assert_eq!(grid_dims(5, wide), (5, 1));

        let normal = Rect::new(0, 0, 80, 24);
        assert_eq!(grid_dims(5, normal), (2, 3));

        // 200x10: rows of 8 do not fit, so it keeps growing columns until
        // one row of five 40-wide cells fits the height.
        let short = Rect::new(0, 0, 200, 10);
        assert_eq!(grid_dims(5, short), (5, 1));
    }

    #[test]
    fn selection_moves_and_wraps() {
        let panes = vec![
            OverviewPane {
                info: make_pane(AgentType::Claude, PaneStatus::Waiting, "%9"),
                grid: None,
                err: None,
            },
            OverviewPane {
                info: make_pane(AgentType::Claude, PaneStatus::Running, "%1"),
                grid: None,
                err: None,
            },
            OverviewPane {
                info: make_pane(AgentType::Claude, PaneStatus::Idle, "%2"),
                grid: None,
                err: None,
            },
        ];
        assert_eq!(step_selection(&panes, None, 1).as_deref(), Some("%9"));
        assert_eq!(step_selection(&panes, None, -1).as_deref(), Some("%2"));
        assert_eq!(
            step_selection(&panes, Some("%9".into()), 1).as_deref(),
            Some("%1")
        );
        assert_eq!(
            step_selection(&panes, Some("%2".into()), 1).as_deref(),
            Some("%9")
        );
        assert_eq!(
            step_selection(&panes, Some("%9".into()), -1).as_deref(),
            Some("%2")
        );
    }

    #[test]
    fn snapshot_single_cell_frame() {
        let mut info = make_pane(AgentType::Claude, PaneStatus::Running, "%1");
        info.prompt = "Editing src/main.rs".into();
        let pane = OverviewPane {
            info,
            grid: Some(grid_padded("\x1b[32m[claude] line one\x1b[0m")),
            err: None,
        };
        insta::assert_snapshot!(render_frame(&[pane], None, None, SortMode::Sidebar, 40, 10), @"
        «   ●1tab move · enter zoom · q quit   ✕
        ┌ ● claude · project───────────────────┐
        │ Editing src/main.rs …                │
        │                                      │
        │                                      │
        │                                      │
        │                                      │
        │                                      │
        │[claude] line one                     │
        └──────────────────────────────────────┘
        ");
    }

    #[test]
    fn snapshot_waiting_sorts_before_running() {
        let mut waiting = make_pane(AgentType::Claude, PaneStatus::Waiting, "%2");
        waiting.wait_reason = "permission".into();
        let mut running = make_pane(AgentType::Claude, PaneStatus::Running, "%1");
        running.prompt = "Reviewing the diff".into();
        let panes = vec![
            OverviewPane {
                info: running,
                grid: Some(grid_padded("\x1b[32m[claude] editing src/main.rs\x1b[0m")),
                err: None,
            },
            OverviewPane {
                info: waiting,
                grid: Some(grid_padded("\x1b[33m[claude] approval prompt\x1b[0m")),
                err: None,
            },
        ];
        // %1 is passed as selected but the waiting pane must sort first.
        let mut panes = panes;
        sort_for_grid(&mut panes);
        insta::assert_snapshot!(render_frame(&panes, Some(0), None, SortMode::Status, 80, 12), @"
        «   ◐1 ●1tab move · enter zoom · p prompt · f go · s sidebar · q quit          ✕
        ┌ ◐ claude · project───────────────────┐┌ ● claude · project───────────────────┐
        │ Waiting · permission                 ││ Reviewing the diff …                 │
        │                                      ││                                      │
        │                                      ││                                      │
        │                                      ││                                      │
        │                                      ││                                      │
        │                                      ││                                      │
        │                                      ││                                      │
        │                                      ││                                      │
        │[claude] approval prompt              ││[claude] editing src/main.rs          │
        └──────────────────────────────────────┘└──────────────────────────────────────┘
        ");
    }

    #[test]
    fn snapshot_failed_capture_shows_error() {
        let pane = OverviewPane {
            info: make_pane(AgentType::Hermes, PaneStatus::Error, "%3"),
            grid: None,
            err: Some("pane vanished".into()),
        };
        insta::assert_snapshot!(render_frame(&[pane], None, None, SortMode::Sidebar, 40, 6), @"
        «   ✕1tab move · enter zoom · q quit   ✕
        ┌ ✕ hermes · project───────────────────┐
        │ Error · failed                       │
        │pane vanished                         │
        │                                      │
        └──────────────────────────────────────┘
        ");
    }

    #[test]
    fn snapshot_zoomed_frame() {
        let mut info = make_pane(AgentType::Codex, PaneStatus::Running, "%1");
        info.prompt = "Refactoring the export path".into();
        let pane = OverviewPane {
            info,
            grid: Some(grid_padded("\x1b[34m[codex] waiting for input\x1b[0m")),
            err: None,
        };
        insta::assert_snapshot!(render_zoom(&[pane], 0, None, 40, 12), @"
        «   ●1esc grid · enter go · q quit     ✕
        ┌ ● codex · project────────────────────┐
        │ Refactoring the export path …        │
        │                                      │
        │                                      │
        │                                      │
        │                                      │
        │                                      │
        │                                      │
        │                                      │
        │[codex] waiting for input             │
        └──────────────────────────────────────┘
        ");
    }

    #[test]
    fn light_theme_pane_is_normalized_to_dark_canvas() {
        // opencode-style: explicit light-pink background everywhere with
        // near-black text, plus one darker panel — the whole pane is a
        // light theme, so every background is stripped and dark text is
        // lifted to light grey.
        let mut grid = vec![vec![StyledCell::default(); 8]; 4];
        for row in grid.iter_mut() {
            for cell in row.iter_mut() {
                cell.bg = Some(218);
                cell.fg = Some(16);
            }
        }
        grid[3][0].bg = Some(236);
        grid[3][0].fg = Some(250);
        normalize_captured_grid(&mut grid);
        for row in grid.iter().flatten() {
            assert_eq!(row.bg, None);
            assert_eq!(row.fg, Some(250));
        }
    }

    #[test]
    fn dark_theme_pane_keeps_its_colors_but_strips_light_bars() {
        // claude-style: dark panels with green text, plus a white input
        // bar row with dark text. The dark theme passes through untouched;
        // only the white bar is stripped and its text lifted.
        let mut grid = vec![vec![StyledCell::default(); 8]; 4];
        for row in grid.iter_mut() {
            for cell in row.iter_mut() {
                cell.bg = Some(236);
                cell.fg = Some(114);
            }
        }
        for cell in grid[3].iter_mut() {
            cell.bg = Some(231);
            cell.fg = Some(16);
        }
        normalize_captured_grid(&mut grid);
        assert_eq!(grid[0][0].bg, Some(236));
        assert_eq!(grid[0][0].fg, Some(114));
        assert_eq!(grid[3][0].bg, None);
        assert_eq!(grid[3][0].fg, Some(250));
    }

    #[test]
    fn luminance_covers_the_256_palette() {
        assert_eq!(indexed_luminance(0), 0.0);
        assert!(indexed_luminance(15) > 0.9);
        assert!(indexed_luminance(231) > 0.9);
        // The grayscale ramp is monotonic and its upper end reads light.
        assert!(indexed_luminance(240) > indexed_luminance(239));
        assert!(indexed_luminance(255) > indexed_luminance(240));
    }

    #[test]
    fn luminance_verdicts_match_the_screenshot_themes() {
        // opencode's light pinks and claude's white input bar are light;
        // claude's dark grey panels and the theme's inactive border (240
        // = #585858) are not.
        assert!(indexed_luminance(218) >= LIGHT_BG_LUMINANCE);
        assert!(indexed_luminance(224) >= LIGHT_BG_LUMINANCE);
        assert!(indexed_luminance(231) >= LIGHT_BG_LUMINANCE);
        assert!(indexed_luminance(250) >= LIGHT_BG_LUMINANCE);
        assert!(indexed_luminance(240) < LIGHT_BG_LUMINANCE);
        assert!(indexed_luminance(236) < LIGHT_BG_LUMINANCE);
        assert!(indexed_luminance(235) < LIGHT_BG_LUMINANCE);
    }

    #[test]
    fn header_close_button_pins_to_the_last_two_columns() {
        let pane = OverviewPane {
            info: make_pane(AgentType::Claude, PaneStatus::Running, "%1"),
            grid: None,
            err: None,
        };
        let line = header_line(
            &[pane],
            &ColorTheme::default(),
            grid_hints(SortMode::Sidebar),
            true,
            40,
        );
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.starts_with("«  "),
            "collapse button leads the row: {text:?}"
        );
        assert!(text.ends_with(" ✕"), "close button pins right: {text:?}");
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(text.as_str()),
            40,
            "the row must not wrap: {text:?}"
        );
        assert!(header_close_hit(0, 38, 40));
        assert!(header_close_hit(0, 39, 40));
        assert!(header_close_hit(0, 0, 40));
        assert!(header_close_hit(0, 2, 40));
        assert!(!header_close_hit(0, 37, 40));
        assert!(!header_close_hit(0, 3, 40), "between the two buttons");
        assert!(!header_close_hit(1, 39, 40), "grid body clicks are inert");
        assert!(!header_close_hit(0, 39, 1), "degenerate width is inert");
    }

    #[test]
    fn header_drops_the_quit_hint_while_prompting() {
        let panes = [OverviewPane {
            info: make_pane(AgentType::Claude, PaneStatus::Running, "%1"),
            grid: None,
            err: None,
        }];
        let theme = ColorTheme::default();
        // While prompting, q forwards to the agent — "q quit" would lie.
        let line = header_line(&panes, &theme, DRIVE_HINTS, false, 60);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("ctrl-g leave") && !text.contains("q quit"),
            "{text:?}"
        );
        // And it comes back once the prompt is left.
        let line = header_line(&panes, &theme, ZOOM_HINTS, true, 60);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("q quit"), "{text:?}");
    }

    #[test]
    fn snapshot_empty_state() {
        insta::assert_snapshot!(render_frame(&[], None, None, SortMode::Sidebar, 40, 6), @"



        no agent panes
        ");
    }

    #[test]
    fn selected_cell_border_uses_accent_color() {
        // Selection must be a different color, not a bolder status border:
        // bold on the grey idle border was indistinguishable.
        let theme = ColorTheme::default();
        let panes = vec![
            OverviewPane {
                info: make_pane(AgentType::Claude, PaneStatus::Running, "%1"),
                grid: None,
                err: None,
            },
            OverviewPane {
                info: make_pane(AgentType::Codex, PaneStatus::Idle, "%2"),
                grid: None,
                err: None,
            },
        ];
        // Selected: the first cell's left border is the accent color, bold.
        let buf = render_grid_buffer(&panes, Some(0), 80, 12);
        assert_eq!(buf[(0, 2)].fg, theme.accent);
        assert!(buf[(0, 2)].modifier.contains(Modifier::BOLD));
        // The unselected idle cell next to it keeps the grey border, so
        // the two can never look alike.
        assert_eq!(buf[(40, 2)].fg, theme.border_inactive);
        // Unselected running cells keep the status color, distinct from
        // the accent.
        let buf_unselected = render_grid_buffer(&panes, None, 80, 12);
        assert_eq!(buf_unselected[(0, 2)].fg, theme.status_running);
        assert_ne!(buf_unselected[(0, 2)].fg, theme.accent);
    }

    fn render_grid_buffer(
        panes: &[OverviewPane],
        selected_idx: Option<usize>,
        w: u16,
        h: u16,
    ) -> Buffer {
        let theme = ColorTheme::default();
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_grid(f, panes, selected_idx, None, SortMode::Sidebar, &theme))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn no_cell_is_selected_until_the_user_moves() {
        let theme = ColorTheme::default();
        let panes = vec![
            OverviewPane {
                info: make_pane(AgentType::Claude, PaneStatus::Running, "%1"),
                grid: None,
                err: None,
            },
            OverviewPane {
                info: make_pane(AgentType::Codex, PaneStatus::Idle, "%2"),
                grid: None,
                err: None,
            },
        ];
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_grid(f, &panes, None, None, SortMode::Sidebar, &theme))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        // Border columns of both cells (the grid area starts below the
        // header row): none may carry the accent while nothing is selected.
        for y in 1..12 {
            for x in [0u16, 39, 40, 79] {
                assert_ne!(
                    buf[(x, y)].fg,
                    theme.accent,
                    "accent border at {x},{y} — nothing is selected by default"
                );
            }
        }
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn prompt_refusal_keeps_text_out_of_a_shell() {
        let mut live = make_pane(AgentType::OpenCode, PaneStatus::Waiting, "%1");
        live.current_command = "opencode".into();
        let mut stale = make_pane(AgentType::Claude, PaneStatus::Idle, "%2");
        stale.current_command = "zsh".into();
        let panes = vec![
            OverviewPane {
                info: live,
                grid: None,
                err: None,
            },
            OverviewPane {
                info: stale,
                grid: None,
                err: None,
            },
        ];
        // A live agent foreground prompts.
        assert_eq!(prompt_refusal(&panes, "%1"), None);
        // An exited agent (shell back in the foreground) is refused, and
        // the message names the process so the user sees why.
        let refused = prompt_refusal(&panes, "%2").expect("shell foreground must refuse");
        assert!(
            refused.contains("zsh") && refused.contains("nothing sent"),
            "{refused}"
        );
        // A pane that vanished between ticks is refused too.
        assert!(prompt_refusal(&panes, "%9").is_some());
    }

    #[test]
    fn interactions_target_the_zoomed_cell_then_the_selection() {
        let panes = vec![
            OverviewPane {
                info: make_pane(AgentType::Claude, PaneStatus::Running, "%1"),
                grid: None,
                err: None,
            },
            OverviewPane {
                info: make_pane(AgentType::Codex, PaneStatus::Idle, "%2"),
                grid: None,
                err: None,
            },
        ];
        // Nothing selected: no target, so p/f/enter-go are all inert.
        assert!(target_pane(&panes, None, None).is_none());
        // From the grid the selection is the target; while zoomed, the
        // zoomed cell wins so typing lands on the pane being read.
        assert_eq!(
            target_pane(&panes, None, Some("%2")).unwrap().info.pane_id,
            "%2"
        );
        assert_eq!(
            target_pane(&panes, Some("%1"), Some("%2"))
                .unwrap()
                .info
                .pane_id,
            "%1"
        );
        // A vanished pane is not a target, even if its id is remembered.
        assert!(target_pane(&panes, Some("%9"), None).is_none());
    }

    fn make_drive(err: Option<&str>) -> DriveInput {
        DriveInput {
            pane_id: "%1".into(),
            label: "claude · project".into(),
            dims: Some((80, 24)),
            err: err.map(str::to_string),
        }
    }

    #[test]
    fn drive_keys_leave_or_forward() {
        // Ctrl-g leaves; every other chord — including Esc, the agents'
        // interrupt key, and plain g — forwards.
        assert!(matches!(
            handle_drive_key(&key(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            DriveAction::Leave
        ));
        assert!(matches!(
            handle_drive_key(&key(KeyCode::Char('g'), KeyModifiers::NONE)),
            DriveAction::Forward
        ));
        assert!(matches!(
            handle_drive_key(&key(KeyCode::Esc, KeyModifiers::NONE)),
            DriveAction::Forward
        ));
        assert!(matches!(
            handle_drive_key(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            DriveAction::Forward
        ));
    }

    #[test]
    fn forwarded_keys_map_to_tmux_names_or_literal_text() {
        // Plain and shifted characters are literal text, never key names —
        // key-name lookup would reinterpret some of them (a space alone is
        // fine, but the point of -l is that nothing is up for debate).
        assert!(matches!(
            forward_key_name(&key(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(ForwardedKey::Text(ref t)) if t == "a"
        ));
        assert!(matches!(
            forward_key_name(&key(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            Some(ForwardedKey::Text(ref t)) if t == "A"
        ));
        assert!(matches!(
            forward_key_name(&key(KeyCode::Char(' '), KeyModifiers::NONE)),
            Some(ForwardedKey::Text(ref t)) if t == " "
        ));
        // Chords become tmux key names; a space names "Space" (C- is not
        // a name), and M-Enter is the agents' newline-in-a-prompt chord.
        assert!(matches!(
            forward_key_name(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(ForwardedKey::Named(ref n)) if n == "C-c"
        ));
        assert!(matches!(
            forward_key_name(&key(KeyCode::Enter, KeyModifiers::ALT)),
            Some(ForwardedKey::Named(ref n)) if n == "M-Enter"
        ));
        assert!(matches!(
            forward_key_name(&key(KeyCode::Char('x'), KeyModifiers::CONTROL | KeyModifiers::ALT)),
            Some(ForwardedKey::Named(ref n)) if n == "C-M-x"
        ));
        assert!(matches!(
            forward_key_name(&key(KeyCode::Char(' '), KeyModifiers::CONTROL)),
            Some(ForwardedKey::Named(ref n)) if n == "C-Space"
        ));
        // A chorded character ignores Shift (its shift is the character);
        // only special keys keep S- in the name.
        assert!(matches!(
            forward_key_name(&key(KeyCode::Char('a'), KeyModifiers::CONTROL | KeyModifiers::SHIFT)),
            Some(ForwardedKey::Named(ref n)) if n == "C-a"
        ));
        // Named keys use tmux's vocabulary (verified against tmux 3.8).
        let named = |k: KeyEvent| match forward_key_name(&k) {
            Some(ForwardedKey::Named(n)) => n,
            other => panic!("expected a named key, got {other:?}"),
        };
        assert_eq!(named(key(KeyCode::Enter, KeyModifiers::NONE)), "Enter");
        assert_eq!(named(key(KeyCode::Esc, KeyModifiers::NONE)), "Escape");
        assert_eq!(named(key(KeyCode::Tab, KeyModifiers::NONE)), "Tab");
        assert_eq!(named(key(KeyCode::BackTab, KeyModifiers::NONE)), "BTab");
        assert_eq!(named(key(KeyCode::Backspace, KeyModifiers::NONE)), "BSpace");
        assert_eq!(named(key(KeyCode::Delete, KeyModifiers::NONE)), "DC");
        assert_eq!(named(key(KeyCode::Insert, KeyModifiers::NONE)), "IC");
        assert_eq!(named(key(KeyCode::Home, KeyModifiers::NONE)), "Home");
        assert_eq!(named(key(KeyCode::End, KeyModifiers::NONE)), "End");
        assert_eq!(named(key(KeyCode::PageUp, KeyModifiers::NONE)), "PPage");
        assert_eq!(named(key(KeyCode::PageDown, KeyModifiers::NONE)), "NPage");
        assert_eq!(named(key(KeyCode::Up, KeyModifiers::SHIFT)), "S-Up");
        assert_eq!(named(key(KeyCode::Left, KeyModifiers::ALT)), "M-Left");
        assert_eq!(named(key(KeyCode::F(5), KeyModifiers::NONE)), "F5");
        // Modifier-only presses and out-of-range keys drop, never type
        // characters: send-keys types an unrecognized name literally.
        assert!(forward_key_name(&key(KeyCode::Null, KeyModifiers::NONE)).is_none());
        assert!(forward_key_name(&key(KeyCode::F(13), KeyModifiers::NONE)).is_none());
        assert!(forward_key_name(&key(KeyCode::CapsLock, KeyModifiers::NONE)).is_none());
    }

    #[test]
    fn drive_is_refused_when_the_pane_is_gone() {
        let mut live = make_pane(AgentType::Claude, PaneStatus::Running, "%1");
        live.current_command = "claude".into();
        let panes = vec![OverviewPane {
            info: live,
            grid: None,
            err: None,
        }];
        // A pane in the listing passes (the fresh per-key foreground query
        // is the shell gate; under test tmux IPC is off and returns empty,
        // which is not a shell).
        assert_eq!(drive_refusal(&panes, "%1"), None);
        // A pane that vanished between ticks is refused: nothing to type
        // into, and the drive bar carries the reason.
        let refused = drive_refusal(&panes, "%9").expect("vanished pane must refuse");
        assert!(refused.contains("gone"), "{refused}");
    }

    #[test]
    fn snapshot_zoomed_with_drive_bar() {
        let mut info = make_pane(AgentType::Claude, PaneStatus::Running, "%1");
        info.prompt = "Editing src/main.rs".into();
        let pane = OverviewPane {
            info,
            grid: Some(grid_padded("\x1b[32m[claude] line one\x1b[0m")),
            err: None,
        };
        insta::assert_snapshot!(
            render_zoom(&[pane], 0, Some(&make_drive(None)), 40, 12),
            @"
        «   ●1ctrl-g leave                     ✕
        ┌ ● claude · project───────────────────┐
        │ Editing src/main.rs …                │
        │                                      │
        │                                      │
        │                                      │
        │                                      │
        │                                      │
        │                                      │
        │[claude] line one                     │
        └──────────────────────────────────────┘
         ▶ PROMPT claude · project · ctrl-g leav
        "
        );
    }

    #[test]
    fn snapshot_drive_bar_shows_a_refusal() {
        let pane = OverviewPane {
            info: make_pane(AgentType::Claude, PaneStatus::Waiting, "%1"),
            grid: None,
            err: None,
        };
        let drive = make_drive(Some("no agent in the foreground (zsh) — nothing sent"));
        insta::assert_snapshot!(
            render_zoom(&[pane], 0, Some(&drive), 100, 7),
            @"
        «   ◐1ctrl-g leave                                                                                 ✕
        ┌ ◐ claude · project───────────────────────────────────────────────────────────────────────────────┐
        │ Waiting · input                                                                                  │
        │(pane gone)                                                                                       │
        │                                                                                                  │
        └──────────────────────────────────────────────────────────────────────────────────────────────────┘
         ▶ PROMPT claude · project · ctrl-g leave  no agent in the foreground (zsh) — nothing sent
        "
        );
    }

    #[test]
    fn prompt_label_names_the_pane_like_the_cell_title() {
        let mut info = make_pane(AgentType::Claude, PaneStatus::Waiting, "%1");
        assert_eq!(prompt_label(&info), "claude · project");
        info.worktree.name = "fix-login".into();
        assert_eq!(prompt_label(&info), "claude · project ⎇ fix-login");
    }

    fn cells(infos: Vec<PaneInfo>) -> Vec<OverviewPane> {
        infos
            .into_iter()
            .map(|info| OverviewPane {
                info,
                grid: None,
                err: None,
            })
            .collect()
    }

    fn ids(panes: &[OverviewPane]) -> Vec<&str> {
        panes.iter().map(|p| p.info.pane_id.as_str()).collect()
    }

    fn window(id: &str, panes: Vec<PaneInfo>) -> crate::tmux::WindowInfo {
        crate::tmux::WindowInfo {
            window_id: id.into(),
            window_name: String::new(),
            window_active: false,
            auto_rename: false,
            panes,
        }
    }

    /// A pane in repo `repo`, session `session`, window `window`.
    fn located(
        agent: AgentType,
        status: PaneStatus,
        pane_id: &str,
        repo: &str,
        session: &str,
        window_index: i64,
    ) -> PaneInfo {
        let mut info = make_pane(agent, status, pane_id);
        info.path = format!("/src/{repo}");
        info.tmux_session = session.into();
        info.window_index = window_index;
        info
    }

    #[test]
    fn sidebar_order_matches_the_sidebars_group_then_position_sort() {
        // Repos beta/alpha sort by display name; inside a group it is
        // session name, then window index, then tmux line order. Statuses
        // are deliberately adversarial (waiting cells late, running first)
        // to prove they never enter the key.
        let infos = vec![
            located(
                AgentType::Claude,
                PaneStatus::Running,
                "%1",
                "beta",
                "work",
                3,
            ),
            located(
                AgentType::Claude,
                PaneStatus::Waiting,
                "%2",
                "beta",
                "work",
                1,
            ),
            located(AgentType::Codex, PaneStatus::Idle, "%3", "alpha", "work", 2),
            located(
                AgentType::Codex,
                PaneStatus::Waiting,
                "%4",
                "alpha",
                "zeta",
                1,
            ),
            located(
                AgentType::OpenCode,
                PaneStatus::Error,
                "%5",
                "alpha",
                "work",
                2,
            ),
        ];
        let mut panes = cells(infos);
        // Pre-seed the git cache: default info groups each pane under its
        // raw anchor; seeding keeps the test subprocess-free.
        let mut cache = HashMap::new();
        for repo in ["alpha", "beta"] {
            cache.insert(format!("/src/{repo}"), PaneGitInfo::default());
        }
        let sessions = vec![
            SessionInfo {
                session_name: "work".into(),
                windows: vec![
                    window(
                        "@1",
                        vec![
                            panes[1].info.clone(),
                            panes[2].info.clone(),
                            panes[4].info.clone(),
                        ],
                    ),
                    window("@3", vec![panes[0].info.clone()]),
                ],
            },
            SessionInfo {
                session_name: "zeta".into(),
                windows: vec![window("@4", vec![panes[3].info.clone()])],
            },
        ];
        sort_like_sidebar(&mut panes, &sessions, GroupBy::Repo, &mut cache);
        // alpha group first: %3, %5 (same window, tmux line order), then
        // %4 (same group, session zeta after work); then beta: %2, %1.
        assert_eq!(ids(&panes), vec!["%3", "%5", "%4", "%2", "%1"]);

        // Status-first still floats the loud cells — it is one `s` away.
        // Stable within a class, so the two waiting cells keep their
        // sidebar-relative order (%4 ahead of %2 as the sort sees them).
        sort_for_grid(&mut panes);
        assert_eq!(ids(&panes)[0], "%4", "waiting leads");
        assert_eq!(SortMode::Sidebar.toggled(), SortMode::Status);
        assert_eq!(SortMode::Status.toggled(), SortMode::Sidebar);
    }

    #[test]
    fn unknown_panes_keep_encounter_order_at_the_end() {
        let infos = vec![
            located(
                AgentType::Claude,
                PaneStatus::Running,
                "%1",
                "beta",
                "work",
                1,
            ),
            located(
                AgentType::Claude,
                PaneStatus::Idle,
                "%9",
                "ghost",
                "work",
                1,
            ),
        ];
        let mut panes = cells(infos);
        let mut cache = HashMap::new();
        cache.insert("/src/beta".into(), PaneGitInfo::default());
        let sessions = vec![SessionInfo {
            session_name: "work".into(),
            windows: vec![window("@1", vec![panes[0].info.clone()])],
        }];
        sort_like_sidebar(&mut panes, &sessions, GroupBy::Repo, &mut cache);
        assert_eq!(
            ids(&panes),
            vec!["%1", "%9"],
            "unranked cell trails, never dropped"
        );
    }

    #[test]
    fn grid_body_click_hits_the_cell_under_it() {
        // 4 panes across an 80x20 body starting at row 1: 2 cols x 2 rows
        // of 40x10 cells (cell_rects is the single layout source of truth
        // shared by drawing and hit-testing).
        let body = Rect::new(0, 1, 80, 20);
        assert_eq!(cell_at(4, body, 1, 0), Some(0));
        assert_eq!(cell_at(4, body, 1, 39), Some(0));
        assert_eq!(cell_at(4, body, 1, 40), Some(1));
        assert_eq!(cell_at(4, body, 15, 79), Some(3));
        assert_eq!(cell_at(4, body, 11, 0), Some(2));
        // The header row and below-the-body rows hit no cell.
        assert_eq!(cell_at(4, body, 0, 10), None);
        assert_eq!(cell_at(4, body, 21, 10), None);
        // Empty grid and degenerate sizes are inert.
        assert_eq!(cell_at(0, body, 1, 1), None);
        assert_eq!(cell_at(2, Rect::new(0, 1, 80, 0), 5, 5), None);
    }

    #[test]
    fn window_zoom_probe_decides_eligibility() {
        // Two panes, unzoomed: eligible.
        assert!(may_take_window_zoom("2|0"));
        assert!(may_take_window_zoom("3|0"));
        // A pane alone in its window has nothing to gain.
        assert!(!may_take_window_zoom("1|0"));
        // A window already zoomed on arrival is the user's state.
        assert!(!may_take_window_zoom("2|1"));
        // A failed probe (pane gone) and garbage both skip.
        assert!(!may_take_window_zoom(""));
        assert!(!may_take_window_zoom("garbage"));
        assert!(!may_take_window_zoom("2|"));
    }

    #[test]
    fn zoom_flag_probe_gates_the_restore() {
        assert!(zoom_flag_set("1"));
        assert!(!zoom_flag_set("0"));
        // An empty probe (pane gone while zoomed) reads as not zoomed:
        // the restore degrades to a silent no-op instead of re-zooming.
        assert!(!zoom_flag_set(""));
    }

    #[test]
    fn paint_grid_centers_a_narrower_source_when_asked() {
        let grid = ansi::parse_ansi(b"abcd", 4, 1);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        paint_grid(&mut buf, Rect::new(0, 0, 10, 1), &grid, true);
        let line: String = (0..10).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(line, "   abcd   ");
        // Grid cells keep the left edge: the pane sits left in its window
        // and the cell mirrors that geometry.
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        paint_grid(&mut buf, Rect::new(0, 0, 10, 1), &grid, false);
        let line: String = (0..10).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(line, "abcd      ");
    }

    #[test]
    fn snapshot_zoomed_centers_a_source_narrower_than_the_area() {
        let mut info = make_pane(AgentType::Codex, PaneStatus::Running, "%1");
        info.prompt = "Refactoring the export path".into();
        let pane = OverviewPane {
            info,
            grid: Some(grid_padded("\x1b[34m[codex] waiting for input\x1b[0m")),
            err: None,
        };
        // A 40-column source in a 60-column popup: the residual margin
        // splits evenly instead of hugging the left edge — the fallback
        // for sources that cannot reach the popup's width even zoomed
        // (multi-client setups).
        insta::assert_snapshot!(render_zoom(&[pane], 0, None, 60, 12), @"
        «   ●1esc grid · enter go · p prompt · q quit              ✕
        ┌ ● codex · project────────────────────────────────────────┐
        │ Refactoring the export path …                            │
        │                                                          │
        │                                                          │
        │                                                          │
        │                                                          │
        │                                                          │
        │                                                          │
        │                                                          │
        │         [codex] waiting for input                        │
        └──────────────────────────────────────────────────────────┘
        ");
    }

    /// The zoom lifecycle against a throwaway server: a 60-column window
    /// split 20/40 (the standard sidebar shape whose narrow pane is what
    /// focus mode has to widen), exercising take/restore, the ownership
    /// rules, and the dead-pane no-op.
    #[test]
    #[ignore = "requires local tmux"]
    fn window_zoom_lifecycle_on_a_scoped_server() {
        use std::process::Command;

        // Isolation per the capture integration tests: a throwaway server,
        // TMUX unset so every tmux invocation (ours and the binary's) lands
        // there. Runs under `cargo test -- --ignored --test-threads=1`.
        // `-f /dev/null` keeps the server config-clean: the user's plugin
        // hooks would otherwise fire on the windows this test creates (the
        // after-new-window hook would add a sidebar pane and change
        // `window_panes` under the test's feet).
        let tmux_root = tempfile::tempdir().unwrap();
        // SAFETY: single-threaded ignored runs only (see the capture
        // tests); no other test in this binary spawns tmux (the IPC gate
        // is closed for them).
        unsafe {
            std::env::set_var("TMUX_TMPDIR", tmux_root.path());
            std::env::remove_var("TMUX");
            std::env::remove_var("TMUX_PANE");
        }
        crate::tmux::enable_tmux_ipc();

        let run = |args: &[&str]| {
            let out = Command::new("tmux").args(args).output().unwrap();
            assert!(
                out.status.success(),
                "tmux {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        run(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "zoom-life",
            "-x",
            "60",
            "-y",
            "20",
        ]);
        let _cleanup = scopeguard::guard((), |_| {
            let _ = Command::new("tmux").args(["kill-server"]).status();
        });
        // 66% to the new pane: an asymmetric 20/40 split, so restoring the
        // exact layout is observable.
        let agent = run(&[
            "split-window",
            "-h",
            "-p",
            "66",
            "-d",
            "-t",
            "zoom-life:0.0",
            "-P",
            "-F",
            "#{pane_id}",
        ]);
        let sibling = run(&["display-message", "-p", "-t", "zoom-life:0.0", "#{pane_id}"]);
        std::thread::sleep(Duration::from_millis(100));

        let widths = || -> HashMap<String, u16> {
            run(&[
                "list-panes",
                "-t",
                "zoom-life:0",
                "-F",
                "#{pane_id},#{pane_width}",
            ])
            .lines()
            .filter_map(|l| {
                let (id, w) = l.split_once(',')?;
                Some((id.to_string(), w.parse().ok()?))
            })
            .collect()
        };
        let flag =
            |pane: &str| run(&["display-message", "-p", "-t", pane, "#{window_zoomed_flag}"]);
        let probe = |pane: &str| {
            run(&[
                "display-message",
                "-p",
                "-t",
                pane,
                "#{window_panes}|#{window_zoomed_flag}",
            ])
        };

        let before = widths();
        assert_eq!(before.len(), 2, "two siblings in the window");
        assert_eq!(probe(&agent), "2|0");
        let dims_before = pane_dimensions_for(&agent).expect("live pane has dims");

        // Take: the pane (and its pty) grow to the full window width; the
        // hidden sibling keeps its size under the zoom.
        let guard = WindowZoom::take(&agent, &probe(&agent)).expect("eligible window takes");
        assert_eq!(flag(&agent), "1");
        assert_eq!(widths().get(&agent).copied(), Some(60));
        assert_eq!(
            widths().get(&sibling).copied(),
            before.get(&sibling).copied()
        );
        // The focus path's targeted dims probe sees the new geometry.
        assert_eq!(pane_dimensions_for(&agent), Some((60, 20)));

        // Restore (the Drop every exit path rides): the exact pre-zoom
        // layout comes back.
        drop(guard);
        assert_eq!(flag(&agent), "0");
        assert_eq!(widths(), before);
        assert_eq!(pane_dimensions_for(&agent), Some(dims_before));

        // The focus path's entry helper: zoom and immediate re-capture in
        // one call — the first frame is already wide, and the dims land in
        // the map drive entry reads.
        let mut pane = OverviewPane {
            info: make_pane(AgentType::Claude, PaneStatus::Running, &agent),
            grid: None,
            err: None,
        };
        pane.grid = capture_grid(&pane.info, pane_dimensions_for(&agent)).ok();
        let pre_w = pane.grid.as_ref().map(|g| g[0].len()).unwrap_or(0);
        assert!(pre_w < 60, "pre-zoom capture mirrors the narrow pane");
        let mut last_dims: HashMap<String, (u16, u16)> = HashMap::new();
        let guard = enter_focus_zoom(&mut pane, &mut last_dims).expect("eligible window takes");
        assert_eq!(flag(&agent), "1");
        assert_eq!(
            pane.grid.as_ref().map(|g| g[0].len()),
            Some(60),
            "the entry re-capture widens the cell in the same tick"
        );
        assert_eq!(last_dims.get(&agent).copied(), Some((60, 20)));
        drop(guard);
        assert_eq!(flag(&agent), "0");
        assert_eq!(widths(), before);

        // The flag gate: a zoom someone else already lifted must not be
        // re-toggled by a late restore.
        let guard = WindowZoom::take(&agent, &probe(&agent)).expect("re-take after restore");
        run(&["resize-pane", "-Z", "-t", &agent]); // someone else lifts it
        assert_eq!(flag(&agent), "0");
        guard.restore();
        assert_eq!(flag(&agent), "0", "restore must not re-zoom");

        // Ownership rule: a window already zoomed on arrival is the
        // user's state — never touched, left exactly as found.
        run(&["resize-pane", "-Z", "-t", &sibling]);
        assert_eq!(probe(&agent), "2|1");
        assert!(WindowZoom::take(&agent, &probe(&agent)).is_none());
        assert_eq!(flag(&sibling), "1", "the user's zoom survives");
        assert_eq!(widths().get(&sibling).copied(), Some(60));
        run(&["resize-pane", "-Z", "-t", &sibling]); // undo the user zoom
        assert_eq!(flag(&sibling), "0");

        // A pane alone in its window: nothing to gain, no churn.
        run(&["new-window", "-t", "zoom-life"]);
        let lone = run(&["display-message", "-p", "-t", "zoom-life", "#{pane_id}"]);
        assert_eq!(probe(&lone), "1|0");
        assert!(WindowZoom::take(&lone, &probe(&lone)).is_none());
        assert_eq!(flag(&lone), "0");

        // The dead-pane restore no-op: the zoomed pane dies mid-zoom, the
        // probe fails, and the restore degrades silently (no re-zoom of a
        // dead target, no panic, the surviving sibling untouched).
        let guard = WindowZoom::take(&agent, &probe(&agent)).expect("take before the kill");
        assert_eq!(flag(&agent), "1");
        run(&["kill-pane", "-t", &agent]);
        assert_eq!(
            flag(&sibling),
            "0",
            "tmux unzooms when the zoomed pane dies"
        );
        guard.restore();
        assert_eq!(flag(&sibling), "0");
        assert_eq!(
            run(&["list-panes", "-t", "zoom-life:0", "-F", "#{pane_id}"]),
            sibling,
            "the sibling survives the dead-pane restore"
        );
    }
}
