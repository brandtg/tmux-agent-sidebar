# tmux 3.8 Integration Track

Living record for leveraging tmux 3.8 features. 3.8 shipped recently (floating
panes overhaul, themes, OSC 133 hook events, set-hook monitors); the dev
machine already runs 3.8, so this work can be developed and verified locally.

Origin: 2026-11-18 review of the 3.8 CHANGES against the codebase's tmux
integration inventory. Every claim below was checked against current source.

Status: **T0 spike complete (2026-11-18)** — findings recorded below; T1 is the
next batch. All spike facts were measured on tmux 3.8 in a scoped
`tmux -L tas38-probe` server (`-f /dev/null`, kill-server in an EXIT trap),
per the HARD RULE in AGENTS.md. Raw probe scripts and artifacts:
/tmp/opencode/tas38/ (not committed).

## What the codebase already handles passively

No work needed — recorded so nobody re-investigates:

- Layout strings: 3.8 switched to a JSON subset format that includes floating
  panes; the old format is still accepted. `#{window_layout}` consumers accept
  both shapes already (src/cli/toggle.rs:336-358).
- `show -g` decoding covers 3.8's single-quoted values, `\$`, backtick, `\t`,
  doubled backslashes, with verbatim fixtures (src/tmux/options.rs:369-439).
- `#{q:...}` field quoting: on 3.8, `q:` now escapes `%` (a pane id comes back
  as `\%0`). The pane-line splitter already unescapes backslashes generically
  (src/tmux/query.rs:990-1023), so ids decode back to `%0` with no change.
- Version gating exists twice over: the conf gates 3.2-only hooks with an
  `if-shell 'tmux -V | awk …'` probe on every load (agent-sidebar.conf:273-291),
  and Rust has a probe-once-remember pattern in clipboard.rs
  (`static W_SUPPORTED: OnceLock<bool>`, src/clipboard.rs:39-63, remembering
  `set-buffer -w` capability for the process lifetime). New 3.8-gated features
  should follow one of these two established patterns — conf gate for
  conf-registered wiring, OnceLock for in-process decisions.

## Batch T0 — Scoped-server spike (complete)

Findings, per original question:

### Floating-pane lifecycle

- `new-pane` is the 3.8 floating-pane command — it creates a floating pane by
  default (`-L` makes it behave like `split-window`). `split-window` cannot
  create a floating pane directly, but splitting a floating target creates an
  adjacent floating pane (verified).
- Plain `new-pane` prints nothing; capture the new pane with
  `new-pane -P -F '#{pane_id}'` (works despite `-P` being absent from the
  synopsis). Flag inventory: `new-pane` has `-A` (float above zoomed), `-O`
  (modal), `-B` (border lines), `-T` (title), `-W` (wait for exit), plus
  `-x/-y/-X/-Y` geometry accepting `%`. `split-window` has `-B`/`-T` but no
  geometry flags and no `-A`/`-O`/`-W`.
- Requested `-x 60 -y 12` yields an actual 58x10 — the request includes the
  border. Unspecified position cascades from the window's top-left.
- `-A` verified: pane floats while `#{window_zoomed_flag}` is 1.
- `-O` modal verified as modal=1/floating=1/always-active. `-D` Escape-close
  could not be triggered via `send-keys` (it reaches the pty, not the client
  key path) — unverified, and modal is not needed for T1.
- `-W` verified: blocks until the command exits and propagates its exit status
  (`-W 'true'` → rc 0, `-W 'false'` → rc 1); the pane self-closes.
- `resize-pane -x/-y` sets absolute size on floating panes;
  `-U/-D/-L/-R` move them. `break-pane -W` lifts a tiled pane out as floating
  and errors cleanly ("pane is already floating") on a floating source.
- Pane exit closes the floating pane like any split pane.

### list-panes -a with floating panes

- Floating panes appear in `list-panes -a` and in `list-panes -s`; the
  project's 36-field format (src/tmux/query.rs:81-121) resolves all 36 fields
  for them. `window_id`/`pane_id` are normal and stable.
