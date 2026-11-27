# tmux 3.8 Integration Track

Living record for leveraging tmux 3.8 features. 3.8 shipped recently (floating
panes overhaul, themes, OSC 133 hook events, set-hook monitors); the dev
machine already runs 3.8, so this work can be developed and verified locally.

Origin: 2026-11-18 review of the 3.8 CHANGES against the codebase's tmux
integration inventory. Every claim below was checked against current source.

Status: **partially shipped** — the sidebar-aware builtin layouts
(src/cli/layouts.rs, agent-sidebar.conf:341-373) already emit 3.8's JSON
layout trees behind a version gate, and the overview/switcher popups rely
on 3.8's two-positional `display-popup` exec. Batches T1-T5 below remain
open.

## What the codebase already handles passively

No work needed — recorded so nobody re-investigates:

- Layout strings: 3.8 switched to a JSON subset format that includes floating
  panes; the old format is still accepted. `#{window_layout}` consumers accept
  both shapes already (src/cli/toggle.rs:336-358).
- `show -g` decoding covers 3.8's single-quoted values, `\$`, backtick, `\t`,
  doubled backslashes, with verbatim fixtures (src/tmux/options.rs:369-439).
- Version gating exists twice over: the conf gates 3.2-only hooks with an
  `if-shell 'tmux -V | awk …'` probe on every load (agent-sidebar.conf:273-291),
  and Rust has a probe-once-remember pattern in clipboard.rs
  (`static W_SUPPORTED: OnceLock<bool>`, src/clipboard.rs:39-63, remembering
  `set-buffer -w` capability for the process lifetime). New 3.8-gated features
  should follow one of these two established patterns — conf gate for
  conf-registered wiring, OnceLock for in-process decisions.

## Batch T0 — Scoped-server spike (do this first)

Everything in batches T1-T5 depends on facts only a probe can settle. Use a
dedicated scoped server (`tmux -L tas38-probe` or a private-dir socket file,
per the HARD RULE in AGENTS.md) and record findings here.

- [ ] Floating-pane lifecycle: exact `new-pane` / `split-window` syntax for
      creating a floating pane (3.8 CHANGES: `-T` title, `-B` border lines,
      `-W` wait for exit, `-O` modal, `-A` float-above-zoomed); which flags
      exist on `new-pane` vs `split-window`.
- [ ] `list-panes -a` output for floating panes: does the 36-field format
      (src/tmux/query.rs:81-121) resolve for them? Are `window_id` /
      `pane_id` stable and are floating panes included in `-a` at all? What do
      `#{window_width}` / `#{pane_left}` report for a floating pane?
- [ ] Layout-string interplay: capture a JSON layout containing a floating
      pane; confirm toggle.rs's parser tolerates it; check what
      maintain-width/dedupe do when a floating pane sits in the window.
- [ ] Whether a floating pane can host a full-screen Ratatui TUI the way
      `display-popup -E` does today (resize behavior, `-W` semantics, what
      happens on pane exit).
- [ ] OSC 133 hook events: confirm `pane-command-started` /
      `pane-command-finished` / `pane-shell-prompt` fire with fish (native)
      and bash (with shell integration); inspect the hook payload keys
      (`wait-for -v` shows them per the CHANGES).
- [ ] Theme reporting: what `#{client_colours}` / the `I:` modifier return in
      this terminal; whether the `theme` option's detection is usable from a
      format.

## Batch T1 — Floating pane as sidebar host (flagship)

Today the sidebar is a split pane on wide viewports and a `display-popup -E`
on narrow ones (src/cli/toggle.rs:1153-1183). The popup path carries real
workarounds: popup children get no `TMUX_PANE` and the bridge/TUI must have it
pinned manually (toggle.rs:1145-1152), and the popup is invisible to
`list-panes`, so the TUI cannot see itself (the `SIDEBAR_POPUP=1` marker
exists because of this).

A 3.8 floating pane is a real pane: it appears in `list-panes`, carries pane
options, gets `TMUX_PANE` for free, and can be selected like any pane.

- [ ] On tmux >= 3.8, host the narrow-viewport sidebar in a floating pane
      instead of a display-popup; delete the popup-only workarounds that
      become moot. Gate: conf `if-shell` version probe (3.8+) with the popup
      path kept as the < 3.8 fallback.
- [ ] The bigger UX win: `new-pane -A` creates a pane that floats above a
      zoomed pane (modal panes do this by default). A "sidebar stays visible
      while an agent pane is zoomed" mode is impossible with a split today —
      explore it as an opt-in setting.
- [ ] Known risk to verify in T0: layout strings now include floating panes,
      so maintain-width, dedupe, and the scheduled layout-restore
      (toggle.rs:398-436) must be confirmed safe when a floating pane is in
      the window.

## Batch T2 — OSC 133 hooks vs shell-exit.sh

3.8 fires `pane-command-started`, `pane-command-finished`, and
`pane-shell-prompt` from OSC 133 escape sequences. fish emits these natively;
bash/zsh need shell integration.

This can retire the per-command bash DEBUG trap in shell-exit.sh for modern
shells — the trap whose cost a prior audit flagged, and which still leaves the
cosmetic `_unlink_marks` first-`@pane_last_cmd` residue.

- [ ] Spike first (T0): confirm event payloads and cadence.
- [ ] Wire the new hooks to the same `@pane_last_cmd` / `@pane_last_exit`
      writes shell-exit.sh performs, with dedupe so a shell that has both
      integration and shell-exit.sh sourced does not double-report.
- [ ] Keep shell-exit.sh as the fallback for shells without OSC 133; document
      the precedence.

## Batch T3 — New hooks for reactivity

3.8 added hooks covering pane movement and resizing, window creation, close,
swap and zoom. The newest main commit (566eeae, auto-hide/restore on wide
viewport) already infers some of these conditions; the hooks make them
push-driven.

- [ ] Register window-zoom/swap hooks (gated 3.8+) so auto-hide/restore reacts
      to the user's actual zoom action instead of inferring it.
- [ ] Pane-movement hooks: when a user break-panes an agent pane to another
      window, rows regroup immediately rather than on the next poll.
- [ ] Evaluate the activity/`pane-mode` hooks against the current 1 s poll —
      only adopt where they let the poll stretch, not on top of it.

## Batch T4 — Light/dark theme support

3.8 has builtin light/dark themes, detects the terminal theme, reports it to
panes, and expands colours in styles as formats (new `theme*` colour names,
`#{client_colours}`, the `I:` client-info modifier).

The sidebar's palette is a fixed 256-color set (src/ui/colors.rs) — likely
rough on light terminals.

- [ ] Spike (T0): what theme signal is actually reachable from the TUI.
- [ ] Auto light/dark variants of the palette, or theme-aware defaults for
      new installs, gated on the reported terminal theme. User-facing polish;
      scope after the spike.

## Batch T5 — Smaller items (opportunistic)

- [ ] `capture-pane -I` includes the time each line entered history. The
      approval-prompt worker (src/app/workers.rs:327-343) re-captures the last
      15 lines and re-matches them every tick; with timestamps it could skip
      lines it has already judged instead of re-pattern-matching.
- [ ] `set-hook -B` monitors (format checked every second, hook fires on
      change) could let the TUI stretch its idle poll gap for specific
      conditions. Post-batch-9 the poll is cheap — revisit only if idle CPU
      ever shows up as a complaint.
- [ ] `new-window -E` (empty pane) where the spawn flow creates a shell first
      and sends keys — only if it simplifies commands.rs:136-159.

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
