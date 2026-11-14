# Handoff: Push-based program detail (nvim first)

Status: design agreed, nothing built. Come back here when picking this up.

## Context

- 2026-11-11, branch remove-process-subtitle (commit 07a1d85) removed the process
  card's second line (the gray repo/branch subtitle) and deployed it. Process
  cards are now single-line in both densities.
- Standing UI contract that motivated it, and that any follow-up must respect:
  - text_inactive gray is exclusively agent text (prompt previews, Waiting for
    prompt…, todos). Scanning for "what needs me" is a pure gray-scan.
  - Magenta triangle = process family, green = agents. Never blur the split.
  - Process cards render one line; detail rides line 1.

## The idea

Give common programs richer detail on their process row, the way agents get
rich state. The standout case is nvim: buffer name, modified flag, and mode().
The mode is arguably the more valuable signal — human presence ("actively
typing" vs "reading"), which complements the sidebar's purpose.

Scope calibration: editors are basically the whole category. less, man,
lazygit, htop, git already show what matters via args; TUIs have no queryable
state. Possible later riders: vim (--remote-expr, clientserver often off),
emacs (emacsclient -e, needs server). Ship nvim alone first.

## The decision: push, not pull

- Pull (a probe: find the nvim pid in the pane's process tree, discover its
  socket, poll via nvim --server … --remote-expr) was designed and rejected.
  Owner preference: no pull. Push also gives instant events and no polling
  cost, at the price of opt-in install — acceptable, dotfiles handle that.
- Push is the established pattern here; an nvim plugin would be the fourth
  instance of it:
  1. agent hook scripts (adapter/ + hook CLI),
  2. the OpenCode bridge (.opencode/plugins/tmux-agent-sidebar.js — plugin
     inside the tool, spawned fresh per event, pushing through the same CLI),
  3. the generated Antigravity bundle (setup antigravity: the sidebar
     generates emitter files into the tool's config dir).
- Important: do NOT route nvim through the agent model. adapter/ and
  AgentEventKind are shaped around agent lifecycle and drive the green status
  diamond. nvim has no lifecycle. It stays a process row (magenta) that
  carries a pushed detail, the way ports ride pane runtime state today.

## Architecture

Emitter — a single nvim-lua file, auto-loaded from ~/.config/nvim/plugin/:

- Autocmds: VimEnter, BufEnter, BufWritePost, DirChanged, ModeChanged, VimLeave.
- Each fires one spawn of the hook CLI with a small JSON payload:
  program, pid, buffer basename, modified flag, mode(), cwd.
- Pane identity: TMUX_PANE is inherited by nvim, so the CLI resolves the pane
  from env exactly like agent hooks do. The emitter never needs to know it is
  in tmux. No-op when TMUX_PANE is unset (nvim outside tmux).
- ModeChanged is the chatty one (fires constantly while typing). One fork per
  event is fine — the CLI is spawn-per-event by design — but add a ~200ms
  debounce timer as cheap insurance.

Receiver — new, program-agnostic (not nvim-specific):

- A hook program subcommand that writes ONE structured tmux option on the
  pane, same shape as WorktreeMetadata (existing precedent for structured
  option payloads), plus a timestamp for freshness.
- Renderer applies it in the args slot on line 1, styled text_muted (the args
  shade — NOT text_inactive): `◆ nvim main.rs [+] · insert`. Buffer basename
  keeps line 1 compact.
- No detail, no match, or no reply → renders exactly today's row. Degrades to
  the status quo, so the receiver alone is shippable.

## Correctness rules to pin (with tests)

- Presence vs detail: the ps-scan already decides the foreground program each
  refresh. Apply the pushed detail only when its recorded program matches the
  current foreground basename; clear the option when they diverge. This is
  what stops a stale nvim detail leaking onto the shell after :q.
- Only trust the option for the pane it was written to (hook writes scoped to
  $TMUX_PANE; the existing per-pane option machinery already guarantees this).
- nvim :suspend, piped nvim, plugin unloaded: emitter no-ops or receiver
  mismatches and ignores. No new state paths.
- Keep the read-only property if the design ever drifts toward RPC: never
  send keystrokes into the editor, only read state.

## Build order

1. Receiver: hook program subcommand + @pane_program option + renderer guard.
   Testable with snapshot tests and no nvim present. Degrades to today's row.
2. Emitter: the lua file + its payload contract.
3. setup nvim: writes the lua file (and prints the one-liner), mirroring how
   the agy bundle is generated. Dotfiles just run setup.

Two clean commits minimum; each shippable alone.

## Code pointers

- src/ui/panes/row.rs — window_row: line-1 args slot where detail renders;
  note the test window_row_never_renders_an_elapsed_counter (elapsed time is
  a pinned non-feature — do not add it while here).
- src/ui/panes/row_collector.rs — where ports are pulled from pane runtime
  and passed into window_row; detail would ride the same call.
- src/process.rs — the ps-scan that classifies the foreground program
  (basename match guard hooks in here).
- src/cli/hook.rs + src/cli/hook/handlers.rs — how hook events become tmux
  pane option writes + activity log entries.
- src/tmux/options.rs — option names; add the new option here.
- src/group.rs — WorktreeMetadata, the structured-option precedent.
- src/app/workers.rs — background thread patterns, if anything needs caching.
- .opencode/plugins/tmux-agent-sidebar.js — the bridge to mimic for push
  semantics (including its deploy gotcha: the opencode daemon caches the
  module until restart; see Debugging section of the root AGENTS.md).

## Open questions (decide when building)

- Buffer basename vs full path on line 1 (basename recommended; full path
  could feed the search matcher instead).
- Should pushed detail participate in search (state/search.rs matches
  pane_title today)? Probably yes for the buffer name.
- Timestamp source: emitter-supplied epoch vs receiver write-time.
- Whether a shell-level emitter could later revive the ssh remote-title info
  that died with the subtitle (same pipe, different emitter). Low priority.

## Deployment notes (for later)

- Receiver changes: normal build + copy to
  ~/.tmux/plugins/tmux-agent-sidebar/bin (see Debugging in root AGENTS.md).
- The lua emitter is copied by setup, not by the release build; changing it
  means re-running setup nvim on each machine.
