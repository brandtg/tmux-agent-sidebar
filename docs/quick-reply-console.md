# Quick-Reply Console (Bottom Panel Redesign)

Status: approved direction, not yet implemented. This document is the
reference for the build. It records the workflow findings that motivated
the redesign, the agreed behavior, and the implementation plan.

## Background

The bottom panel has gone through two iterations: per-pane Activity / Git
tabs, then the cross-agent Feed / Repos tabs. Both failed for the same
reason — they are second lists. Repos duplicates what the main agent list
already shows (repo grouping, branches); Feed is a passive, noise-filtered
log whose entries lose pane identity and offer no way to act.

The redesign is grounded in how the sidebar is actually used:

- Context switching between agent panes is the primary pain. The panel
  should contain information and actions that remove the need to jump.
- "Is this agent moving?" is already solved by the main list (status
  colors, spinners, elapsed timers). The panel must not duplicate that.
- Agent supervision works like an email inbox: some agents get an instant
  reply, others are deferred until the user has thought. The panel should
  be the reply surface for the instant tier, not a status display.
- Reading agent prose in bulk ("inbox of transcripts") was rejected as
  too much information; per-agent messages belong in the agent rows,
  which already render the latest prompt/response text.
- A live output tail was rejected (the sidebar tail is never a
  substitute for the full pane). A dev-server/port dashboard was
  reviewed and set aside — reasonable, but not the thing.

## Concept

The bottom panel becomes a quick-reply console. It is empty most of the
time and appears when there is something to answer: an agent waiting on a
question or permission, or one that just finished (the reply to "Done" is
the next prompt). The user triages the queue without ever switching
panes; jumping remains one keypress away for work that needs the TUI.

```
╭ Reply · 2 waiting ═══════════════════════════════╮
│ claude · brandtg/api · waiting 4m                │
│ ❯ Can I add a migration for the users table?     │
│ → █                                              │
╰──────────────────────────────────────────────────╯
```

- Line 1 identifies the entry: agent label, repo, how long it has been
  waiting, and the tier.
- The quoted line is the question / pending request (wrapped, 2-3 rows).
- The `→` line is the input. Enter sends the text to the pane and
  advances to the next entry. The queue drains like an inbox.

When the queue is empty the panel auto-hides and the agent list takes
every row. The layout breathes with the queue instead of reserving a
fixed 20 rows for two rarely-used tabs.

## Queue membership

An entry exists only while the pane's state is hook-confirmed — the
`@pane_wait_reason` / `@pane_attention` options are written by the agents'
own hooks and are authoritative. States that cannot be answered with text
or a key do not queue; the jump is the only sensible action for them, and
the main list already flags them.

| State | Wait reason | Tier | Respondable via |
|-------|-------------|------|-----------------|
| Waiting on permission | `permission`, `permission_prompt` | pending action | quick approve/deny keys, or text |
| Waiting on a question | `question` | pending action | text reply |
| Turn finished | `@pane_attention=done`, status `idle` | done | text reply (the next prompt) |
| Turn failed | `@pane_attention=notification` + `@pane_status=error`, wait reason holds the error text | done | text reply (correction / retry) |
| Rate limit, teammate idle, session resumed, idle prompt | `rate_limit`, `teammate_idle:*`, `session_resumed*`, `idle_prompt` | excluded | nothing useful to send — jump instead |

Helpers that already classify these: `is_permission_wait_reason` and
`is_pending_action_wait_reason` in src/cli/hook/handlers/status_priority.rs.

Sort order: pending-action tier first, then done tier; within a tier,
longest-waiting first (FIFO drain). The done tier's age key is
`attention_since` (observation time — a sidebar started mid-wait
undercounts, the same contract PaneRuntimeState documents).

## Entry text

What the quoted line shows, by tier, in priority order:

- Question: the AskUserQuestion text. Claude's adapter already extracts
  question text into activity-log labels (src/cli/label.rs), so it can be
  recovered from the pane's activity log. Fallback: the latest
  `@pane_prompt`.
- Permission: the tool/request being permitted — the pending tool's
  activity-log label (e.g. `Bash cargo test`), falling back to the
  Antigravity capture-scan prompt text when present, then
  `@pane_prompt`.
- Done: the response text — Stop already writes it to `@pane_prompt`
  with `@pane_prompt_source=response`.
- Error: the failure text, which StopFailure writes into
  `@pane_wait_reason`.

Long text wraps to a maximum of 3 rows and truncates with an ellipsis;
the full text is available by jumping. The agent rows continue to carry
per-agent prose — the panel never tries to be a transcript.

## Interactions

Panel focus reuses the existing `Focus::BottomPanel` state and the j/k
focus walk (moving past the list edge enters the panel, as today).

