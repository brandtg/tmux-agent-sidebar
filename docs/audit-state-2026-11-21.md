# State-Management Audit — 2026-11-21

Scope: the runtime state layer as it governs cross-session sidebar consistency, cursor and
selection behavior (flash, jump, stick), and the way view modes interact with the cursor —
`@sidebar_group_by` (repo vs session), `@sidebar_show_windows` (on/off), and the two
window-header levels (`@sidebar_session_windows` / `@sidebar_worktree_windows`). The three
shared "view identity" fields — `selected_pane_row`, `repo_filter`, `collapsed_groups` — plus
the TODO panel's shared visibility are the audit's center of gravity.

Method: read-only trace of every code path that writes or adopts shared state, from the hook
side (`cli/hook.rs` pane-option writes) through the worker-fed snapshot fold
(`app/workers.rs` → `state/refresh.rs`) into the cursor re-anchor pass
(`state/layout.rs::rebuild_row_targets`) and the render-side row collection
(`ui/panes/row_collector.rs`, `ui/panes.rs::compute_scroll_offset`), then back out through
the persistence helpers in `state/global.rs` and the keyboard/mouse handlers in
`app/input.rs` and `state/layout.rs::handle_mouse_click`. Every claim below was verified
against the checked-in source before being recorded; the line references are load-bearing.
No code, test, or existing document was modified.

Ground truth: main @ 8eff345 ("docs: execute the 2026-11-20 docs audit (#152)"), clean tree.

## Handoff notes for the next session

This report is the plan of record. Nothing has been implemented — the change set in Part 3
is proposals only, and each item needs an explicit go-ahead before code changes.

- This is a snapshot. If code has moved since 8eff345, re-verify any High finding against
  source before acting on it.
- The intended contract lives in docs/state-management.md. Where source and that document
  disagree, both sides are cited (see F-1 and F-11).
- Two of the findings (F-14, F-12) are "document the behavior" rather than "change the
  behavior" — they are intentional design decisions that are simply not written down where
  the next reader will find them. Decide per item.
- F-10 (the duplicated row-index contract) is the one structural item: everything else in
  the cursor area is cheap. If only one thing gets done, do that one, because it is the
  finding that makes all the others safe to change.
- Severity scale, same as the 2026-11-20 audit: High = the code does something the stated
  contract does not support, or a shared-state write path can diverge from what is rendered;
  Medium = a behavior that is defensible but inconsistent with its neighbors, or a contract
  that is undocumented; Low = wording, one-off omissions, stale references.

## Summary

Audited: 6 shared tmux options and their adoption rules, 14 cursor/selection key bindings,
9 mouse click classes, 4 view-mode axes (group_by, show_windows, session_windows,
worktree_windows), 5 shared-state write paths, and the startup and focus-gain reconciliation
passes.

Verdict: the core invariant holds. The cursor survives blur/refocus, re-anchors by pane id
across rebuilds, and the highlight and auto-scroll are both focus-gated so an unfocused
sidebar never shows someone else's selection. The echo-guard pattern
(`last_saved_*`) does what docs/state-management.md claims for it. What the audit found is a
long tail of asymmetries — places where one navigation path persists the cursor and its
near-neighbor does not, where a render-side function writes tmux, and where the shared-cursor
contract is stronger in prose than in code.

| Area | Verdict |
|---|---|
| Cursor re-anchor across rebuilds (pane id → within-group offset → clamp) | CONSISTENT |
| Highlight / auto-scroll focus gating | CONSISTENT |
| Shared cursor adoption (echo guard, no runtime adoption) | CONSISTENT |
| Repo filter adoption and stale reset | DRIFT — render-side tmux write (F-2) |
| Collapsed-group persistence | DRIFT — one-shot write (F-4), O(n) observer re-writes (F-3) |
| Cursor persistence parity across navigation paths | DRIFT — five paths never save (F-5, F-9, F-12) |
| View-mode change (s / w) cursor handling | DRIFT — no park, no save, deferred rebuild (F-9) |
| Search cursor behavior | DRIFT — cancel/restore contract unimplemented (F-5) |
| Startup context reconciliation (pane vs popup mode) | DRIFT — asymmetric (F-1, F-14) |
| Group-by change / window-header levels / proxy focus interaction | CONSISTENT |
| TODO panel shared visibility | MINOR DRIFT — reload-branch-gated adoption (F-8) |
| Event-loop reload bookkeeping | MINOR DRIFT — unconditional clock reset (F-7) |
| Row-index contract (targets vs collector) | DRIFT — duplicated logic (F-10) |
| docs/state-management.md refresh claims | DRIFT — two stale lines (F-11) |

Fourteen findings: 3 High, 7 Medium, 4 Low.

---

# Part 1 — High severity

Ordered by risk if left alone.

## F-1. The shared cursor is adopted at startup in pane mode with no context reconciliation

Status (2026-11-21): fixed, first proposal. `select_focused_context_row()` is now called
unconditionally at the end of `init_state` (src/app/setup.rs); the `apply_all` doc comment
(src/state/global.rs) and docs/state-management.md describe the startup reconciliation.
Tests unchanged — the semantics of the call (including its no-op when the context pane is
unknown or filtered out) were already pinned in src/state/scroll.rs.

The documented contract for `@sidebar_cursor` is in src/state/global.rs:517-524: the startup
variant "rides along so a fresh sidebar lands where the user last navigated." In pane mode
that is not what happens, and nothing reconciles it until the user first jumps into the
sidebar.

