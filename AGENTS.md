## Project Overview

A tmux sidebar TUI (built with Ratatui + Crossterm) that monitors AI coding agents (Claude Code, Codex) across all tmux sessions/windows/panes in real-time. Distributed as a single binary via tmux plugin managers.

## Build & Development Commands

```bash
cargo build                    # Debug build
cargo build --release          # Release build (strip + lto enabled)
cargo test                     # Run all tests
cargo test <test_name>         # Run a single test
cargo clippy                   # Lint
cargo fmt                      # Format code
cargo fmt --check              # Check formatting (used in CI)
```

CI runs `cargo test`, `cargo clippy`, and `cargo fmt --check` on every push/PR.

**Before creating any git commit**, always run `cargo fmt` first to avoid CI formatting failures. This applies to every commit, not just the final one.

After implementation is complete, run `cargo build --release`. The running plugin (`~/.tmux/plugins/tmux-agent-sidebar`) is a git clone of the installed release, not a symlink to this repo — build artifacts and config changes must be copied into it manually (see "Debugging" section below).

## Architecture

### Entry Points

The binary has two modes controlled by CLI args (`src/cli/mod.rs`):
1. **TUI mode** — default. `src/main.rs` handles CLI arg parsing, SIGUSR1 signal wiring, and TUI session setup, then delegates to `app::run` (`src/app.rs`) for the event loop.
2. **CLI subcommands** — `setup`, `hook`, `toggle`, `toggle-all`, `auto-close`, `set-status`, `spawn`, `capture`, `--version` / `version`.

### Core Data Flow

```
Agent hooks (hook.sh) → CLI `hook` subcommand
                           ↓
        adapter/ normalizes raw JSON into AgentEventKind
                           ↓
        event/ builds an internal AgentEvent
                           ↓
        cli/hook/handlers dispatches on_* per event, which:
          • sets tmux pane options (@pane_status, @pane_attention, etc.)
          • appends to $XDG_RUNTIME_DIR/tmux-agent-activity*.log (fallback /tmp)
                           ↓
TUI event loop (app::run) → AppState::sync_global_state()
          • reads tmux panes via single `list-panes -a`
          • parses $XDG_RUNTIME_DIR/tmux-agent-activity*.log (fallback /tmp)
                           ↓
                ui::draw() renders frame
```

### Key Modules