- `#{pane_floating_flag}` is 1 (0 for tiled); `#{pane_modal_flag}` exists.
  `#{pane_left}`/`#{pane_top}` report the float position — this is the source
  of the T1 prerequisite below.

### Layout strings

- A 3.8 JSON layout containing floating panes: each floating pane is a cell
  with `w/h/x/y`, a `z` stack order, and `I` (pane id), e.g.
  `{"V":2,"L":{"t":"v","w":120,"h":40,"x":0,"y":0,"c":[{"t":"p",...,"z":2,"I":"%1"},...]}}`.
  Fixtures captured to /tmp/opencode/tas38/layout-*.json; promote into
  toggle.rs unit tests when T1 touches the parser.
- Replaying a floating-containing layout with `select-layout` succeeds (rc 0).
- Replaying a stale layout whose cell count no longer matches is rejected by
  tmux itself (rc 1, "unexpected trailing data") — the maintain-width restore
  path cannot corrupt a window that has gained/lost a floating pane.
- `#{window_width}`/`#{window_height}` are unaffected by floating panes, so
  the maintain-width guard inputs are unchanged.

### Floating pane as TUI host

- The Ratatui TUI runs in a floating pane: TMUX_PANE is set (real pane), it
  enters the alternate screen, renders, redraws on `resize-pane` resize, and
  quits on `q` leaving no residue. This answers the popup-replacement thesis
  of T1.
- Capture note (cost an hour, recorded to save the next one): for a pane in
  the alternate screen, default `capture-pane -p` reads the DISPLAYED grid
  (the TUI), while `-a` reads the hidden base grid (blank). Do not use `-a`
  to verify TUI frames.

### OSC 133 hooks

- Mapping verified by emitting sequences from a pane: `A` →
  `pane-shell-prompt`, `C` → `pane-command-started`, `D;status` →
  `pane-command-finished`. `B` fires nothing.
- Cadence with bash integration (`PS1`=A+B, `PS0`=C,
  `PROMPT_COMMAND` emitting `D;$?`): per command, started (running=1) →
  finished (status correct, `pane_command_start_time`/`end_time`/
  `duration` populated) → prompt. Exit status flows (`false` → status=1).
- Hook context formats: `hook_pane`, `pane_command_status`,
  `pane_command_running`, `pane_command_start_time`, `pane_command_end_time`,
  `pane_command_duration`, `pane_last_prompt_time` all resolve.
- Gap: no format exposes the command TEXT. `@pane_last_cmd` (the shell-exit.sh
  write T2 wants to replace) gets its text from the DEBUG trap; OSC 133 gives
  tmux only timing/status. The shell integration must supply the text itself
  (e.g. write the option from PROMPT_COMMAND/PS0) or the B..C screen region
  must be captured. Design decision needed before T2.

### Theme reporting

- Reachable from formats: `#{client_colours}` (count — 16777216 for foot
  1.28 here), `#{I/f:RGB}` (1), `#{client_termfeatures}` (includes RGB),
  `#{client_termname}`. All client-dependent — empty on a clientless server.
- NOT reachable: the detected light/dark theme. There is no format variable
  for it; `#{themeblack}` and friends resolve EMPTY in `display-message`
  even with a real attached client; `client-light-theme`/`client-dark-theme`
  hooks exist (man-confirmed) but could not be made to fire in the lab with
  script-pty clients, control clients, or a full synthesized handshake
  (DA1 + OSC 10 + OSC 11 with mid-session switches). tmux does query
  OSC 10/11 at attach (byte-verified from the client side), and panes get no
  answer to their own OSC 11 query. T4 is re-scoped accordingly below.

### set-hook monitors

- Working syntax: `set-hook -B '@name:@*:#{format}' 'command'` — `what` is a
  selector (`%0`, `%*`, `@0`, `@*`), not the word "window". Fires at most
  once a second on format change (verified on `window_zoomed_flag`).