What the code actually does, traced:

1. src/cli/toggle.rs:158 captures the window's active pane before the split, and
   src/cli/toggle.rs:210-215 restores focus to it after the sidebar pane is created. A
   pane-mode sidebar therefore opens unfocused: `sidebar_focused` is false from the first
   snapshot onward (`apply_snapshot` copies `snapshot.sidebar_pane_active`,
   src/state/refresh.rs:602-608).
2. src/app/setup.rs:21 calls `global.load_from_tmux()` → `apply_all` →
   `apply_options(opts, true)` (src/state/global.rs:525-527), which adopts the shared cursor
   row verbatim, subject only to the echo guard (`n != last_saved_cursor`, and
   `last_saved_cursor` starts at 0, src/state/global.rs:197).
3. src/app/setup.rs:39 runs the startup `refresh()`, whose `apply_session_snapshot` calls
   `rebuild_row_targets()` (src/state/refresh.rs:183) — which clamps the adopted index into
   whatever this sidebar's own list happens to be.
4. src/app/setup.rs:46-49 calls `select_focused_context_row()` only when
   `state.popup_mode`. Pane mode never reconciles at startup.

Net effect: the adopted row index is a placeholder. It renders nothing while unfocused
because the selection background is focus-gated (`sidebar_focused && focus == Focus::Panes
&& row_index == selected_pane_row`, src/ui/panes/row_collector.rs:360-362 and 440-442), and
it is always overwritten before the user can act on it, because the focus-gain transition
calls `select_focused_context_row()` (src/app.rs:318-330) before the first focused frame
renders. So the observable behavior is correct; the claim in the doc comment is not, and the
adopted value is a lie on screen for exactly as long as the sidebar is unfocused — which is
the whole lifetime of a sidebar nobody ever jumps into.

The asymmetry with popup mode is the tell: a popup reconciles at startup (pinned to
`TMUX_PANE`, src/state/scroll.rs:44-47) precisely because popup mode forces
`sidebar_focused = true` from the first pass and therefore renders the selection
immediately. Pane mode has the same exposure at the moment focus arrives via the periodic
tick rather than the signal path — the two are the same race with different windows.

Proposed change: either call `select_focused_context_row()` unconditionally at the end of
`init_state` (it already returns false and touches nothing when the context pane is unknown
or filtered out, src/state/scroll.rs:38-60), or narrow the doc comment at
src/state/global.rs:517-524 to say that in pane mode the shared cursor is a placeholder that
the first focus-gain reconciliation replaces. The first option is three characters of
control flow and removes a class of "the cursor was on the wrong row" reports; the second is
honest and free. Recommend the first, with the second as the fallback.

## F-2. `rebuild_row_targets` writes tmux from a render-side path

`rebuild_row_targets` is the target-list builder. It is called from three places:
`apply_session_snapshot` (src/state/refresh.rs:183, once per snapshot), the event loop's
reload branch (src/app.rs:291, on shared-option reload), and directly from key handlers
(the `w` toggle at src/app/input.rs:294 and 446) and mouse handlers
(`confirm_repo_popup` at src/state/popup.rs:253, `toggle_group_collapse` at
src/state/layout.rs:550, `toggle_all_groups` at src/state/layout.rs:494 and 500). Its name
and its position in the state layer say "pure recompute."

Its first eight lines are not a recompute (src/state/layout.rs:167-180):

    let stale = matches!(self.global.repo_filter, RepoFilter::Repo(_))
        && !self.repo_groups.iter().any(|g| self.global.repo_filter.matches_group(&g.name));
    if stale {
        self.global.repo_filter = RepoFilter::All;
        self.global.save_repo_filter();
    }

`save_repo_filter()` spawns `tmux set -g @sidebar_repo_filter all` (src/state/global.rs:262-274).
So every rebuild performs a cross-instance shared-state write whenever the repo filter points
at a repo that is momentarily absent from the grouping. Two consequences:

- Frequency. "Every rebuild" includes every snapshot tick. A repo whose panes flap in and
  out of existence — a worktree being created or torn down, an agent pane exiting between
  hook events — produces a tmux write per tick. Worse, a failed `list-panes` is already
  guarded against (`refresh()` holds the last known-good snapshot rather than applying an
  empty one, src/state/refresh.rs:548-556) precisely because the comment there notes that
  "rebuild_row_targets would reset the repo filter to All and persist that reset in a tmux
  global option — surviving restarts and propagating to every sidebar instance." The
  mitigation covers the synchronous path; the worker-fed path reaches the same function with
  no such note.
- Ownership. The write carries this instance's judgment about "stale" to every other
  sidebar, including instances whose own view still contains that repo. A sidebar in a window
  whose grouping is temporarily empty for one tick can flip the shared filter to All for
  everyone. This is the one place where a non-user action mutates shared view state.

The stale-reset itself is correct and desirable — the doc comment is explicit about wanting
it persisted so fresh instances do not reload a dead repo name. The problem is only the
placement: a persistence decision does not belong in a function called from render-side and
input-side paths.

Proposed change: keep the local reset in `rebuild_row_targets` (it must stay for the list to
be correct this frame) and move the tmux write to the paths that own shared state — the
reload branch in src/app.rs and the post-snapshot fold — so it executes at most once per
shared-option reload rather than once per rebuild. Same observable behavior, one owner, no
per-tick spawning.