| Key | Behavior |
|-----|----------|
| `j` / `k`, `h` / `l`, `Shift+Tab` | Previous / next queue entry — moving is the only "skip"; a skipped entry stays queued |
| printable chars | Type into the input line (only while the panel holds focus) |
| `Enter` | Send the input to the entry's pane, advance to the next entry |
| `a` / `d` | On a permission entry: approve / deny via per-adapter keymap, advance |
| `o` (or click on the entry) | Jump to the pane — the escape hatch for real work |
| `Backspace` | Edit the input |
| `Esc` | Clear a non-empty input; return focus to Panes when empty |
| `m` | Pin the panel closed / unpin (unchanged semantics) |

Sending is a no-op when the input is empty (except permission entries,
where `a`/`d` need no text). After the last entry is handled the panel
auto-hides and focus returns to Panes.

### Send mechanics

- Text: reuse the `send_command` pattern from src/tmux/commands.rs —
  `send-keys -l <text>` followed by a separate `Enter`. The text is
  single-line in v1; newlines are stripped on input.
- Permission keys: a new helper sends bare key names (no `-l`), e.g.
  `send_key(target, "y")`. The approve/deny key names per agent live in
  adapter/ next to each agent's other integration points — each TUI's
  permission dialog differs, and the mapping must be verified against
  the live TUI during implementation rather than guessed.
- Freshness check: immediately before sending, re-read the pane's
  `@pane_wait_reason` (and status) and abort with an inline notice if the
  state no longer matches the entry — the agent may have resolved the
  wait elsewhere in the seconds since the frame was drawn. This is a
  check-then-act race that cannot be fully closed; the jump is the
  fallback, and the notice follows the SpawnInput inline-error pattern.
- A failed send renders the error inline (same pattern) and keeps the
  entry so the text is not lost.

## Auto-hide and sizing

- The panel height becomes dynamic: 0 rows when the queue is empty and
  the panel is not pinned open; otherwise the minimum that fits the
  current entry (chrome + identity row + up to 3 wrapped text rows +
  input row), clamped to a configurable maximum.
- `@sidebar_bottom_height` (default 20) is reinterpreted as that maximum
  rather than a fixed height; `0` still forces the panel permanently
  hidden. `effective_bottom_height` in src/state/tab.rs and the
  auto-minimize logic in `apply_auto_minimize` are updated accordingly;
  the pet divider height follows the panel as it does today.
- While pinned closed (`m`) with a non-empty queue, the panel renders
  the single header row with a `·N waiting` count — the Feed badge's
  role moves here. When the queue is empty and unpinned, nothing
  renders (not even the header).
- Popup mode continues to force the panel off (src/app/setup.rs).

## State model

New local-scope module src/state/reply.rs (replacing src/state/feed.rs):

```
struct ReplyQueue {
    entries: Vec<QueueEntry>,   // rebuilt every refresh tick
    selected: usize,            // index into entries
    input: String,              // current entry's draft
    drafts: HashMap<String, String>, // pane_id -> preserved text on skip
}

struct QueueEntry {
    pane_id: String,
    agent: AgentType,
    repo: Option<String>,
    tier: QueueTier,            // PendingAction | Done
    text: String,               // resolved per "Entry text"
    since: u64,                 // epoch for the age display
}
```

- The queue is derived, not persisted: each 1s refresh rebuilds entries
  from the existing snapshot (repo_groups + PaneInfo + PaneRuntimeState).
  No new tmux options, no new hook writes.
- `selected` and `drafts` are preserved across rebuilds by pane_id so a
  half-written reply survives a refresh, a skip, and the entry sliding
  through the sort order.
- `FeedState`, its unseen-badge logic, and the `bottom_tab` /
  `BottomTab` machinery are deleted; scroll state for the panel goes
  with them (entries navigate, they do not scroll).

## Removal of Feed / Repos

- src/ui/bottom/repos.rs is deleted outright — the main list already
  groups by repo and shows branches.
- src/ui/bottom/feed.rs and src/state/feed.rs are deleted. The activity
  logs stay: they feed task progress, tool labels, and the entry-text
  resolution above.
- src/ui/bottom.rs keeps only the chrome it still needs (the bordered
  box, focus coloring, header-with-count while pinned). Tab click
  targets become entry click targets.
- Help rows (src/ui/help.rs) change: h/l and Shift+Tab stop being
  "switch bottom panel tab" and become entry-navigation bindings; j/k
  in-panel behavior is documented; `a`/`d`/`o` gain rows. Tab keeps
  cycling the status filter — it is a global binding today
  (src/app/input.rs) and is not repurposed.
