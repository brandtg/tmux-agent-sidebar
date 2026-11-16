# Agent chyron (top-row event channel)

> **Status:** draft, 2026-11-15. Branch `agent-chyron`.
>
> **Update (2026-11-15):** implemented as designed. Two deviations from
> the letter of the draft, both simpler than what was written: (1) the
> `primed` first-tick flag turned out to be unnecessary — the first
> refresh diffs against an empty `repo_groups` baseline, so no
> transition can exist yet; startup floods are impossible by
> construction. (2) The label resolves live at render time (renames
> stay current) with the fire-time title kept only as a fallback for
> vanished panes; `tail_fit` keeps the trailing verb on narrow rows.
> The old `@sidebar_color_all` / `@sidebar_icon_all` options died with
> the bar's `≡` glyph and were removed alongside the filter options.
>
> **Update (2026-11-15, overlay):** after living with it, the reserved
> row read as dead air — chyron state is per-process, so every restart
> blanked it again. The dedicated row is gone: the header is one row,
> the toolbar, and the chyron overlays it for the flash/fade window
> only, then the toolbar returns (immediately on consumption, or when
> `/` or the menu opens — both dismiss the event outright). The list
> gains the reclaimed row. On the same pass the two toolbar buttons
> merged: the right-edge button reads `ⓘ ▾` when a notice exists and
> `— ▾` otherwise, and opens one dropdown whose notices sections
> (version, plugin, missing hooks with their `[copy]` / `[prompt]`
> slots) sit above the repo list; the standalone notices popup is gone.
> The search input is now flush left. `l` keeps its vim "go into"
> binding alongside `→`.
>
> **Update (2026-11-15, linger):** the 1.6 s flash+fade was readable
> only as a jolt — it caught the eye and vanished before the name
> could be parsed. The lifecycle is now flash (600 ms, background tint
> + brightest foreground) → linger (3 s, plain colored text in the
> event hue, no background) → fade (800 ms) ≈ 4.4 s total. The ramps
> encode the phases: step 0 is the flash foreground, step 1 the linger
> foreground, the rest the fade. On the same pass the menu button
> stays visible and clickable in the banner's corner (the banner
> leaves its three columns free and tints its cells to match the
> flash background), so a click there opens the menu — dismissing the
> event — instead of selecting the pane.
>
> Replaces the status filter bar in the agents panel's top row with a
> one-line event channel: a transient, self-fading banner that names the
> agent an event just happened to. The filter bar is deleted in the same
> change; the second header row absorbs its surviving controls (notices
> indicator, search, repo filter) into a single merged toolbar.

## Goal

The sidebar's list is a *state* channel: rows show what is (status
icons, attention markers, git info). Nothing shows *what just changed*.
Attention flags clear the moment a pane gains focus and statuses mutate
silently, so once a moment has passed the only trace of it is whatever
the rows happen to show *now*. The old filter bar tried to be the event
surface and failed at it — its counts were state summaries, its filters
just hid rows.

The chyron is the event channel: when a pane's attention flag or status
transitions, the top row flashes the event — glyph, session name, verb —
in the event's color, fades over ~1.6s, then rests as dim text naming
the last event until the next one replaces it. Clicking it selects the
pane and scrolls it into view.

The animation budget is near zero because the machinery already exists:
the event loop redraws on a 200ms spinner tick, and the palette already
carries luminance ramps for exactly this kind of effect
(`SPINNER_PULSE`, `DONE_PULSE`, `PROCESS_PULSE` in `src/lib.rs`). The
chyron is one more ramp, indexed by elapsed time instead of an infinite
breathe.

## Background: why the filter bar goes

The status filter (All/Running/Background/Waiting/Idle/Error) had three
functions and each is better served elsewhere or not worth serving:

- *Filtering by state* — state is already encoded per row (glyph +
  color); hiding rows answers "what's here?" worse than the list does.
  The one valuable destination, "what needs my eye", is `a`
  (`jump_to_next_attention`) without hiding anything.
- *Aggregate counts* — only pay off when the list overflows the
  viewport; a permanent row is too expensive for a rarely-true
  condition.
- *Persistent shared filter* — the only filter that hides rows *and*
  persists across sidebar instances (via `@sidebar_filter`), which is
  the classic "where did my agent go?" footgun.

Search (`/`) matches session/agent/branch text, not status, so nothing
else filters by state — but the fix for that is navigation (`a`), not
row-hiding. `matches_window` exists purely to feed the filter views and
dies with them.

## Decisions (settled)

- **Placement:** dedicated top row, permanent and reserved. The header
  stays two rows tall; the chyron inherits the old filter row rather
  than freeing it. An overlay that transiently borrows the toolbar row
  would net a row back but makes controls vanish mid-fade and needs
  mode-dependent click targets — rejected for v1, revisit if the chyron
  earns its keep.
- **Toolbar merge:** row 2 becomes the only toolbar — notices `ⓘ`
  (left), `/` search input (left, when active), repo dropdown (right).
  Same controls as today's secondary header, unchanged behavior.