## F-3. Every sidebar that observes a group-by change re-writes the cleared collapsed set

`s` (toggle group_by) is implemented as: changer writes the shared option, clears folds
locally and in tmux, broadcasts (src/app/input.rs:244-253 →
`global.toggle_group_by()` at src/state/global.rs:353-358, plus
`persist_cleared_collapsed_groups()` at src/state/global.rs:299-302). That is right.

Then every observer does it too. The reload branch (src/app.rs:269-316) compares
`previous_group_by` against the adopted value and, on a difference, calls
`persist_cleared_collapsed_groups()` and clears `collapsed_cursor_group` again:

    if state.global.group_by != previous_group_by {
        state.global.persist_cleared_collapsed_groups();
        state.collapsed_cursor_group = None;
    }

The comment above it (src/app.rs:283-288) explains the intent — an instance that missed the
broadcast would otherwise keep the previous mode's names in tmux and fold them back in on
the next sync — and the local clear is genuinely necessary. The write is not.
`persist_cleared_collapsed_groups` clears locally and then calls `save_collapsed_groups`,
which on success also stamps `last_saved_collapsed_groups` (src/state/global.rs:280-288), so
the echo guard will happily accept the same empty set again on the next sync and the write
does nothing for anyone.

Cost: with n sidebars open, one `s` press produces n tmux `set -g` spawns and n SIGUSR1
broadcasts (each save success in the toggles is followed by `broadcast_change()`,
src/state/global.rs:211-213; `save_collapsed_groups` has no broadcast, but every
`toggle_*` beside it does, and the extra writes lengthen the window in which an unrelated
change can be lost). The broadcasts re-enter the reload branch, which is the branch that
also re-persists.

Proposed change: split the function. `clear_collapsed_groups_local()` for observers
(clear `collapsed_groups`, clear `collapsed_cursor_group`, stamp `last_saved_collapsed_groups
= Some({})` so the echo guard matches the shared value without a write); keep the
persisting variant for the changer only. Behavior is unchanged for every scenario the
comment worries about, because an observer that missed the broadcast will still find the
shared option already cleared by the changer.

---

# Part 2 — Medium severity

## F-4. `save_collapsed_groups` is one-shot; cursor saves are queued

Two shared view fields, two different failure policies.

Cursor: every navigation path calls `queue_cursor_save()` (src/state/global.rs:236-238), the
event loop flushes after a 120 ms idle (src/app.rs:364-365 →
`flush_pending_cursor_save`, src/state/global.rs:242-258), and the flush deliberately keeps
the pending marker when the tmux write fails so the next tick retries — the comment at
src/state/global.rs:252-255 calls out exactly the bug that policy prevents ("a transient
failure would silently drop the queued save").

Collapsed groups: `save_collapsed_groups` (src/state/global.rs:280-288) fires one spawn from
inside `toggle_group_collapse` (src/state/layout.rs:549), `toggle_all_groups`
(src/state/layout.rs:494, 500) and the `z`/`Z` handlers. On failure it leaves
`last_saved_collapsed_groups` untouched on purpose (the doc comment at src/state/global.rs:276-279
says so: the untouched marker becomes the revert anchor), so the next shared-option sync
re-adopts the old set from tmux and the fold silently walks back. The user sees the header
expand or collapse for a tick and then revert, with no explanation.

Same class of asymmetry in the same file: `save_repo_filter` (src/state/global.rs:262-274)
is also one-shot, and it is the one reached from the render path (F-2).

Proposed change: give folds the cursor's queue — a `pending_collapsed_save` stamp plus a
flush beside `flush_pending_cursor_save`, retry-on-failure included. Keep the revert-anchor
semantics (only stamp `last_saved_*` on success) so the two policies coexist. If the added
state is not wanted, the minimum honest fix is a `set_flash` on a failed fold write, so the
revert is at least attributed to tmux rather than to the app.

## F-5. The search cursor contract is not implemented — no capture, no restore, no save

docs/state-management.md describes the cursor as the user's selection, preserved across
unfocused reloads (docs/state-management.md:30-34, :45) and reconciled on focus gain. Search is the one mode
where the cursor is repeatedly moved by the system rather than the user, and it has no
protection.

Trace:

- `open_search` (src/state/search.rs:50-58) sets `active = true`, claims
  `Focus::Panes`, clears the chyron. It captures nothing — there is no pre-search
  `selected_pane_row` anywhere in the state struct.
- Every keystroke in the input rebuilds the target list (`search_push_char` /
  `search_pop_char` / `search_pop_word` / `clear_search_query`, src/state/search.rs:76-115),
  and each rebuild that drops the cursor's pane lands on row 0 because `search.has_query()`
  short-circuits the within-group anchor (src/state/layout.rs:302-303).
- Esc while active → `cancel_search()` (src/state/search.rs:67-74): clears the query,
  rebuilds, and returns the list to unfiltered — with the cursor left on row 0 (or on the
  stale re-anchor result), not where the user was before typing `/`. No restore, because
  nothing was saved. No `queue_cursor_save()`, so the pre-search position is also lost from
  tmux if it was ever the shared value. No scroll touch — `compute_scroll_offset` only
  auto-scrolls while focused (src/ui/panes.rs:668), so a restored row could sit off-screen
  with no correction either way.
- Enter → `finish_search()` (src/state/search.rs:61-63) only sets `active = false`; the
  query stays applied, which is the documented intent.
- Navigation inside the active input (`Up`/`Down`/`Ctrl-n`/`Ctrl-p`,
  src/app/input.rs:159-170) calls `move_pane_selection` without `queue_cursor_save()` —
  every other cursor-moving binding in the same file queues one (src/app/input.rs:196-241,
  455-500). So j/k while searching moves the shared cursor conceptually but never writes it,
  and the row the user is about to Enter on is not the row any other sidebar would show.

There is a defensible reading of all this: search is transient, and `activate_selected_pane`
clears the query on jump (src/state/focus.rs:204-209) so the common exit is already
"restored." But the Esc path is the one that strands the user, and the missing save during
search-active navigation is a plain inconsistency with every sibling binding.

Proposed change: capture `selected_pane_row` (plus the pane id, so the restore can re-anchor
by identity rather than index) in `open_search` when the query is empty; restore it in
`cancel_search` before the rebuild, letting the normal re-anchor take it to the right row if
the pane is gone; add `queue_cursor_save()` to the search-active navigation arms. If the
product decision is instead "search owns the cursor, period," then the doc should say so and
`cancel_search` should at least park at row 0 deterministically rather than leaving whatever
the re-anchor produced.

## F-6. `rebuild_row_targets` runs twice per tick, and the `w` toggle does it a third time

`apply_session_snapshot` ends with `rebuild_row_targets()` (src/state/refresh.rs:183). When
the tick also carries a shared-option reload, the event loop calls it again
(src/app.rs:291), immediately after `apply_shared_options_without_cursor`. Nothing between
the two calls changes grouping inputs other than the adopted options themselves — which is
exactly why the second call exists — so in the common case the first pass's work (the whole
per-group filter + `ordered_group_rows` sort, src/state/layout.rs:209-274) is discarded.
That is a full list rebuild, twice per tick, on every tick that is signal-driven or on the
5 s floor.