## Batch T1 — Floating pane as sidebar host (flagship)

Today the sidebar is a split pane on wide viewports and a `display-popup -E`
on narrow ones (src/cli/toggle.rs:1153-1183). The popup path carries real
workarounds: popup children get no `TMUX_PANE` and the bridge/TUI must have it
pinned manually (toggle.rs:1145-1152), and the popup is invisible to
`list-panes`, so the TUI cannot see itself (the `SIDEBAR_POPUP=1` marker
exists because of this).

A 3.8 floating pane is a real pane: it appears in `list-panes`, carries pane
options, gets `TMUX_PANE` for free, and can be selected like any pane. T0
confirmed the TUI runs, renders, resizes, and exits cleanly in one.

- [ ] Prerequisite (correctness on 3.8 today, independent of the floating
      sidebar): `target_pane_for_position` (toggle.rs:1059-1066) picks the
      split anchor by `pane_left`, and floating panes report their float
      position there — a pane parked right of the tiled layout would be
      chosen as the anchor, and splitting it yields another floating pane
      (verified). The geometry query must add `#{pane_floating_flag}` and
      filter floats out of anchor selection.
- [ ] On tmux >= 3.8, host the narrow-viewport sidebar in a floating pane
      instead of a display-popup; delete the popup-only workarounds that
      become moot (TMUX_PANE pinning; the "TUI cannot see itself" marker
      semantics). Keep the compact-viewport UI behavior the `SIDEBAR_POPUP=1`
      marker drives (setup.rs:20, focus/refresh semantics) — derive it from a
      floating-mode marker or `pane_floating_flag` rather than deleting it.
      Gate: conf `if-shell` version probe (3.8+) with the popup path kept as
      the < 3.8 fallback.
- [ ] The bigger UX win: `new-pane -A` creates a pane that floats above a
      zoomed pane (modal panes do this by default; T0 verified `-A` directly).
      A "sidebar stays visible while an agent pane is zoomed" mode is
      impossible with a split today — explore it as an opt-in setting.
- [ ] Layout safety (T0-resolved risks to carry into tests): stale layout
      replay is rejected by tmux when floating panes change the cell count
      (safe); replay of a snapshot that includes floating panes succeeds.
      Promote the captured JSON layout fixtures into toggle.rs tests.

## Batch T2 — OSC 133 hooks vs shell-exit.sh

3.8 fires `pane-command-started`, `pane-command-finished`, and
`pane-shell-prompt` from OSC 133 escape sequences. fish emits these natively
(no fish on this machine — verify on a fish box before shipping fish docs);
bash/zsh need shell integration. T0 confirmed the sequence→hook mapping and
that exit status and timing formats resolve in hook context.

- [x] Spike first (T0): confirmed event mapping and cadence (see T0).
- [ ] Decide how `@pane_last_cmd` gets its text: tmux exposes no command-text
      format from OSC 133, so the integration snippet itself must write the
      option (PROMPT_COMMAND/PS0-side) while the hooks provide
      timing/status-driven writes for `@pane_last_exit`. Design before wiring.
- [ ] Wire the new hooks to the same `@pane_last_cmd` / `@pane_last_exit`
      writes shell-exit.sh performs, with dedupe so a shell that has both
      integration and shell-exit.sh sourced does not double-report.
- [ ] Keep shell-exit.sh as the fallback for shells without OSC 133; document
      the precedence.

## Batch T3 — New hooks for reactivity

T0 verified the relevant hooks fire on a scoped server. Correction to the
original plan: there is no `window-swapped` hook — 3.8 has `window-zoomed`,
`window-unzoomed`, `window-created`, `window-closed`, `window-layout-changed`,
`window-pane-changed`, `window-linked`/`window-unlinked`, `pane-moved`,
`pane-resized`, and the `pane-mode-*` family. Swap operations surface only
through `window-layout-changed`.