- **`state.rs` + `state/`** — `AppState` central struct plus topical submodules (`activity`, `session`, `focus`, `scroll`, `pane_runtime`, `layout`, `popup`, `notices`, `timers`, `filter`, `global`, `refresh`, `tab`). All UI is computed from this state.
- **`app.rs` + `app/`** — TUI orchestration: `setup` (prime `AppState`), `workers` (background git/session/version threads), `input` (keyboard/mouse handling), `render` (per-frame render entry). Split out from `main.rs` so the binary entry point only handles CLI dispatch, signal wiring, and TUI session setup.
- **`tmux.rs`** — Tmux integration: queries all panes via single `list-panes -a` call, defines `PaneInfo`/`PaneStatus`/`AgentType`/`PermissionMode`/`WorktreeMetadata`.
- **`adapter/`** — Per-agent hook adapters (`claude`, `codex`, `opencode`, `hermes`, `antigravity`). Each exposes a `HOOK_REGISTRATIONS` table binding upstream hook triggers to an internal `AgentEventKind`, plus a `parse()` that maps raw JSON payloads into `AgentEvent`. Single source of truth consumed by the setup wizard, README snippets, and tests. Hermes is the one agent whose wire payload nests event fields under `extra` and whose launcher hides the agent name behind a `python3` shim (see `detect_shimmed_agent` in `src/tmux/query.rs`). Antigravity's executable is `agy` rather than its label, so its process matching goes through the `AGY_BIN` alias, and its `hooks.json` maps hook names to events (no `hooks` wrapper — see `build_antigravity_snippet`); its hook payloads carry no prompt/response text, so the adapter recovers both from the tail of the transcript the payload points at, and session titles come from a `conversation_summaries.db` probe in `src/session.rs`.
- **`event.rs` + `event/`** — Internal event layer: `AgentEvent` (pre-extracted fields; handlers never touch raw JSON or agent names), `AgentEventKind` (compile-time enum for hook kinds), `EventAdapter` trait + `resolve_adapter`.
- **`cli/hook.rs` + `cli/hook/`** — Receives real-time status updates from agent hooks; dispatch in `hook.rs`, with submodules `context` (shared helpers + `AgentContext`), `handlers` (per-event `on_*` handlers), `activity` (activity log writing), `notifications` (desktop notification helpers).
- **`git.rs`** — Git operations (branch, ahead/behind, PR numbers via `gh` CLI, diff stats). Runs in a background polling thread.
- **`activity.rs`** — Parses `$XDG_RUNTIME_DIR/tmux-agent-activity*.log` files (fallback `/tmp`), maps tool types to colors.
- **`debug.rs`** — Opt-in debug trace (`TMUX_AGENT_SIDEBAR_DEBUG=1`): hook-dispatch decisions and tmux-write failures are appended to `$XDG_RUNTIME_DIR/tmux-agent-sidebar-debug.log` (fallback `/tmp`). Silent no-op when unset.
- **`paths.rs`** — Runtime-dir resolution (`$XDG_RUNTIME_DIR`, `/tmp` fallback) and hardened opens (O_NOFOLLOW, `0600` creation) shared by activity logs and the debug trace.
- **`group.rs`** — Groups panes by repository path.
- **`session.rs` / `worktree.rs` / `tool_name.rs` / `version.rs` / `port.rs` / `clipboard.rs` / `desktop_notification.rs`** — Leaf helpers used across modules (session name resolution, worktree metadata parsing, tool-name classification, version reporting, port detection, clipboard + desktop notification shims).
- **`ui/`** — Rendering layer: `mod.rs` (entry `draw`), `panes.rs` (agent list + repo filter) with submodules (`filter_bar`, `row`, `row_collector`, `click_targets`, `popups`); `bottom.rs` + `bottom/` with submodules (`feed`, `repos`) for the cross-agent Feed and Repos tabs; `colors.rs` (256-color theme); `icons.rs` (agent/status glyphs); `notices.rs` (transient banner rendering); `text.rs` (text formatting/truncation).

### State Management

See `docs/state-management.md` for the full scope/update-frequency table, per-pane tmux options, data flow, and key type definitions.

Process-level detail not covered there: SIGUSR1 triggers an instant refresh on tmux pane/window focus change and client session changes (the after-select-pane/after-select-window/client-session-changed hooks all route through `notify-focus`, which signals every live sidebar pid in the affected window; handler in `src/main.rs` flips a shared `AtomicBool` that the `app::run` loop polls).

### Testing

Tests are in `/tests/` using Ratatui's `TestBackend` for UI rendering assertions. `test_helpers.rs` provides buffer-to-string conversion utilities. Heavy use of snapshot-style tests for UI regression prevention.

**UI test rule**: any test that renders a frame MUST use `insta::assert_snapshot!(output, @"...")` inline snapshots — never `assert!(output.contains(...))` or similar substring checks. A contains assertion only verifies that a specific string appears somewhere; it silently tolerates layout drift (border shifts, color changes, row reordering, new artifacts) that a snapshot diff would surface immediately. The stronger check is free — `cargo insta accept` regenerates the expected output when the change is intentional. Substring assertions are acceptable only for non-visual properties (`layout.repo_spawn_targets` contents, state struct fields, etc.) where there is no frame to snapshot.

## Debugging (Local tmux Plugin)

`~/.tmux/plugins/tmux-agent-sidebar` is the production deploy: a git clone of the installed release that tmux loads from. It is NOT a symlink to this repository, so testing changes from a checkout or worktree means copying build artifacts into it, then restarting the sidebar (toggle off → on via the tmux keybinding):

```bash
cargo build --release
cp target/release/tmux-agent-sidebar ~/.tmux/plugins/tmux-agent-sidebar/bin/tmux-agent-sidebar
```

Two copy targets matter:

- `bin/tmux-agent-sidebar` — what tmux actually runs. The entry script (`tmux-agent-sidebar.tmux`) prefers `bin/` over `target/release/` when both exist, so copying only `target/release/` leaves the old binary live (the classic "my change didn't do anything" trap).
- `agent-sidebar.conf` — copy when hook registration or option defaults changed. Hooks are registered once at conf-load time, so a binary-only copy leaves stale wiring (e.g. the after-new-window hook) until the next plugin reload (prefix+R or `bash ~/.tmux/plugins/tmux-agent-sidebar/tmux-agent-sidebar.tmux` — the entry script is bash and must be executed, not `tmux source-file`d; source-file cannot parse it).