The `w` toggle compounds it: src/app/input.rs:292-295 toggles `show_windows` and calls
`rebuild_row_targets()` inline for immediate feedback, and the fold in the next snapshot
rebuilds again. Same shape at src/app/input.rs:444-447 in the TODO-focus arm.

Proposed change: pick one owner. Either have the reload branch set a `targets_dirty` flag
consumed by the same code path that `apply_session_snapshot` uses (so the rebuild happens
once, after options are adopted), or make the reload branch skip its call when it is
executing immediately after a snapshot fold — which, in the current loop structure, is
always. Cheap either way; the flag is more honest about the invariant.

## F-7. `last_globals_sync` is reset unconditionally, including on a failed fetch

In the reload branch (src/app.rs:278):

    last_globals_sync = std::time::Instant::now();

This runs after the match that either folds the worker-fetched map or re-reads tmux
directly — but it runs regardless of whether either succeeded. The failure case is real and
named: `raw.globals_requested` with `raw.global_options == None` means the worker's
`show -g` failed, and the branch takes the `reload_shared_options_without_cursor()` path
(src/state/global.rs:437-440), which calls `tmux::get_all_global_options()` and returns
whatever it got, possibly an empty map. `apply_options` on an empty map adopts nothing
(every arm is `if let Some(...)`).

Consequence: the 5 s safety net (src/app.rs:36, `GLOBALS_RESYNC_INTERVAL`; the comment at
src/app.rs:103-109 explains that it exists because signals are edge-triggered and can be
lost) is restarted by the very event that proves the fetch is broken. A sidebar whose tmux
server is briefly unresponsive, or whose `show -g` fails under a hook storm, waits a full
5 s before trying again instead of retrying on the next tick. The same reset also applies to
the window-inactive reload trigger (src/app.rs:271: `is_window_active &&
window_inactive_count >= 2`), so a rapid hide→show→hide→show cycle can produce zero
effective reloads if the ticks land inside the restarted window.

Proposed change: reset `last_globals_sync` only when the map was actually applied (the
`Some(opts)` arm), and leave it untouched on the failed-fetch arm so the next tick retries
immediately. Alternatively, keep the unconditional reset and document why a failed sync
deserves the same silence as a successful one; I could not construct a reason.

## F-8. TODO panel shared visibility is adopted only in the reload branch and at startup

The TODO file is re-read every tick (`todos.refresh_if_changed(hide_completed)`,
src/app.rs:121-125 → src/state/todo.rs:137-157), so item changes converge in ≤1 s. Panel
visibility — also shared, via `@sidebar_todo_visible` — is not: `sync_todo_visibility()`
(src/state/todo.rs:763-778) is called from exactly two places, src/app/setup.rs:32 and the
reload branch at src/app.rs:296.

So when another sidebar opens or closes the panel, this instance adopts the change on the
next tick that happens to be signal-driven, or on the 5 s floor, or on the window-inactive
reload — never on a plain periodic tick. That is a latency band of up to 5 s during which two
sidebars disagree about whether the panel exists. The changing sidebar broadcasts (the
`set_todo_visible` → `save_todo_visible` → `broadcast_change` chain at
src/state/global.rs:398-406), so the normal path is fine; the gap is the lost-signal case
that the 5 s floor exists to cover, stretched here because the fold itself is gated.

