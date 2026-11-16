//! Experimental `overview` subcommand: a fullscreen grid of live agent panes.
//!
//! Design notes live in docs/overview-grid-spike.md. The short version: the
//! grid never attaches to the agent panes (attaching couples terminal sizes
//! — a tmux window has exactly one size shared by every session linking it,
//! so a smaller viewer resizes the real agent). Instead each cell mirrors a
//! pane by polling `capture-pane -p -e` and painting the captured styled
//! grid into a ratatui buffer, reusing the vte parser from the `capture`
//! subcommand. Read-only by construction; the originals are untouched.
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
//!
//! Spike scope: popup container (`--popup`), single-threaded refresh in the
//! event loop. Not yet registered in agent-sidebar.conf.

use std::collections::{HashMap, HashSet};
use std::io;
use std::process::Command;
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
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
    let mut cmd = Command::new("tmux");
    cmd.args(["display-popup", "-E", "-w", "100%", "-h", "100%", "-d"]);
    cmd.arg(cwd);
    if let Some(client) = client {
        cmd.arg("-t").arg(client);
    }
    cmd.arg(exe).arg("overview");
    let status = cmd
        .status()
        .map_err(|e| format!("spawn tmux display-popup: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("tmux display-popup exited with {status}"))
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
    let mut child = Command::new("tmux")
        .args(["display-popup", "-E", "-w", "100%", "-h", "100%", "-d"])
        .arg(cwd)
        .args(["-e", &format!("TMUX_PANE={pane}")])
        .arg("-t")
        .arg(window)
        .arg(exe)
        .arg("overview")
        .spawn()
        .map_err(|e| format!("spawn tmux display-popup: {e}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn run_tui(interval: Duration) -> io::Result<i32> {
    // One tmux spawn for the user's palette; the overview renders with the
    // same status/agent colors as the sidebar.
    let theme = ColorTheme::from_tmux();
    enable_raw_mode()?;
    let result = (|| -> io::Result<i32> {
        execute!(io::stdout(), EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;

        let mut panes: Vec<OverviewPane> = Vec::new();
        // Selection and zoom track pane ids, not indexes: the grid is
        // status-sorted, so a refresh can reorder cells and an index would
        // silently move the selection onto a different agent.
        let mut selected: Option<String> = None;
        let mut zoomed: Option<String> = None;
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
                last_refresh = Instant::now();
            }

            let selected_idx = selected
                .as_ref()
                .and_then(|id| panes.iter().position(|p| &p.info.pane_id == id));
            let zoom_idx = zoomed
                .as_ref()
                .and_then(|id| panes.iter().position(|p| &p.info.pane_id == id));
            terminal.draw(|f| match zoom_idx {
                Some(i) => draw_zoomed(f, &panes, i, &theme),
                None => draw_grid(f, &panes, selected_idx, &theme),
            })?;

            let budget = interval
                .checked_sub(last_refresh.elapsed())
                .unwrap_or(Duration::from_millis(30))
                .max(Duration::from_millis(30));
            if event::poll(budget)?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                match key.code {
                    KeyCode::Char('q') => return Ok(0),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(0);
                    }
                    // Zoom collapses first; Esc only quits from the grid.
                    KeyCode::Esc => {
                        if zoomed.take().is_none() {
                            return Ok(0);
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(idx) = selected_idx {
                            zoomed = Some(panes[idx].info.pane_id.clone());
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
        }
    })();

    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    result
}

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

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Fill(1)])
        .split(area);
    let counts = header_line(panes, theme, false);
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
}

/// Zoomed view: the selected pane's capture rendered into one fullscreen
/// cell. Same renderer, same rules — just no cropping pressure.
fn draw_zoomed(f: &mut Frame, panes: &[OverviewPane], idx: usize, theme: &ColorTheme) {
    let area = f.area();
    if area.width < 4 || area.height < 3 {
        return;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Fill(1)])
        .split(area);
    let counts = header_line(panes, theme, true);
    f.render_widget(Paragraph::new(counts), chunks[0]);
    draw_cell(f, chunks[1], &panes[idx], true, theme);
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
/// need me?" is one glance, plus the key hints on the same line.
fn header_line(panes: &[OverviewPane], theme: &ColorTheme, zoomed: bool) -> Line<'static> {
    let mut spans = Vec::new();
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
    let hints = if zoomed {
        "   esc grid · q quit"
    } else {
        "   tab move · enter zoom · q quit"
    };
    spans.push(Span::styled(
        hints,
        Style::default().fg(theme.border_inactive),
    ));
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

    fn render_frame(panes: &[OverviewPane], selected_idx: Option<usize>, w: u16, h: u16) -> String {
        let theme = ColorTheme::default();
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_grid(f, panes, selected_idx, &theme))
            .unwrap();
        buffer_to_string(terminal.backend().buffer())
    }

    fn render_zoom(panes: &[OverviewPane], idx: usize, w: u16, h: u16) -> String {
        let theme = ColorTheme::default();
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_zoomed(f, panes, idx, &theme))
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
        insta::assert_snapshot!(render_frame(&[pane], None, 40, 10), @"
         ●1   tab move · enter zoom · q quit
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
        insta::assert_snapshot!(render_frame(&panes, Some(0), 80, 12), @"
         ◐1 ●1   tab move · enter zoom · q quit
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
        insta::assert_snapshot!(render_frame(&[pane], None, 40, 6), @"
         ✕1   tab move · enter zoom · q quit
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
        insta::assert_snapshot!(render_zoom(&[pane], 0, 40, 12), @"
         ●1   esc grid · q quit
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
    fn snapshot_empty_state() {
        insta::assert_snapshot!(render_frame(&[], None, 40, 6), @"



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
            .draw(|f| draw_grid(f, panes, selected_idx, &theme))
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
            .draw(|f| draw_grid(f, &panes, None, &theme))
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
}