The entry script also generates the Antigravity (agy) plugin bundle on every load: `setup antigravity` output → `~/.cache/tmux-agent-sidebar/agy-plugin/{plugin,hooks}.json`, symlinked into `~/.gemini/config/plugins/tmux-agent-sidebar` (agy loads bundled hooks from there at session start). If agy panes get no hook events, check that symlink first — `agy plugin validate` on it should report `hooks 1`. Running agy sessions keep the hooks they started with; they need a restart to pick the bundle up.

A third target exists for OpenCode: `.opencode/plugins/tmux-agent-sidebar.js` (the bridge the OpenCode TUIs load via `~/.config/opencode/plugins/tmux-agent-sidebar.js`). Copy it on every bridge change, then **restart the opencode service daemon** — the daemon imports the plugin module once and caches it for the process lifetime, so bridge edits are invisible until every OpenCode TUI has reconnected to a fresh `opencode serve --service`. This is a silent no-op trap: the Rust half of a bridge change takes effect immediately (the CLI is spawned fresh per hook event) while the routing half keeps running the old module, which makes a correct fix look broken. Verify which module the daemon has by comparing the deployed file's mtime against the daemon's start time (`ps -o lstart -p $(pgrep -f 'opencode.*serve --service')`).

**Reverting** is symmetric: rebuild the main checkout and copy the artifact over the same paths — no git surgery in the plugin clone required.

**Version-check gotcha**: `tmux-agent-sidebar.tmux` compares the installed binary's `version` output against this repo's `Cargo.toml` when the plugin loads. A mismatch (e.g. reloading while testing a worktree build whose version was bumped, or not bumped) triggers the install wizard instead of sourcing the config. Copying the binary and toggling the sidebar avoids this; a full reload with mismatched versions may run the wizard.

**When working in a worktree on macOS (Darwin 24+)**: `cargo` produces a `linker-signed` ad-hoc signature that the kernel will SIGKILL (signal 9) immediately after a `cp` — the kernel refuses to honor a linker-only signature on a file it didn't write itself. Replace it with a fresh ad-hoc signature to avoid the kill:

```bash
cp <worktree-path>/target/release/tmux-agent-sidebar ~/.tmux/plugins/tmux-agent-sidebar/bin/tmux-agent-sidebar
codesign --force --sign - ~/.tmux/plugins/tmux-agent-sidebar/bin/tmux-agent-sidebar
```

If tmux reports `terminated by signal 9` after a worktree build, you almost certainly skipped the `codesign` step. Clearing `com.apple.provenance` with `xattr -c` is not required — the kernel only cares about the signature flavor.

## Safety: tmux server isolation (HARD RULE)

This machine runs a live tmux server (el-default socket) with real work in it. Test reproductions are the only place tmux servers get created or killed.

- NEVER run bare `tmux kill-server`, `tmux kill-session`, `tmux kill-pane`, `tmux kill-window`, or any other potentially destructive tmux command without an explicit socket scope (`-L <name>` or `-S <path>`). A bare `tmux` command operates on the user's live default-socket server.
- Sandbox reproductions must use a dedicated scoped server, e.g. `tmux -L bugtest ...`. Cleaning it up is `tmux -L bugtest kill-server` — scoped flag always required.
- Scope-aware hook testing: children spawned inside a `-L`-scoped server resolve the default socket (the live one), not the scoped one. For a faithful sim, run the scoped server with a socket file named `default` inside a private dir (`tmux -S /tmp/opencode/tsock/default` — the server exports `TMUX_TMPDIR` so in-pane `tmux` CLI calls resolve to the scoped socket), or never spawn hook-side tmux writes from within the sim at all.
- `pkill`/`killall` patterns that can match the user's tmux server or sidebar are forbidden.

## Rust Edition

This project uses Rust edition 2024 (`Cargo.toml`).

## Writing Guidelines

- All documentation under `docs/` and all skill files under `.claude/skills/` must be written in English.
