//! Experimental `overview` subcommand: a fullscreen grid of live agent panes.
//!
//! Design notes live in docs/overview-grid-spike.md. The short version: the
//! grid never attaches to the agent panes (attaching couples terminal sizes
//! — a tmux window has exactly one size shared by every session linking it,
//! so a smaller viewer resizes the real agent). Instead each cell mirrors a
//! pane by polling `capture-pane -p -e` and painting the captured styled
//! grid into a ratatui buffer, reusing the vte parser from the `capture`
//! subcommand. The mirror itself is read-only; the only writes to a real
//! pane are the explicit interaction actions below.
//!
//! Presentation rules that make the grid an instrument instead of a
//! wallpaper:
//!
//! - Bottom-anchored capture: agent TUIs draw their current state at the
//!   bottom of the screen, so a cell shorter than its source pane keeps the
//!   LAST rows (the live part) and drops the top.
//! - Status-first ordering: waiting/error cells sort to the front, get loud
//!   borders and a wait-reason strip; idle cells fade to grey. A header row
//!   summarizes the counts.
//! - Enter zooms the selected cell fullscreen (still a capture — read-only);
//!   Esc collapses back to the grid.
//! - Interaction ladder (docs/overview-grid-spike.md): `p` opens an inline
//!   prompt bar that types into the target pane with `send-keys` (no attach,
//!   no resize side effects); `f` — or Enter again while zoomed — closes the
//!   popup and focuses the real pane via `select_pane`. The target is the
//!   zoomed cell when zoomed, else the selected cell.

use std::collections::{HashMap, HashSet};
use std::io;
use std::time::{Duration, Instant};