Related, and worth deciding at the same time: `Focus::Todos` survives blur and refocus. The
reload branch cancels an active search when the sidebar is unfocused (src/app.rs:314-316)
but never releases TODO focus; only the close path in `sync_todo_visibility`
(src/state/todo.rs:770-776) does that, and only when the shared flag says the panel closed.
A sidebar that loses keyboard focus while the TODO panel owns the keyboard comes back
thinking `Focus::Todos` still rules, so j/k drive a list the user is not looking at.

Proposed change: call `sync_todo_visibility()` from the same unconditional place
`refresh_if_changed` already is (the `refresh_due` block), which makes it a per-tick file
stat comparison against a cached tmux value — no new spawning. And decide whether
`Focus::Todos` should be released when `sidebar_focused` goes false; if survival is
intentional (it has a certain vim-tab feel), say so in docs/state-management.md, since
right now it reads as an oversight next to the search cancel three lines above it.

## F-9. `s` and `w` neither park nor persist the cursor, and `s` defers its rebuild

`s` (group-by toggle, src/app/input.rs:244-253) does: `toggle_group_by()`, clear
`collapsed_cursor_group`, `persist_cleared_collapsed_groups()`, `snapshot_now = true`. It
does not touch `selected_pane_row`, does not call `rebuild_row_targets()` (by design — the
comment at src/app/input.rs:238-243 documents the audit 2026-11-16 J5 fix that moved the
blocking `refresh()` off the keystroke), and does not `queue_cursor_save()`.

`w` (show-windows toggle, src/app/input.rs:292-295) does: `toggle_show_windows()`,
`rebuild_row_targets()`. Also no cursor save, and no park.

