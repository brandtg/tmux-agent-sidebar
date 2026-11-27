# Agent Overview Grid — Findings and Spike Design

Date: 2026-11-15
Status: shipped — the `overview` subcommand and grid popup are wired in
(opened with `o`, the `☰` header button, and documented in the help overlay)

## Goal

A fullscreen MxN grid that shows the live screen of every running agent at
once — the "twitter flex" view. Open it over everything, close it, and the
original panes must be untouched.

## TL;DR

Tractable, and most of the machinery already exists in this repo. The one
design constraint that matters: never attach to the agent panes. Attaching
couples terminal sizes (a tmux window has exactly one size shared by every
session linking it); a grid of cells smaller than the source panes would
constantly resize the real agents. The correct primitive is `capture-pane`:
read each pane's screen as styled text and render it ourselves. The spike
implements that as an `overview` subcommand.

## Why not attach (probe receipts)

Validated in a scoped tmux server (`-L ovtest -f /dev/null`), tmux 3.8:

- `new-session -t <session>:<window>` links the shared window into the new
  session. A tmux window has exactly one size for all viewers: after
  `resize-window -x 80 -y 24` against the mirror, the original session's
  pane measured 80x24 (was 200x50). An attach-based grid therefore either
  squishes the agents' TUIs on every relayout, or pins `window-size` and
  shows clipped thumbnails with no pan control.
- Teardown safety is not the problem — killing the mirror session left all
  original panes alive and sized. Sizing is the problem.
- `pipe-pane` was also rejected: it streams raw TUI bytes with cursor
  addressing; unusable without a terminal emulator per cell.

## What the probe proved (capture pipeline)

`capture-pane -p -e -t <id>` returns the pane's visible screen as line
text plus SGR color codes, for any pane — background windows, detached
sessions — without touching it (no resize, no input, no mode change).

- ~1ms per capture (80 captures in 21ms when chained into one tmux client
  via bare `;` separators). Twenty agents at 2Hz is negligible.
- Round trip verified: agent panes printed in red/green/blue, a grid of
  shell loops re-printed their captures, and re-capturing the grid cells
  showed the SGR codes survived. The full mirror pipeline works with zero
  new infrastructure.
- Killing grid/mirror sessions left every original pane intact — the
  "close it and everything is fine" requirement holds by construction.

## What the repo already had

- src/cli/capture/ansi.rs — a vte-based parser turning `capture-pane -p -e`
  bytes into a `Vec<Vec<StyledCell>>` grid (fg/bg 256-color, bold,
  reverse, underline). Written for the HTML capture subcommand; the spike
  renders the same grid into a ratatui buffer instead.
- src/cli/capture/tmux_probe.rs — `capture_pane` (capture-pane -p -e).
- src/tmux/query.rs — the single `list-panes -a` sync (`fetch_pane_listing`
  + `build_session_snapshot`) yielding `PaneInfo` with agent detection,
  `PaneStatus`, worktree metadata.
- src/ui/colors.rs — `ColorTheme::from_tmux()` with `status_color` /
  `agent_color`.
- src/cli/toggle.rs — the sidebar already runs itself inside a
  `display-popup -E` container, proving the popup-hosted-TUI pattern.
- src/app/workers.rs — the approval scan already polls capture-pane
  periodically; the worker-thread pattern for v1 exists.

## Design

Pipeline: one `list-panes -a` sync for the agent set (reuse the existing
format and parse so agent detection stays consistent), one geometry query
per tick, then `capture-pane -p -e` per agent pane; parse with the existing
vte parser; paint cells into a ratatui buffer; chrome per cell (status
icon/color, agent label, repo) from `PaneInfo` + `ColorTheme`.

Rendering rules:

- Sources are wider and taller than cells, so content is cropped:
  bottom-anchored (agent TUIs draw current state at the bottom of the
  screen — the top of the screen is stale history) and left-anchored.
  Cursor-following or scrollable crops are follow-ups (`cursor_x`/
  `cursor_y` can be added to the listing format).
- Ordering: the default is sidebar order — cells sort exactly like the
  sidebar's rows (repo groups alphabetically, or session groups under
  `@sidebar_group_by session`, then session name → window index → tmux
  line order, reusing src/group.rs's own grouping and git resolver) so
  the two views share one mental map and cells don't jump when an agent
  starts working. `s` toggles status-first: cells sort waiting/error →
  running → idle, waiting/error get bold borders and a wait-reason strip,
  idle fades to grey. A header row shows per-status counts either way.
  The overview must answer "does anything need me?" in one glance and
  "what is this one doing?" per cell.