use crossterm::{
    event::{
        self, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton,
        MouseEventKind,
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
use crate::tmux::{self, AgentType, PaneInfo, PaneStatus};
use crate::ui::colors::ColorTheme;

const DEFAULT_INTERVAL_MS: u64 = 500;
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

/// The inline prompt bar: text typed here goes to the named pane with
/// `send-keys` when Enter is pressed (see [`PromptAction::Send`]). While
/// open it owns the keyboard — every key is editing, never a grid command.
struct PromptInput {
    pane_id: String,
    /// `agent · repo` label before the caret, so the user can see at a
    /// glance which pane the text will land in.
    label: String,
    text: String,
    /// Last `send-keys` failure, shown on the bar; the next edit clears it.
    err: Option<String>,
}

/// What a key press inside the prompt bar asks the caller to do.
enum PromptAction {
    /// The bar consumed the key; keep editing.
    Stay,
    /// Dismiss the bar without sending (Esc, or Enter on an empty bar).
    Close,
    /// Enter with text: the caller sends it to the bar's pane.
    Send(String),
}

/// Edit the prompt bar from one key press. Pure — the tmux round trip for
/// [`PromptAction::Send`] stays with the caller, so this needs no server.
fn handle_prompt_key(
    input: &mut PromptInput,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> PromptAction {
    // Any keystroke retires a stale send failure; if this send fails too,
    // the caller restores the text and the error.
    input.err = None;
    match code {
        KeyCode::Esc => PromptAction::Close,
        KeyCode::Backspace => {
            input.text.pop();
            PromptAction::Stay
        }
        KeyCode::Enter => {
            let text = std::mem::take(&mut input.text);
            if text.trim().is_empty() {
                PromptAction::Close
            } else {
                PromptAction::Send(text)
            }
        }
        // Control/Alt chords would smuggle control bytes into the agent's
        // input line; plain (shifted) characters are the only accepted text.
        KeyCode::Char(c) if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            input.text.push(c);
            PromptAction::Stay
        }
        _ => PromptAction::Stay,
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

pub fn cmd_overview(args: &[String]) -> i32 {
    let mut popup = false;
    let mut popup_client: Option<String> = None;
    let mut interval = Duration::from_millis(DEFAULT_INTERVAL_MS);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--popup" => popup = true,
            "--popup-client" => {
                popup_client = Some(iter.next().map(|s| s.to_string()).unwrap_or_default());
                if popup_client.as_deref() == Some("") {
                    eprintln!("overview: --popup-client requires a client name");
                    return 2;
                }
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
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;

        let mut panes: Vec<OverviewPane> = Vec::new();
        // Selection and zoom track pane ids, not indexes: the grid is
        // status-sorted, so a refresh can reorder cells and an index would
        // silently move the selection onto a different agent.
        let mut selected: Option<String> = None;
        let mut zoomed: Option<String> = None;
        // The inline prompt bar, open while the user composes a message for
        // one pane (see handle_prompt_key).
        let mut prompt: Option<PromptInput> = None;
        // Refresh on the first iteration: the elapsed time starts "in the past".
        let mut last_refresh = Instant::now() - interval;

        loop {
            if last_refresh.elapsed() >= interval {
                // A failed listing keeps the previous frame instead of
                // blanking the grid on one transient tmux hiccup.
                if let Some(next) = refresh_panes() {
                    panes = next;
                }
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
                }
                // A prompt bar whose target pane vanished closes itself.
                if prompt
                    .as_ref()
                    .is_some_and(|input| !panes.iter().any(|p| p.info.pane_id == input.pane_id))
                {
                    prompt = None;
                }
                last_refresh = Instant::now();
            }

            let selected_idx = selected
                .as_ref()
                .and_then(|id| panes.iter().position(|p| &p.info.pane_id == id));
            let zoom_idx = zoomed
                .as_ref()
                .and_then(|id| panes.iter().position(|p| &p.info.pane_id == id));
            terminal.draw(|f| match zoom_idx {
                Some(i) => draw_zoomed(f, &panes, i, prompt.as_ref(), &theme),
                None => draw_grid(f, &panes, selected_idx, prompt.as_ref(), &theme),
            })?;

            let budget = interval
                .checked_sub(last_refresh.elapsed())
                .unwrap_or(Duration::from_millis(30))
                .max(Duration::from_millis(30));
            if event::poll(budget)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        // While the prompt bar is open it owns the keyboard.
                        let mut dismiss = false;
                        if let Some(input) = prompt.as_mut() {
                            match handle_prompt_key(&mut *input, key.code, key.modifiers) {
                                PromptAction::Stay => {}
                                PromptAction::Close => dismiss = true,
                                PromptAction::Send(text) => {
                                    let pane_id = input.pane_id.clone();
                                    match tmux::send_prompt(&pane_id, &text) {
                                        Ok(()) => dismiss = true,
                                        Err(e) => {
                                            // Keep the bar open with the text
                                            // intact so the user can retry or
                                            // bail; the failure shows on the bar.
                                            input.text = text;
                                            input.err = Some(e);
                                        }
                                    }
                                }
                            }
                        } else {
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
                                            tmux::select_pane(&pane.info.pane_id);
                                            return Ok(0);
                                        }
                                    } else if let Some(idx) = selected_idx {
                                        zoomed = Some(panes[idx].info.pane_id.clone());
                                    }
                                }
                                KeyCode::Char('p') => {
                                    if let Some(pane) =
                                        target_pane(&panes, zoomed.as_deref(), selected.as_deref())
                                    {
                                        prompt = Some(PromptInput {
                                            pane_id: pane.info.pane_id.clone(),
                                            label: prompt_label(&pane.info),
                                            text: String::new(),
                                            err: None,
                                        });
                                    }
                                }
                                KeyCode::Char('f') => {
                                    // Focus: close the popup and hand the
                                    // client to the agent's real pane.
                                    if let Some(pane) =
                                        target_pane(&panes, zoomed.as_deref(), selected.as_deref())
                                    {
                                        tmux::select_pane(&pane.info.pane_id);
                                        return Ok(0);
                                    }
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
                        if dismiss {
                            prompt = None;
                        }
                    }
                    Event::Mouse(mouse)
                        if mouse.kind == MouseEventKind::Down(MouseButton::Left) =>
                    {
                        let size = terminal.size()?;
                        if header_close_hit(mouse.row, mouse.column, size.width) {
                            return Ok(0);
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
        crossterm::event::DisableMouseCapture
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

/// One refresh cycle: the standard single `list-panes -a` sync for the agent
/// set, one geometry query, then one capture per agent pane. `None` when the
/// listing itself failed (caller keeps the previous frame).
fn refresh_panes() -> Option<Vec<OverviewPane>> {
    let listing = tmux::fetch_pane_listing()?;
    let snapshot = tmux::build_session_snapshot(&listing, "%overview-none");

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
    sort_for_grid(&mut panes);
    Some(panes)
}

/// Status-first cell order (see status_rank); stable within a class.
fn sort_for_grid(panes: &mut [OverviewPane]) {
    panes.sort_by_key(|p| status_rank(&p.info.status));
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

fn draw_grid(
    f: &mut Frame,
    panes: &[OverviewPane],
    selected_idx: Option<usize>,
    prompt: Option<&PromptInput>,
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
    if prompt.is_some() {
        constraints.push(Constraint::Length(1));
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let width = area.width;
    let counts = header_line(panes, theme, false, width);
    f.render_widget(Paragraph::new(counts), chunks[0]);

    let (cols, rows) = grid_dims(panes.len(), chunks[1]);
    let row_rects = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Fill(1); rows as usize])
        .split(chunks[1]);
    let mut idx = 0;
    for row in row_rects.iter() {
        let col_rects = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Fill(1); cols as usize])
            .split(*row);
        for cell_area in col_rects.iter() {
            if idx >= panes.len() {
                return;
            }
            draw_cell(f, *cell_area, &panes[idx], selected_idx == Some(idx), theme);
            idx += 1;
        }
    }
    if let Some(input) = prompt {
        f.render_widget(Paragraph::new(prompt_line(input, theme)), chunks[2]);
    }
}

/// Zoomed view: the selected pane's capture rendered into one fullscreen
/// cell. Same renderer, same rules — just no cropping pressure.
fn draw_zoomed(
    f: &mut Frame,
    panes: &[OverviewPane],
    idx: usize,
    prompt: Option<&PromptInput>,
    theme: &ColorTheme,
) {
    let area = f.area();
    if area.width < 4 || area.height < 3 {
        return;
    }
    let mut constraints = vec![Constraint::Length(1), Constraint::Fill(1)];
    if prompt.is_some() {
        constraints.push(Constraint::Length(1));
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let width = area.width;
    let counts = header_line(panes, theme, true, width);
    f.render_widget(Paragraph::new(counts), chunks[0]);
    draw_cell(f, chunks[1], &panes[idx], true, theme);
    if let Some(input) = prompt {
        f.render_widget(Paragraph::new(prompt_line(input, theme)), chunks[2]);
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

fn draw_cell(f: &mut Frame, area: Rect, pane: &OverviewPane, selected: bool, theme: &ColorTheme) {
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
        Some(grid) => paint_grid(f.buffer_mut(), content, grid),
        None => {
            let msg = pane.err.as_deref().unwrap_or("(pane gone)");
            let failed = Paragraph::new(msg).style(Style::default().fg(theme.border_inactive));
            f.render_widget(failed, content);
        }
    }
}

/// Header row: per-status counts (only non-zero classes) so "does anything
/// need me?" is one glance, plus the key hints and a clickable `✕` pinned
/// to the last two columns for closing with the mouse.
fn header_line(
    panes: &[OverviewPane],
    theme: &ColorTheme,
    zoomed: bool,
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
    // `q quit` is the one everyone needs, so it survives even when the rest
    // is squeezed out, and the `✕` keeps its pinned columns at any width.
    let optional: &[&str] = if zoomed {
        &["esc grid", "enter go", "p prompt"]
    } else {
        &["tab move", "enter zoom", "p prompt", "f go"]
    };
    const REQUIRED: &str = "q quit";
    use unicode_width::UnicodeWidthStr as _;
    let used_prefix: usize = spans.iter().map(|s| s.width()).sum();
    let hint_budget = (width as usize)
        .saturating_sub(used_prefix)
        .saturating_sub(" ✕".width());
    let mut shown: Vec<&str> = Vec::new();
    let mut hints_width = REQUIRED.width();
    for hint in optional {
        // Each extra segment costs its own width plus the " · " separator.
        let cost = hint.width() + 3;
        if hints_width + cost > hint_budget {
            break;
        }
        shown.push(hint);
        hints_width += cost;
    }
    shown.push(REQUIRED);
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

/// The prompt bar line: caret, target label, typed text, block cursor. A
/// failed send appends its message in the error color — the text stays so
/// the user can retry or dismiss with Esc.
fn prompt_line(input: &PromptInput, theme: &ColorTheme) -> Line<'static> {
    let label = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![
        Span::styled(format!(" ❯ {} ", input.label), label),
        Span::raw(input.text.clone()),
        Span::styled("█", Style::default().fg(theme.accent)),
    ];
    if let Some(err) = &input.err {
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
/// side.
fn paint_grid(buf: &mut Buffer, area: Rect, grid: &[Vec<StyledCell>]) {
    let skip = grid.len().saturating_sub(area.height as usize);
    for (y, row) in grid.iter().skip(skip).enumerate() {
        if y as u16 >= area.height {
            break;
        }
        for (x, cell) in row.iter().enumerate() {
            if x as u16 >= area.width {
                break;
            }
            let pos = Position::new(area.x + x as u16, area.y + y as u16);
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
    if cell.reversed {
        mods |= Modifier::REVERSED;
    }
    if cell.underlined {
        mods |= Modifier::UNDERLINED;
    }
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
        prompt: Option<&PromptInput>,
        w: u16,
        h: u16,
    ) -> String {
        let theme = ColorTheme::default();
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_grid(f, panes, selected_idx, prompt, &theme))
            .unwrap();
        buffer_to_string(terminal.backend().buffer())
    }

    fn render_zoom(
        panes: &[OverviewPane],
        idx: usize,
        prompt: Option<&PromptInput>,
        w: u16,
        h: u16,
    ) -> String {
        let theme = ColorTheme::default();
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_zoomed(f, panes, idx, prompt, &theme))
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
        insta::assert_snapshot!(render_frame(&[pane], None, None, 40, 10), @"
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
        insta::assert_snapshot!(render_frame(&panes, Some(0), None, 80, 12), @"
        «   ◐1 ●1tab move · enter zoom · p prompt · f go · q quit                      ✕
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
        insta::assert_snapshot!(render_frame(&[pane], None, None, 40, 6), @"
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
        let line = header_line(&[pane], &ColorTheme::default(), false, 40);
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
    fn snapshot_empty_state() {
        insta::assert_snapshot!(render_frame(&[], None, None, 40, 6), @"



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
            .draw(|f| draw_grid(f, panes, selected_idx, None, &theme))
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
            .draw(|f| draw_grid(f, &panes, None, None, &theme))
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

    fn make_prompt(text: &str, err: Option<&str>) -> PromptInput {
        PromptInput {
            pane_id: "%1".into(),
            label: "claude · project".into(),
            text: text.into(),
            err: err.map(str::to_string),
        }
    }

    #[test]
    fn prompt_bar_edits_and_sends() {
        let mut input = make_prompt("", None);
        // Plain characters accumulate, shifted ones included; control/alt
        // chords are ignored so no control byte reaches the agent's line.
        assert!(matches!(
            handle_prompt_key(&mut input, KeyCode::Char('h'), KeyModifiers::NONE),
            PromptAction::Stay
        ));
        handle_prompt_key(&mut input, KeyCode::Char('i'), KeyModifiers::NONE);
        handle_prompt_key(&mut input, KeyCode::Char('C'), KeyModifiers::SHIFT);
        handle_prompt_key(&mut input, KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(input.text, "hiC");
        handle_prompt_key(&mut input, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(input.text, "hi");
        // Enter with text hands it to the caller and empties the bar.
        assert!(matches!(
            handle_prompt_key(&mut input, KeyCode::Enter, KeyModifiers::NONE),
            PromptAction::Send(ref t) if t == "hi"
        ));
        assert!(input.text.is_empty());
        // Enter on an empty bar dismisses without sending; Esc always does.
        assert!(matches!(
            handle_prompt_key(&mut input, KeyCode::Enter, KeyModifiers::NONE),
            PromptAction::Close
        ));
        input.text = "draft".into();
        assert!(matches!(
            handle_prompt_key(&mut input, KeyCode::Esc, KeyModifiers::NONE),
            PromptAction::Close
        ));
        // Whitespace-only is no prompt either.
        input.text = "  ".into();
        assert!(matches!(
            handle_prompt_key(&mut input, KeyCode::Enter, KeyModifiers::NONE),
            PromptAction::Close
        ));
    }

    #[test]
    fn prompt_bar_editing_clears_a_send_failure() {
        let mut input = make_prompt("retry", Some("send-keys failed"));
        handle_prompt_key(&mut input, KeyCode::Char('!'), KeyModifiers::NONE);
        assert_eq!(input.err, None);
        assert_eq!(input.text, "retry!");
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

    #[test]
    fn prompt_label_names_the_pane_like_the_cell_title() {
        let mut info = make_pane(AgentType::Claude, PaneStatus::Waiting, "%1");
        assert_eq!(prompt_label(&info), "claude · project");
        info.worktree.name = "fix-login".into();
        assert_eq!(prompt_label(&info), "claude · project ⎇ fix-login");
    }

    #[test]
    fn snapshot_grid_with_prompt_bar() {
        let mut info = make_pane(AgentType::Claude, PaneStatus::Running, "%1");
        info.prompt = "Editing src/main.rs".into();
        let pane = OverviewPane {
            info,
            grid: Some(grid_padded("\x1b[32m[claude] line one\x1b[0m")),
            err: None,
        };
        let prompt = make_prompt("run the tests", None);
        insta::assert_snapshot!(render_frame(&[pane], Some(0), Some(&prompt), 40, 9), @"
        «   ●1tab move · enter zoom · q quit   ✕
        ┌ ● claude · project───────────────────┐
        │ Editing src/main.rs …                │
        │                                      │
        │                                      │
        │                                      │
        │[claude] line one                     │
        └──────────────────────────────────────┘
         ❯ claude · project run the tests█
        ");
    }

    #[test]
    fn snapshot_prompt_bar_shows_a_send_failure() {
        let pane = OverviewPane {
            info: make_pane(AgentType::Claude, PaneStatus::Waiting, "%1"),
            grid: None,
            err: None,
        };
        let prompt = make_prompt("yes, go ahead", Some("send-keys failed"));
        insta::assert_snapshot!(render_frame(&[pane], Some(0), Some(&prompt), 56, 7), @"
        «   ◐1tab move · enter zoom · p prompt · f go · q quit ✕
        ┌ ◐ claude · project───────────────────────────────────┐
        │ Waiting · input                                      │
        │(pane gone)                                           │
        │                                                      │
        └──────────────────────────────────────────────────────┘
         ❯ claude · project yes, go ahead█  send-keys failed
        ");
    }

    #[test]
    fn snapshot_zoomed_with_prompt_bar() {
        let mut info = make_pane(AgentType::Codex, PaneStatus::Running, "%1");
        info.prompt = "Refactoring the export path".into();
        let pane = OverviewPane {
            info,
            grid: Some(grid_padded("\x1b[34m[codex] waiting for input\x1b[0m")),
            err: None,
        };
        let prompt = make_prompt("add a test", None);
        insta::assert_snapshot!(render_zoom(&[pane], 0, Some(&prompt), 40, 12), @"
        «   ●1esc grid · enter go · q quit     ✕
        ┌ ● codex · project────────────────────┐
        │ Refactoring the export path …        │
        │                                      │
        │                                      │
        │                                      │
        │                                      │
        │                                      │
        │                                      │
        │[codex] waiting for input             │
        └──────────────────────────────────────┘
         ❯ claude · project add a test█
        ");
    }
}