Both are view-mode changes that reshape the entire list. Repo mode and session mode produce
different group names and different group counts (src/group.rs:83-95 vs
`group_panes_by_session`); `show_windows` on/off inserts or removes every window row in the
list (src/state/layout.rs:236-250 gates window rows on it). The cursor is a raw index into
that list. So after `s`, `selected_pane_row` is a number that meant something in a list that
no longer exists, and it will be clamped/re-anchored by the fold's rebuild one iteration
later. The pane-id re-anchor (src/state/layout.rs:292-300) usually rescues it, but only when
the pane survives into the new grouping — which `s` between repo and session mode does not
guarantee at all (a repo group's panes may land in a different session group, or none).

Compare the neighbors: `z` and `Z` do park the cursor's group and do `queue_cursor_save()`
(src/app/input.rs:222-241, src/state/layout.rs:531-561 and 476-512); a header click saves
(src/state/layout.rs:779). The two toggles that most completely reshape the list are the two
that persist nothing.

Two separate questions here:
- Persistence. After `s` or `w` re-anchors the cursor, the shared `@sidebar_cursor` still
  holds the pre-change index. Add `queue_cursor_save()` to both, matching every other
  cursor-moving path.
- Feedback. `s` shows the old grouping for one loop iteration (~150 ms best case given
  `MIN_REFRESH_GAP`, src/app.rs:26; up to 1 s if `snapshot_now` is consumed while a request
  is in flight, since `forced` stays latched, src/app.rs:112-118). The old inline `refresh()`
  was slow but instant. A non-blocking inline `rebuild_row_targets()` on `s` — grouping will
  be recomputed from the last known-good `repo_groups`, which is stale only by one tick —
  recovers most of the perceived immediacy without reintroducing the spawn. Or accept the
  lag and document it.

## F-10. The row-index contract is implemented twice

`pane_row_targets` is the cursor's coordinate space, and `line_to_row` is the render's map
from screen line to that space. They must agree exactly, on four independent rules, or clicks
and selection land on the wrong pane.

They are built by two independent implementations of the same query:

| Rule | `rebuild_row_targets` | `row_collector::collect` |
|---|---|---|
| repo filter | src/state/layout.rs:210 | src/ui/panes/row_collector.rs:76 |
| collapse skip | src/state/layout.rs:216 | src/ui/panes/row_collector.rs:114 + 297 |
| group-name-hit-keeps-group / per-row fields | src/state/layout.rs:222-228 | src/ui/panes/row_collector.rs:81-90 |
| window set + `group_window_key` | src/state/layout.rs:236-251 | src/ui/panes/row_collector.rs:97-112 |
| row order | src/state/layout.rs:252 | src/ui/panes/row_collector.rs:330 |

Both lower the query the same way (audit 2026-11-19 B6), both call
`crate::group::ordered_group_rows`, both OR `search_matches_group` with the per-row
predicate. The comments acknowledge the coupling in both directions ("This mirrors the
renderer's rule in `row_collector::collect`", src/state/layout.rs:221-222; "The same rule as
`rebuild_row_targets`", src/ui/panes/row_collector.rs:80-81). Tests pin the interleave
(src/group.rs:1534-1576) and the `line_to_row` mapping (src/ui/panes/row_collector.rs:618,
645, 703-714, 753, 824, 1071, 1165), which catches regressions in the shared helpers but not
in the divergence of the two call sites — a filter added to one side and not the other keeps
all of those tests green, because each test exercises one side.

The failure is silent and it is the worst kind: the cursor renders on one row and activates
another, or a click selects the pane above what the pointer is on, and nothing ever errors.

Proposed change: extract one ordering/filtering function that both paths consume — the
natural home is src/group.rs beside `ordered_group_rows`, returning the per-group
`(RowTarget, line_kind)` sequence that `collect` decorates and `rebuild_row_targets` records.
That is the structural fix, and it is what makes the cursor findings in this audit safe to
act on: today, a change to one side's filtering has to be reasoned about twice.

---

# Part 3 — Low severity

## F-11. Two stale lines in docs/state-management.md about the synchronous refresh

docs/state-management.md:67-68: "The synchronous `refresh()` runs only at startup and on an
explicit `s` regroup." docs/state-management.md:229-231 repeats it inside the data-flow
diagram.

`s` has not called `refresh()` since audit 2026-11-16 J5. The handler sets
`state.snapshot_now = true` and the event loop consumes it on the next iteration
(src/app/input.rs:251, src/app.rs:112-127). `refresh()` (src/state/refresh.rs:539) now has
exactly one caller, `setup::init_state` (src/app/setup.rs:40). The doc was rewritten on
2026-11-20 (5323ddb) and these two sentences survived it.

Proposed change: both lines become "runs only at startup; the `s` regroup requests a worker
snapshot via `snapshot_now` and folds it on the next tick."

## F-12. `confirm_repo_popup` changes the filter and never saves the cursor

`confirm_repo_popup` (src/state/popup.rs:241-255) sets `repo_filter`, closes the popup,
`save_repo_filter()`, `rebuild_row_targets()`. The rebuild can and routinely does move the
cursor: the list shrinks to one repo's rows, the previous pane usually is not in it, the
search is empty, so the cursor lands on the group anchor or the clamp
(src/state/layout.rs:292-330). `selected_pane_row` changes and nothing persists it, so every
other sidebar and every future startup still holds the pre-filter index into a list that no
longer exists in that shape.

The same omission exists in `toggle_repo_popup` on close (src/state/popup.rs:220-238) via
the same rebuild.

Proposed change: `queue_cursor_save()` in `confirm_repo_popup` after the rebuild. One line.

## F-13. The empty-list branch of the rebuild preserves a stale parked group

`rebuild_row_targets` has two exits and they disagree about `collapsed_cursor_group`:

- `len == 0` (src/state/layout.rs:276-285): reset `selected_pane_row = 0` and return,
  deliberately keeping the parked group — the comment says it is "the only way `z` can still
  resolve a fold." Correct for the collapse-everything case.
- `len > 0` (src/state/layout.rs:286): clear `collapsed_cursor_group` unconditionally,
  "the cursor ends up on a visible row from here on."

Sequence that breaks the invariant: `Z` folds everything (parks the cursor's group,
src/state/layout.rs:490-491), then the repo filter or a search empties the list — parked
survives. Then a pane appears or the filter narrows differently, the list is non-empty for
one tick and the parked group is cleared. Then the list empties again. `collapsed_cursor_group`
is now `None` while the entire list is folded away, and `z` is dead — the exact bug the park
was invented to prevent (the comment at src/state/layout.rs:533-538 names it).

The mirror case: park survives through a genuinely-empty list where the parked group name
came from a mode that no longer exists (see F-3's `collapsed_cursor_group` clear, which the
len==0 path also skips).

Proposed change: make the two exits agree on one rule — park survives only while
`group_starts` is empty, i.e. clear it in the len==0 branch too when
`self.layout.group_starts` is empty but some group was skipped by filter rather than by
collapse. That distinguishes "folded away" from "filtered away" without new state. Failing
that, document that `z` is inert after a filter-empties-then-refills cycle.

## F-14. Window-header clicks and keyboard navigation disagree about the cursor

Window headers (`index:name` under a group title, gated on
`@sidebar_session_windows` for session mode and `@sidebar_worktree_windows` for repo mode,
src/ui/panes/row_collector.rs:325-331) are not rows: they map to `None` in `line_to_row`, so
they are not selectable, and `jump_to_window` deliberately does not touch the cursor
(src/state/focus.rs:295-302, pinned by the test at src/state/focus.rs:572-584). Clicking one
switches the client and stamps `focus_written_at`; nothing else moves. Good, and consistent
with the keyboard, which has no window-header binding at all.

The disagreement is at the neighbors: a group-header click does `toggle_group_collapse` plus
`Focus::Panes` plus `queue_cursor_save()` (src/state/layout.rs:763-780); a window-header
click does `jump_to_window` plus `Focus::Panes` and no save (src/state/layout.rs:784-802).
Both are header clicks in the agents panel, and one of them can leave the sidebar's own
focus state and cursor in a different relationship to the shared value than the other — the
window click changes which window the client is looking at without any shared-state write,
so the next `sidebar_gained_focus` reconciliation in the *destination* window's sidebar is
the thing that fixes the cursor, in a different process.

This is defensible (the cursor belongs to the sidebar that renders it, and the destination
sidebar owns that), so the finding is Low and the proposal is documentation: state-management.md
should record that window headers are client-navigation, not selection, and that their clicks
intentionally persist nothing, next to the existing note that a focus event is not a user
choice and does not persist the cursor (docs/state-management.md:40-41).

---

# Part 4 — Verified consistent (no findings)

Recorded because each of these is a plausible-looking finding that the trace cleared. The
next reader should not spend time here.

| Claim | Evidence |
|---|---|
| The selection highlight never renders while the sidebar is unfocused | `is_selected` gated on `sidebar_focused && focus == Focus::Panes` for both row kinds: src/ui/panes/row_collector.rs:360-362 (agent), 440-442 (window) |
| Auto-scroll never fights a preserved cursor | `compute_scroll_offset` auto-scrolls only under the same gate (src/ui/panes.rs:668); it clamps a stale offset via `scroll(0)` (src/ui/panes.rs:664) but never parks at 0 on unfocus, matching the deliberate non-parking documented at src/app.rs:300-313 and docs/state-management.md:30-34 |
| The shared cursor is never adopted at runtime | `apply_shared_options_without_cursor` passes `adopt_cursor = false` (src/state/global.rs:536-538) and both reload arms use it (src/app.rs:274-277) |
| Echo guards actually guard | `selected_pane_row` (src/state/global.rs:541-548), `repo_filter` (549-555), `collapsed_groups` (608-620) each compare against their `last_saved_*` marker before adopting; the markers are stamped only on successful writes (218-232, 262-274, 280-288) |
| The cursor survives blur/refocus | The reload branch cancels a search but explicitly leaves cursor and scroll alone (src/app.rs:300-316); the focus-gain pass then reconciles (src/app.rs:318-330) and keeps them when the context pane is unlisted (src/state/scroll.rs:57-60) |
| A deliberate jump is not repudiated by a stale snapshot | `focus_written_at` is stamped after the tmux spawn in both `activate_selected_pane` (src/state/focus.rs:238) and `jump_to_window` (src/state/focus.rs:301); `apply_snapshot` (src/state/refresh.rs:598-610) and `find_focused_pane_from` (src/state/focus.rs:96-111) skip focus writes for snapshots fetched before it |
| The proxy focus cannot double-light next to its window row | Resolved once per snapshot (src/state/refresh.rs:186), cleared by jumps (src/state/focus.rs:217), applied by the renderer only when `show_windows` is false (src/ui/panes/row_collector.rs:43-46) — documented as invariant 9 |
| Group-by change genuinely resets folds, not just the shared option | Observer-side clear of both `collapsed_groups` (via persist) and `collapsed_cursor_group` (src/app.rs:287-290) — the write is the problem (F-3), the local clear is correct |
| Window-header levels are config-time-only and read on every sync | `session_windows` / `worktree_windows` adopted unconditionally from tmux (src/state/global.rs:579-596); no runtime toggle exists in src/app/input.rs; the header render picks the right level per mode (src/ui/panes/row_collector.rs:325-328) |
| Window rows are correctly excluded from selection | `line_to_row.push(None)` for group headers (121, 301), window headers (347) and the flash row (481); the `jump_to_window` no-cursor-move contract is pinned (src/state/focus.rs:572-584) |
| Attention consumption respects the sidebar's own focus state | `mark_focused_pane_seen` returns early while `sidebar_focused` and while the window is inactive (src/state/refresh.rs:449-472); `mark_active_window_panes_seen` pins to `TMUX_PANE` in popup mode (src/state/refresh.rs:253-259) |
| Self-close needs positive evidence | `window_has_only_own_pane` requires having seen its own pane and no other (src/state/refresh.rs:1115-1125), skipped in popup mode (src/state/refresh.rs:187-188), tested at 2402-2424 |
| Landing defaults cannot override live shared state | `apply_default_view` seeds only keys absent from the option map (src/state/global.rs:494-507); `compact` and friends are adopted unconditionally afterward (556-566) — and the startup order (load_from_tmux at src/app/setup.rs:21, then apply_default_view_from_tmux at :23) makes the live value win, as the comment at :558-562 claims |
| The TODO file converges on its own | `refresh_if_changed` runs every tick (src/app.rs:121-125 → src/state/todo.rs:137-157), re-anchors an open edit and clamps the selection |
| The 5 s globals floor exists and is not redundant with the window-inactive net | The comment at src/app.rs:103-109 explains that the inactive net cannot fire for a sidebar in an unviewed session; `#{window_active}` is per-session (src/tmux/query.rs:87, 151-152) |

---

# Part 5 — Proposed change set

Nothing below has been implemented. Ordered by risk-adjusted value, smallest first within
each tier. Each is independently shippable.

Tier 1 — cursor persistence parity (small, mechanical, no behavior redesign)

1. `queue_cursor_save()` in `confirm_repo_popup` (src/state/popup.rs:253) — F-12.
2. `queue_cursor_save()` on `s` (src/app/input.rs:251) and `w` (src/app/input.rs:294, 446)
   — F-9 persistence half.
3. `queue_cursor_save()` on the search-active navigation arms (src/app/input.rs:159-170)
   — F-5 third bullet.

Tier 2 — single-owner writes (behavior-preserving)

4. Move `save_repo_filter()` out of `rebuild_row_targets` (src/state/layout.rs:179) into the
   reload/snapshot paths — F-2.
5. `clear_collapsed_groups_local()` for observers; keep the persisting variant for the
   `s` changer only (src/app.rs:288) — F-3.
6. Reset `last_globals_sync` only on a successful option application
   (src/app.rs:278) — F-7.

Tier 3 — convergence timing (small, needs a tick-cost check)

7. Call `sync_todo_visibility()` from the `refresh_due` block beside
   `refresh_if_changed` (src/app.rs:121-125) instead of only the reload branch — F-8 first
   half.
8. De-duplicate `rebuild_row_targets` per tick: a `targets_dirty` flag owned by the fold,
   or have the reload branch skip its call (src/app.rs:291) — F-6.

Tier 4 — cursor semantics (needs a decision before code)

9. Queue-and-retry for collapsed-group saves, mirroring the cursor's
   `pending_cursor_save_since` pattern (src/state/global.rs:236-258) — F-4.
10. Pre-search cursor capture in `open_search` + restore in `cancel_search`
    (src/state/search.rs:50-74), plus a decision on whether search owns the cursor — F-5.
11. Reconcile the `len == 0` / `len > 0` disagreement about `collapsed_cursor_group`
    (src/state/layout.rs:276-286) — F-13.
12. Pane-mode startup `select_focused_context_row()` (src/app/setup.rs:46-49), or narrow the
    `apply_all` doc comment (src/state/global.rs:517-524) — F-1.
13. Inline `rebuild_row_targets()` on `s` for immediate regrouping feedback
    (src/app/input.rs:251), or document the one-tick lag — F-9 feedback half.

Tier 5 — structural

14. Single source for the row-index contract: one ordering/filtering function in
    src/group.rs consumed by both `rebuild_row_targets` and `row_collector::collect` — F-10.
    Do this before any Tier 4 change that touches filtering.

Tier 6 — documentation

15. docs/state-management.md:67-68 and :229-231 — the `refresh()` / `s` regroup claim —
    F-11.
16. docs/state-management.md — record that window-header clicks are client-navigation and
    persist nothing (src/state/layout.rs:784-802, src/state/focus.rs:295-302) — F-14.
17. docs/state-management.md — state the `Focus::Todos` blur/refocus policy explicitly,
    whichever way item 18 decides — F-8 second half.

## Appendix A — Verification notes

Findings whose existence rests on a negative (something not being there) were checked by
exhaustive grep of the state and input layers, not by sampling:

- No pre-search cursor capture: `selected_pane_row` appears in src/state/search.rs only via
  the rebuilds it triggers; `open_search` (src/state/search.rs:50-58) has no capture and the
  struct (`SearchState`, src/state/search.rs:10) has no field for one.
- No cursor save on `s` / `w` / search-nav / `confirm_repo_popup`: the `queue_cursor_save`
  call sites in src/app/input.rs are lines 197, 202, 207, 212, 217, 224, 233, 415, 465, 469,
  482, 495 and in src/state/layout.rs are 599, 779, 856 — none inside the `s` (244-257),
  `w` (292-295, 444-447), search-active (158-169), or popup-confirm paths.
- `refresh()` has exactly one caller: src/app/setup.rs:40; grep for `\.refresh()` in src/
  returns that plus tests.
- Pane mode never reconciles at startup: the only `select_focused_context_row` call sites
  are src/app/setup.rs:48 (inside `if state.popup_mode`) and src/app.rs:328 (inside
  `if sidebar_gained_focus`).
- Pane mode starts unfocused: src/cli/toggle.rs:158 captures `active_pane` and :210-215
  restores `select-pane -t` to it after the split; `apply_snapshot` copies the snapshot's
  `sidebar_pane_active` for non-popup mode (src/state/refresh.rs:602-608).
- The len==0 / len>0 park asymmetry: `collapsed_cursor_group` is assigned at
  src/state/layout.rs:286 (unconditional clear, after the len==0 early return at 283), 491,
  498, 538, and cleared at src/app.rs:289; declared at src/state.rs:119, defaulted at :192.
- Window-header levels have no runtime toggle: src/app/input.rs has no binding writing
  `session_windows` or `worktree_windows`; both are adopted only in `apply_options`
  (src/state/global.rs:579-596).

## Appendix B — Method notes and non-findings excluded

- The two implementations compared in F-10 are genuinely separate functions, not one called
  from the other: `rebuild_row_targets` (src/state/layout.rs:166) builds
  `pane_row_targets` + `group_starts`; `collect` (src/ui/panes/row_collector.rs:29) builds
  `lines` + `line_to_row`. Neither calls the other. The duplication is the filtering and
  ordering preamble, not the decoration.
- The double rebuild in F-6 was confirmed to be reachable in the common case rather than
  theoretical: `apply_shared_options_without_cursor` runs in the same block, immediately
  before its own `rebuild_row_targets()` call, and the snapshot fold ran first in the same
  iteration (src/app.rs:189-195 → src/state/refresh.rs:183 → src/app.rs:291).
- Considered and excluded as a finding: the unconditional adoption of `compact`,
  `show_windows`, `group_by`, `todo_*`, `session_windows`, `worktree_windows`,
  `worktree_titles`, `preview_lines` (no echo guard) while cursor / repo filter /
  collapsed groups are echo-guarded. This is deliberate and documented twice
  (src/state/global.rs:558-562, docs/state-management.md:59): the unguarded ones are
  "everyone renders identically" settings where last-writer-wins is the goal; the guarded
  ones are per-instance navigation state. Correct as designed.
- Considered and excluded: `toggle_selected_flag` (src/state/focus.rs:313-350) not queuing a
  cursor save. It does not move the cursor, so there is nothing to persist.
- Considered and excluded: the self-close path (src/app.rs:215-217 +
  src/state/refresh.rs:187-188) racing a cursor save. `quit_requested` is checked at the top
  of the loop and `flush_pending_cursor_save` runs at the bottom, so a save queued on the
  tick that observes the close is flushed before teardown on the next iteration — and a lost
  cursor write on a window that no longer exists is self-consistent anyway.