- README's bottom-panel section and the keybinding doc are updated in
  the same commit.

## Implementation phases

1. Read-only queue: state/reply.rs membership + sorting + text
   resolution; render the panel from it; auto-hide sizing. No input.
   Validate membership and noise level against real sessions.
2. Reply loop: input line, Enter-to-send via send-keys, advance,
   drafts, inline errors, jump key.
3. Permission quick actions: bare-key send helper, per-adapter keymaps
   verified against each agent TUI (claude, codex, opencode, hermes,
   antigravity).
4. Cleanup: delete Feed/Repos and the tab machinery, update help +
   README + docs/state-management.md, remove dead tests.

Each phase ships behind the panel being present; phase 1 is already an
improvement over the static tabs because the panel disappears when
there is nothing to answer.

## Testing

- Queue rebuild: membership per wait-reason table, tier sort order,
  age-key behavior across restarts (observation-time contract), draft
  preservation by pane_id.
- Rendering: inline snapshots via TestBackend for the entry anatomy,
  wrapping/truncation, pinned header with count, and the empty (hidden)
  case — following the UI test rule (insta snapshots, never contains).
- Sending: mock the send-keys calls the way tmux::test_mock mocks pane
  options; assert literal-text sends, bare-key sends, the freshness
  abort, and the advance behavior. No test may send to a live pane.

## Open questions