- **Triggers:** attention flag `None → Done` ("done"), `None →
  Notification` ("waiting"), and `PaneStatus → Error` ("failed"). No
  "agent started" events — the spinner already shows that and it would
  be constant noise.
- **One slot, latest wins.** Severity breaks same-tick ties: error >
  waiting > done. No queue, no cycling ticker.
- **One-shot lifecycle:** flash → fade → rest. No breathing while
  unseen — infinite motion in an ambient surface is distracting, and
  the row's own attention marker already breathes (`DONE_PULSE`) until
  consumed.
- **Rest state:** the last event stays as dim text on the default
  background. A blank row is dead space, and "what was the last thing
  that happened" is a question no other surface answers.
- **Consumed = rested:** if the chyron's pane gains focus (its
  attention flag clears — the existing rule in `PaneAttention` docs) or
  the pane disappears, the chyron drops to rest immediately, skipping
  the remaining fade.
- **Click = select + reveal.** Clicking the chyron selects the pane's
  row and scrolls it into view. It does not jump into the pane —
  Enter/Right remain the explicit jump, consistent with list rows.
- **Chyron state is per-process and transient** (like `SearchState`),
  not persisted to tmux options and not part of the shared global
  state. Every sidebar diffs the same global flags, so sidebars
  converge on the same chyron naturally; consumption (flag clear) is
  already global via tmux.
- **Fixed verbs, no `wait_reason`.** `done` / `waiting` / `failed`.
  `wait_reason` is arbitrary hook text and can be long; surfacing it is
  a future idea.
- **Tiny sidebars:** when the panel has fewer than two rows, the
  toolbar wins the remaining row and the chyron collapses. Functional
  controls beat an informative line.

## Out of scope

- Event history view, queues, or a scrolling ticker.
- Window (non-agent pane) events — a failed `window_finished` task is
  the obvious next trigger and is deliberately deferred; the diff point
  should not preclude it.
- `wait_reason` text, prompt snippets, or any payload beyond glyph +
  name + verb.
- Sound or desktop-notification changes — those stay on the hook side.
- Overlay mode (chyron borrowing the toolbar row).
- Per-sidebar configuration; the chyron is always on. If it ever needs
  an off switch, that is a follow-up option, not a v1 requirement.

## Header layout

```
 ✓ fix-auth done              ← row 1: chyron (blank until first event)
 ⓘ /query             — ▾    ← row 2: merged toolbar
 ▾ brandtg
   ● claude-api   main +2
   ◐ codex-ui     waiting
```

`PaneLayout` renames `filter_area` → `chyron_area` and keeps
`secondary_area` as the toolbar. On height < 2 the toolbar takes row 0
and `chyron_area` is zero-height (today's tiny-area test has the filter
winning; the priority flips).

The secondary-header renderer moves out of `filter_bar.rs` into the new
`ui/panes/chyron.rs` (or stays beside the toolbar render — one module
per row, owner of its own click columns). `notices.button_col` and
`layout.repo_button_col` keep their current contract: the repo button
stays pinned to the right edge whether or not search is open.

## Event model

### Source

The refresh loop applies a fresh pane snapshot every tick. The diff
point is immediately before `state.repo_groups` (and
`state.pane_states`) are replaced: compare each pane's
`(status, attention)` old → new by `pane_id`.

- `attention: None → Done` → event kind `Done`
- `attention: None → Notification` → event kind `Waiting`
- `status → PaneStatus::Error` (from any status) → event kind `Failed`
- `attention: non-None → None` → consumption signal (see lifecycle)
- Status transitions to anything else → ignored

Flags set by a pane that appears new mid-session (created after the
first tick) with an attention flag already set are real events and
fire. The one suppression rule: the first tick after sidebar startup is
baseline only — otherwise every pre-existing attention flag fires at
once and the restart floods the row.

### Replacement

A new event always replaces the current one (restart its timer), even
from the same pane. Multiple transitions in one tick resolve by
severity (Failed > Waiting > Done), then by pane order for ties.

## Lifecycle and visuals

Phases, on the 200ms frame grid (all timings from `shown_at.elapsed()`):

1. **Flash** (frames 0–2, 600ms): full event color as the glyph and
   text foreground, plus a one-step darker background tint of the same
   hue across the row.
2. **Fade** (frames 3–7, 1000ms): background released; foreground walks
   a short luminance ramp from the event color down toward the theme's
   muted color.
3. **Rest** (any time after): dim text (`theme.filter_inactive` or
   equivalent) on the default background, until replaced.

Proposed ramps live in `src/lib.rs` beside the existing pulses
(`CHYRON_DONE`, `CHYRON_WAITING`, `CHYRON_FAILED`), one `&[u8]` of
256-color indices per kind, brightest first. Exact indices get tuned in
implementation against both dark and light terminals — the constraint
is that every flash frame must keep the foreground readable (dark
one-step tints only: green `22`, amber `58`, red `52` are the working
candidates for the flash background). The ramp personality should match
`SPINNER_PULSE` / `DONE_PULSE` (hot-to-pale, no flicker steps).

Event color mapping follows the existing theme:

- Done → `theme.status_running` green ramp
- Waiting → `theme.status_waiting` amber ramp
- Failed → `theme.status_error` red ramp

### Consumption

If the pane referenced by the current event transitions
`attention → None` or vanishes from the snapshot, the chyron jumps to
rest on that tick. Rest events are history and never re-activate.

## Text format

One line, truncated to the row width with the existing
`truncate_to_width`:

```
{glyph} {session_name} {verb}
```

- Glyph: per-kind, from `StatusIcons`. `Waiting` reuses the existing
  waiting glyph (◐), `Failed` the error glyph (✕). `Done` adds a new
  `done` glyph — default `✓`, ascii preset `*`, nerd preset `U+F00C` —
  following the existing `@sidebar_icon_*` override pattern.
- Session name: the pane's rendered session title (the same
  `session_name` / fallback chain the row uses), truncated first so the
  verb survives on narrow sidebars.
- Verb: `done`, `waiting`, `failed`. Lowercase, one word.

Blank until the first event fires; a blank row renders nothing and
registers no click target.

## Interaction

- Click anywhere on the chyron row while an event is shown (flash,
  fade, or rest): select that pane's row and scroll it into view via
  the existing `line_to_row` mapping. The hit rect spans the row and is
  registered per frame alongside the other click targets; it exists
  only when an event is displayed.
- No keyboard focus mode of its own. The chyron never takes focus and
  never steals keys.
- The bottom flash banner (`set_flash` / `take_flash`) is unchanged and
  keeps its role: bottom row = the user's actions (spawn/remove
  feedback), top row = agent-initiated events.

## State and plumbing

```rust
pub struct ChyronEvent {
    pub pane_id: String,
    pub kind: ChyronKind,        // Done | Waiting | Failed
    pub label: String,           // session title at fire time
    pub shown_at: Instant,
}

pub struct ChyronState {
    pub current: Option<ChyronEvent>,
    pub last: Option<ChyronEvent>,   // rest-state text
}
```

- `ChyronState` lives on `AppState` next to `SearchState` (transient,
  not in `GlobalState`). `current` moves to `last` when its fade
  completes or it is consumed; a new event pushes `current` into
  `last` and takes the slot.
- The transition diff runs in the refresh path where the new snapshot
  is already in hand and the old one is still in `state`; it is a
  plain `HashMap<pane_id, (PaneStatus, PaneAttention)>` comparison —
  no new threads, no new timers.
- Rendering is a pure function of `ChyronState` + `Instant::now()` +
  theme + icons, called from `draw_agents` for `chyron_area`. No new
  wake-up sources: the 200ms spinner tick already redraws through any
  fade.

## Removal checklist

Everything the status filter owned dies with it:

- `StatusFilter` (enum, `VARIANTS`, `next`/`prev`, `from_label`,
  `matches`, `matches_window`) and its tests in `src/state/filter.rs`.
  `RepoFilter` stays.
- `status_counts` and the `window_pane_count` / `pane_count` helpers.
  The collapsed-group badge keeps its waiting/error nonzero segments
  (that behavior is liked and independent) but derives counts from
  rows directly; the "under a status filter the total narrows" rule
  goes. `status_counts` tests in `state.rs` follow.
- `render_filter_bar` and its contract test; the `filter_area` slot in
  `PaneLayout` (renamed, repurposed).
- `Focus::Filter` and every match arm keyed on it. `r` moves to Panes
  focus for the repo popup; the `h`/`l` filter bindings die; `Tab`
  (currently next-filter) is unbound for now.
- `@sidebar_filter` (`SIDEBAR_FILTER`) and `@sidebar_default_view`
  (`SIDEBAR_DEFAULT_VIEW`): constants, `global.rs` parse/save
  (`save_filter`, default-view landing in `setup.rs`), the
  `agent-sidebar.conf` defaults, and the install/upgrade docs.
- The other-windows gate "hidden while a non-All status filter is
  active" — windows render under `show_windows` + repo filter +
  search only.
- `timers.last_filter_click` (filter-click debounce) and the filter
  click targets / mouse handling.
- Help popup: remove filter keys, document the chyron row and the
  moved `r`.

## Testing

- Snapshot tests for the chyron row: blank, flash (each kind, bg tint
  + bright fg), fade frames, rest, consumed-jump. Inline
  `insta::assert_snapshot!` per the UI test rule.
- Diff unit tests: transition → event, severity tiebreak, first-tick
  suppression, consumption on flag clear and pane removal.
- Click contract test mirroring `handle_filter_click`'s: the rendered
  row must contain the rect the handler resolves.
- `PaneLayout` tiny-area tests flip priority (toolbar wins row 0).
- `cargo fmt` before any commit; `cargo clippy` and `cargo test` green.

## Docs

- README: replace the filter-bar description with the chyron + merged
  toolbar; drop `@sidebar_filter` / `@sidebar_default_view` from the
  options table.
- `docs/state-management.md`: remove the `status_filter` row, add
  `ChyronState` with its update frequency (event-driven, per tick).
- `reference/tmux-options`: remove the two dead options.
- This spec's header gets an implementation note once landed, per repo
  convention.