- Enter zooms the selected cell fullscreen (still a capture; the one
  write is the temporary window-zoom below, always restored); Esc
  collapses back to the grid. Without zoom, truncated cells are dead
  ends; with it, the grid is a scanner.
- Focus mode briefly zooms the source pane's own window (`resize-pane
  -Z`) so the capture exists at the popup's width — `capture-pane` has
  no size override, and while the pane shares its window with the
  40-column sidebar, content at the popup's width does not exist until
  the pane itself is wider. The zoom rides a drop guard in the run
  loop, so every exit path restores the pre-zoom layout; a window that
  arrived already zoomed is never touched, a pane alone in its window
  skips it, and a source that cannot reach the popup's width even
  zoomed (multi-client setups) centers in the popup instead of hugging
  the left edge.
- Theme normalization: light-themed TUIs (opencode's pink panels,
  claude's white input bar) arrive as explicit light backgrounds with
  dark text — a clash on the dark canvas, and invisible text if the
  background were simply dropped. Captures are normalized: light-themed
  panes (majority explicit light background) lose all backgrounds and
  get dark text lifted to light grey; dark-themed panes keep their
  colors except per-cell light backgrounds.
- Wide (CJK) characters: the existing parser advances one cell per char,
  so wide glyphs render slightly off. Known spike limitation; latin-first.

Containers, cheapest first (the TUI is identical; only the spawner
differs):

1. Popup: `tmux display-popup -E -w 100% -h 100%` running the overview.
   Esc closes; nothing persists.
2. Dedicated session (v2): `new-session -d -s agent-overview` running the
   binary + `switch-client`; on quit the binary switches the client back
   and kill-sessions itself.
3. Mini 2-column mode inside the sidebar column (optional, later).

Interaction ladder:

- v1 (spike): read-only by construction; zoom to read.
- Focus (shipped): `f` targets the zoomed cell when zoomed, else the
  selected cell — closes the container and `select_pane`s to the real pane
  (src/tmux/commands.rs — the same primitive as the sidebar's agent-jump
  path). Enter while zoomed does the same, so zoom-then-Enter steps from
  reading an agent to working in it.
- Prompting (shipped): `p` forwards every key press to the target — from
  the grid (selected cell) or zoomed, the zoom state never changes and the
  cell keeps its size. Keys go over as tmux key names (`send-keys`: `C-c`,
  `M-Enter` for a newline in a prompt, `S-Up`, F1-F12), plain characters
  as literal text, and bracketed pastes as one `paste-buffer -p` block
  (never a key run, or a multi-line paste would submit line by line). The
  agent's own UI — slash menus, completions, permission answers, Esc
  interrupts — runs natively inside the mirrored cell, and the prompted
  pane's capture refreshes at ~16 Hz (60 ms; the rest of the grid keeps
  500 ms — one capture is ~1 ms, see the probe above) so menus render live
  while typing. Ctrl-g is the only key the overview keeps: Esc must reach
  the agent as its interrupt key, so the leave chord is one no agent binds
  (a click leaves too, consuming the click). Forwards are gated per key —
  the pane's foreground is queried fresh (`#{pane_current_command}`), not
  trusted from the up-to-one-refresh-stale listing — and a refused or
  failed forward keeps the prompt open with the reason on the bar; the
  slow tick closes a prompt whose pane vanished or whose agent exited.
  Input must never reach a shell. This replaces the earlier compose bar
  (a LineEdit draft submitted on Enter): once forwards could carry the
  agent's own editing and autocomplete, a second input line in the
  overview was one more thing to learn for less capability.
- Per-cell scrollback: `capture-pane -S` offsets.

## Spike scope

- New `overview` subcommand (src/cli/overview.rs), dispatch from
  src/cli/mod.rs. Not registered in agent-sidebar.conf yet — run manually.
- Single-threaded refresh inside the event loop (listing + captures every
  500ms); v1 moves captures to a worker thread following the
  src/app/workers.rs pattern.
- Grid layout chosen from terminal aspect ratio; selection via
  arrows/tab; Enter zoom; q/Esc quit; `--popup` flag spawns the popup
  container.
- No keybinding, no conf changes, no version bump.

## Findings from the spike

Implemented in this branch (not merged): src/cli/overview.rs with dispatch
from src/cli/mod.rs. Verified with `cargo test` (snapshot tests lock the
grid, strip, zoom, and empty-state frames), `cargo clippy --all-targets`,
and `cargo build --release`.