- [x] Spike: `window-zoomed`/`window-unzoomed` fire on zoom toggles;
      `pane-moved` fires on break-pane; `pane-resized` +
      `window-layout-changed` fire on floating-pane create/resize.
- [ ] Register window-zoom/unzoom hooks (gated 3.8+) so auto-hide/restore
      reacts to the user's actual zoom action instead of inferring it
      (newest main commit 566eeae infers conditions today).
- [ ] Register `pane-moved` so a break-pane'd agent pane regroups immediately
      rather than on the next poll.
- [ ] Evaluate `set-hook -B` monitors (verified syntax and ≤1/s on-change
      cadence in T0) against the current 1 s poll — only adopt where they let
      the poll stretch, not on top of it.

## Batch T4 — Light/dark theme support

T0 found the reachable theme signal to be thinner than the CHANGES suggest:
`client_colours` is a colour COUNT, the `theme*` named colours resolve empty
in display-message contexts, no format carries the detected light/dark state,
the `client-light-theme`/`client-dark-theme` hooks did not fire for any lab
client (script-pty, control, synthesized full handshake), and panes get no
OSC 11 answer. What a real client exposes was verified read-only against the
live server (foot 1.28): colours=16777216, I/f:RGB=1, RGB in termfeatures.

- [x] Spike (T0): theme signal mapped; see T0 findings.
- [ ] Before any auto-detection work, verify the client-theme hooks on a real
      interactive client (not a lab pty). If they fire there, wire a conf hook
      writing `@sidebar_terminal_theme` and read it from the TUI (established
      conf-hook pattern).
- [ ] Regardless of detection: add a manual `@sidebar_theme=light|dark`
      override that switches the palette (src/ui/colors.rs), so light-terminal
      users have an escape hatch now.
- [ ] Auto light/dark variants gated on the reported terminal theme — scope
      only after the hook verification above.

## Batch T5 — Smaller items (opportunistic)

- [x] Spike: `capture-pane -I` prefixes each line with the epoch time it
      ENTERED HISTORY; lines still in the visible region report 0. Usable by
      the approval-prompt worker (src/app/workers.rs:327-343) only for lines
      that have scrolled into history — a partial skip optimization, not a
      full re-match elimination.
- [x] Spike: `set-hook -B` monitors work (`'@name:@*:#{format}'` syntax,
      ≤1/s on-change). Post-batch-9 the poll is cheap — revisit only if idle
      CPU shows up as a complaint.
- [x] Spike: `new-window -E` creates an empty pane (its
      `pane_current_command` reports the default shell); `new-pane -I` pipes
      stdin into an empty pane. Adopt in the spawn flow
      (commands.rs:136-159) only if it simplifies it.

## Constraints

- Every feature gated on tmux >= 3.8 must leave the < 3.8 behavior intact and
  tested; the conf gate pattern (agent-sidebar.conf:273-291) is the template.
- Live tmux server safety: all probes in scoped servers only (`tmux -L` or
  private-dir socket), per the HARD RULE in AGENTS.md.
- Update this doc's progress log as batches land; move per-item findings from
  the spike into the batches they unblock.

## Progress log

- 2026-11-18 — Track created from the tmux 3.8 CHANGES review. Codebase
  confirmed already 3.8-tolerant (layout strings, show -g quoting). All
  batches open; T0 gates T1-T5. main @ 566eeae (v1.3.3).
- 2026-11-18 — T0 spike complete on tmux 3.8 (scoped `tas38-probe` server).
  Floating-pane lifecycle, list-panes -a inclusion, JSON layout behavior, and
  TUI-as-floating-host all confirmed; OSC 133 sequence→hook mapping and
  set-hook -B monitor syntax verified; theme detection proved unreachable from
  formats in the lab (T4 re-scoped). New finding folded into T1: split-anchor
  selection by `pane_left` must exclude floating panes (3.8 correctness fix).
  Two doc claims corrected: `new-pane` prints nothing without `-P`; no
  `window-swapped` hook exists.