- Should a hotkey start a reply from the agent list without first
  focusing the panel (e.g. `R` replies to the selected row's pane)?
  Convenience vs. accidental sends — defer to phase 2 experience.
- Should StopFailure error entries re-queue after a reply lands (the
  reply itself is a new prompt that clears the state via hooks — likely
  no extra work needed).
- Input history recall (up-arrow) — nice-to-have, not planned.

## Implementation touch points

Inventory of the code this build touches, verified 2026-10-22. Line
numbers are from that read and may drift; trust symbol names over
offsets.

New files:

- src/state/reply.rs — queue model, membership, sorting, entry-text
  resolution, draft preservation. TDD against the membership and sort
  tables above.
- src/ui/bottom/reply.rs — panel content rendering (identity row,
  wrapped text, input line, pinned header with count).

Deleted files:

- src/ui/bottom/feed.rs, src/ui/bottom/repos.rs, src/state/feed.rs,
  and the BottomTab machinery in src/state (enum, bottom_tab field,
  next_bottom_tab, handle_bottom_tab_click, scroll_bottom,
  auto_switch_tab).

Modified:

- src/ui/bottom.rs — keeps the bordered chrome and focus coloring;
  build_tab_title / TabClickCols become entry click targets stored on
  state.layout (same recompute-every-frame pattern).
- src/ui/mod.rs — BOTTOM_PANEL_HEIGHT (L19) becomes a maximum, not a
  fixed height; the draw split (L67-99) consumes a dynamic height;
  PET_SCENE_HEIGHT (L5) divider tracking follows.
- src/state/tab.rs — effective_bottom_height (L85) returns 0 when the
  queue is empty and unpinned, otherwise min(needed, max);
  apply_auto_minimize (L101) and the `m` pin (L115) keep their
  semantics; the minimized 1-row header shows the queue count.
- src/state.rs — drop feed/bottom_tab fields, add the reply queue.
- src/app/input.rs — h/l under BottomPanel (L148, L157) and BackTab
  (L203-205) are freed for skip; character input routes to the reply
  draft when Focus::BottomPanel; `m` keeps its height>0 gate (L188);
  Enter is currently a no-op under BottomPanel (L193) — free for send;
  mouse click targets move from tab labels to entries, and the wheel
  scroll split at effective_bottom_height keeps working as-is.
- src/state/layout.rs — bottom_tab_click_cols replaced by entry rows.
- src/app/setup.rs — bottom_panel_height_from_tmux (L17) reads the
  maximum; popup mode still forces the panel off (L26).
- src/state/refresh.rs — refresh_feed (L798) becomes the queue rebuild
  on the same 1s cadence.
- src/ui/help.rs — HELP_ROWS per the removal section.
- src/tmux/commands.rs — add a bare-key sender next to send_command
  (L177); extend the test mock to record send-keys calls.

Reuse, do not reinvent:

- Text-entry modal pattern: PopupState::SpawnInput (src/state/popup.rs
  L50) — input String, inline `error` row, `working` flag; rendering in
  src/ui/panes/popups.rs.
- Jump: activate_selected_pane (src/state/focus.rs L94).
- Wait-state classification: is_permission_wait_reason and
  is_pending_action_wait_reason (src/cli/hook/handlers/
  status_priority.rs L17, L34).
- Question text: AskUserQuestion extraction in src/cli/label.rs (L191)
  lands in activity-log labels; read via
  crate::activity::read_activity_log (epoch|tool|label, newest-first,
  200-line cap). Cache per pane by log mtime, the way
  task_progress_log_mtime does (src/state/pane_runtime.rs).
- Antigravity approval-prompt text: the capture scan in
  src/app/workers.rs (detect_prompt_reason, L247/L250).
- Option constants: SIDEBAR_BOTTOM_HEIGHT /
  SIDEBAR_BOTTOM_MINIMIZED (src/tmux/options.rs L156-157).
- Age source: attention_since and its observation-time contract
  (src/state/pane_runtime.rs L37-44).

## Decisions log

Recorded from the brainstorm session so implementation does not re-litigate:

1. The panel is a content/action surface, not a second status list.
   "Is it moving" is already answered by the main list (status colors,
   spinners, elapsed timers).
2. Queue membership: waiting-on-you (permission, question) plus
   done/error. Excluded: rate_limit, teammate_idle, session_resumed,
   idle_prompt — nothing useful to send.
3. Sort: pending-action tier first, then done tier; FIFO within tier.
4. Entry text is minimal: identity, the question, an input line. No
   transcripts or threads; per-agent prose stays in the agent rows
   (which already render @pane_prompt).
5. Quick replies only from the sidebar; substantial work always gets
   the pane. The user rejected an in-sidebar output tail outright.
6. Permission asks are handled from the panel (quick approve/deny).
7. Empty queue auto-hides the panel entirely — no persistent header or
   all-clear line; the agent list takes the rows. This also resolves
   the "20 rows for two unused tabs" problem.
8. After a send: stay in the panel and advance to the next entry.
9. A bulk prose inbox ("read their summaries in the sidebar") was
   rejected as too much information; a dev-server/port dashboard was
   reviewed and set aside as "fine but not it"; a per-agent activity
   sparkline was rejected because the green circle already conveys
   motion.
10. Feed dies entirely — its unseen-attention badge role moves to the
    pinned header count. Repos dies as redundant with the main list.
11. Input is single-line in v1; multiline is a v2 concern.

## Parallelization plan

Read this document plus docs/state-management.md first; the AGENTS.md
rules (fmt before commits, scoped tmux servers, UI snapshot rule)
apply to every track.

- Track A — core (critical path, one agent, serial): state/reply.rs
  model with unit tests, then ui/bottom/reply.rs rendering with
  snapshots and the dynamic sizing in state/tab.rs + src/ui/mod.rs,
  then input wiring in src/app/input.rs. Delivers phases 1-2 and
  consumes Track B's helpers in phase 2 and Track C's tables in
  phase 3.
- Track B — tmux plumbing (independent, small): the literal-text and
  bare-key send helpers in src/tmux/commands.rs plus test-mock
  support. No file overlap with A; launch immediately in a worktree.
- Track C — adapter keymaps (independent, research-heavy): the
  per-agent approve/deny key tables in adapter/, with a verification
  checklist per agent TUI. Zero overlap with A. The live-TUI
  verification is a manual step — the agent prepares the checklist and
  a scoped reproduction; the human confirms the keys.
- Track D — cleanup (strictly after A): delete Feed/Repos and the tab
  machinery, rewire help.rs, README, docs/state-management.md, remove
  dead tests. Touches the same files as A, so it must not run
  concurrently.

Orchestrator shape: start B and C in worktrees on background lanes;
run A in the foreground lane; hand D to a follow-up task once A's
snapshots are accepted. The only shared-file risk is A vs D — enforced
by ordering, not by merging.

Manual gates that cannot be delegated: Track C's key verification
against each agent TUI, and a final smoke test in a scoped tmux server
(membership appearing/disappearing on real hook events, send landing
in a test pane, auto-hide breathing). Plan for the manual-smoke-test
skill at the end of phase 3.

## Environment notes

- cargo fmt before every commit; cargo test and cargo clippy before
  declaring any phase done.
- UI rendering tests must be inline insta snapshots via TestBackend —
  never contains-style assertions. cargo insta accept regenerates when
  a change is intentional.
- Any tmux reproduction runs on a scoped server (tmux -L ...); bare
  tmux commands against the live default-socket server are forbidden.
  Send-keys testing likewise happens only against scoped reproductions
  — never the user's real agent panes.
- Deploying to the live plugin (~/.tmux/plugins/tmux-agent-sidebar) is
  manual and user-owned: cargo build --release, copy the binary to
  bin/, toggle the sidebar. This redesign changes no hook
  registrations, so agent-sidebar.conf needs no update — the
  @sidebar_bottom_height semantic change is interpreted by the binary,
  not the conf.