- The reuse bet paid off. The feature is a few hundred lines of new code:
  the listing (`fetch_pane_listing` + `build_session_snapshot`), the
  styled grid (`ansi::parse_ansi`), the theme (`ColorTheme::status_color`
  / `agent_color`), and the status glyphs (`PaneStatus::icon`) all came
  from the existing tree. No new dependencies.
- One real bug surfaced and was fixed: src/cli/capture/ansi.rs only mapped
  the extended-color SGR forms (`38;5;n` / `48;5;n`), silently dropping the
  basic 30-37/40-47 and bright 90-97/100-107 codes — i.e. the colors every
  agent TUI actually emits, in the HTML capture output too. Fixed with
  unit tests.
- A second, worse parser bug surfaced from the live opencode pane: the
  truecolor forms (`38;2;R;G;B` / `48;2;R;G;B`) were not consumed, so
  their RGB components leaked back into the SGR stream — opencode's
  muted-text color (38;2;110;106;134) turned its G component (106) into a
  solid background-14 bar behind every muted line. The parser now
  consumes the full parameter list and quantizes truecolor to the nearest
  256-color entry, so opencode renders with its actual dark theme
  (verified against a real capture: zero leaked backgrounds).
- The first live look (9 real agent panes) proved the pipe but not the
  product: top-left crops showed stale history instead of current
  activity, status was invisible beyond a tiny icon, and truncated cells
  were dead ends. The v2 iteration addresses exactly those: bottom-
  anchored capture, status-first ordering with loud waiting/error borders
  and a counts header, a per-cell activity strip (wait reason or last
  prompt, from data the sidebar already syncs), and Enter-to-zoom.
- Rendering quality is a non-issue. Captures arrive as a fixed grid of
  styled cells; painting into the ratatui buffer is a straight loop.
- Selection tracks pane ids, not indexes — the status sort reorders cells
  between refreshes and an index would silently move the selection onto a
  different agent.
- Single-threaded refresh (listing + N captures inside the event loop)
  is fine at this scale; input stays responsive because keys are polled
  between cycles. v1 should still move captures into a worker thread per
  the src/app/workers.rs pattern before this ships.

Not in the spike (deliberately): worker-threaded captures, focus-to-pane
via `select_pane`, `send-keys` typing, per-cell scrollback, paging,
repo-major ordering via src/group.rs, and the keybinding in
agent-sidebar.conf.

## How to run the spike

```bash
cargo build --release
# from any shell inside tmux — read-only, safe against the live server:
target/release/tmux-agent-sidebar overview --popup
# or drive the container yourself:
tmux display-popup -E -w 100% -h 100% target/release/tmux-agent-sidebar overview
```

With the plugin installed, the sidebar TUI itself opens it: focus the
sidebar and press `o` — a fullscreen popup of every agent pane, anchored
to the sidebar's window. The `☰` button in the sidebar's header row
(top-left corner) does the same for the mouse, and the grid's own header
row carries a clickable `✕` at its right edge for closing it. Quitting
the grid (q / Esc) returns to the sidebar. From a plain shell the
standalone invocation also works:

```bash
target/release/tmux-agent-sidebar overview --popup
```

For a key outside the sidebar, set `set -g @sidebar_overview_key O` in
`tmux.conf` (before the plugin loads): the shipped conf then binds
`prefix + O` to `overview --popup --popup-client <pressing client>` —
opt-in, no default key is claimed. Bind the command by hand for anything
else (any `run-shell` gets `TMUX_PANE`, so `--popup-client` is optional
there).

Keys: nothing is selected until you move; arrows/tab (or j/k) select and
move with wraparound, Enter zooms the selected cell fullscreen, Esc
collapses zoom (and quits from the grid), q quits. Click a cell to select
it; click the selected cell again to zoom it (the grid layout feeding the
render also drives the click hit-test, so they cannot drift). `s` toggles
the cell order between the sidebar's row order (default) and status-first.
On the targeted cell — the zoomed one when zoomed, else the selected one —
`p` prompts it live: every key goes straight to the agent (slash menus and
all, from the grid or zoomed — no zoom change), ctrl-g or a click leaves,
Esc interrupts the agent. `f` — or Enter again while zoomed — closes the
popup and focuses the agent's real pane.
Refresh interval defaults to 500ms (`--interval-ms N` to change).

## Open questions

1. Which panes qualify: agent panes only (current), idle shells, respect
   the sidebar's filters?
2. Crop policy beyond bottom-left: cursor-centered, or horizontally
   scrollable cells?
3. Popup vs dedicated session as the default container?
4. Refresh cadence; pause when the overview is unfocused?
5. One shared overview session vs per-client instances (v2 only)?
