# Correctness & Performance Audit — 2026-11-25

Scope: whole codebase, focused on subtle correctness bugs and performance issues, plus
feature opportunities. This is the third audit pass; the 2026-11-20 docs audit and the
2026-11-21 state audit are both fully implemented, so their findings are excluded here.

Ground truth: main @ 1852914 (v1.5.3), clean tree.

Status: COMPLETE — all lanes recorded. See Summary for priorities.
Addendum 2026-11-26: OpenCode 2.1.4 → 2.4.2 upgrade check recorded under
Lane 0 — no breaking changes for the bridge.

## Findings

### Lane 0 — OpenCode bridge, entry script, conf (audited directly)

- **H-bridge-1. Pane shows idle while background subagents are running
  (user-reported 2026-11-25, confirmed).** When the main turn ends while
  background subagent child sessions still execute, the pane flips to idle and
  stays there: (1) the child's `session.execution.started` is skipped by the
  `!isMainSession` gate (tmux-agent-sidebar.js:895-901), (2) the child's tool
  results are dropped by `turnActiveFor` (:755-762 — `activeTurns.has(parent)`
  is false after the main stop), and (3) the child's terminal events are
  skipped (:903-911). Reproduced live: two background subagents running, pane
  idle. Fix sketch: track an `activeChildTurns` set — add on child
  `session.execution.started`, forwarding an activity event so the pane flips
  running (the activity handler stamps the run clock when missing); remove on
  the child's terminal event, forwarding `stop` only when neither the main
  turn nor any other child turn remains for the pane (otherwise a mid-turn
  child finishing would stop the clock under a running main turn); extend
  `turnActiveFor` to include the child's own entry so its tool results and
  permission asks keep flowing between main turns.

- **M-bridge-1. Hook chain stalls forever on a hung RPC.** The serialized
  `hookChain` link awaits `routeSessionId` → `lookupSessionInfo` →
  `ctx.session.get` (and `stampExplainedByOwnBinding` → `resolveSessionTitle`)
  with no timeout — only the final spawn is raced against 3 s
  (.opencode/plugins/tmux-agent-sidebar.js:482, 494, 526-544). One hung RPC
  wedges every subsequent opencode hook event for the daemon's lifetime.
  Fix: race the link body (not just the spawn) against a timeout.
- **M-bridge-2. `listPanes` listens for `exit`, not `close`.**
  tmux-agent-sidebar.js:153 resolves with the accumulated stdout on `exit`;
  `exit` can fire before the pipe's buffered data is delivered, truncating the
  pane table under load and sending resolution down fallback paths. Use
  `close` (as with `spawnSync` semantics) or accumulate until both exit and
  end-of-stream.
- **M-bridge-3. Timed-out hook spawns are not killed.** The 3 s
  `Promise.race` (tmux-agent-sidebar.js:543) unblocks the chain but leaves the
  wedged child alive; its late tmux writes then race the next link's writes,
  breaking the serialized-ordering invariant the comment at :115-123 and
  :500-503 promises. Kill the child when the timeout wins.
- **L-bridge-1. `sessionProjects`/`mainSessions` grow unboundedly.** The bus is
  server-global, so `session.created` for every session on the daemon (other
  projects included) inserts into `sessionProjects` (:800) and nothing is ever
  evicted; a long-lived daemon accumulates every session ever created. Bounded
  impact (small records) but unbounded growth.
- **L-bridge-2. Stale `activeTurns` entry survives a missed stop.** If the
  terminal `session.execution.*` event is missed while the daemon lives, the
  session stays in `activeTurns` forever and `turnActiveFor` keeps forwarding
  child tool activity between turns — the exact "timer ticking forever" shape
  the execute.after gate (:755-762) exists to prevent. No staleness bound
  exists. (Daemon restart clears it; plugin reload does not.)
- **L-bridge-3. Unhandled abort in the event pump.** The `for await` over
  `ctx.event.subscribe({ signal })` (:775) is wrapped in `void (async ...)`
  with no catch — if the iterator throws on abort (implementation-dependent),
  it becomes an unhandled rejection at unload.
- **L-conf-1. Paths containing double quotes break hook quoting.** The
  toggle/auto-create hooks interpolate `#{pane_current_path}` inside a
  `run-shell "..."` span (agent-sidebar.conf:183, 232-236): spaces are safe,
  but a `"` in the path terminates the quoted word and corrupts the command.
  Paths with quotes are rare; noting for completeness.
- Clean: entry script version gate, agy bundle generation, bridge symlink
  healing, hook dedup awk patterns, maintain-width version gates, hook.sh
  fallback chain — all verified correct.

#### OpenCode 2.1.4 → 2.4.2 upgrade check (2026-11-26, audited directly)

The daemon was upgraded past the bridge's tested 2.1.x baseline
(@opencode/cli@2.4.2 via npm; daemon restarted 2026-11-26 08:51). No breaking
changes to the bridge's consumed surface — no new findings. Verified by
diffing v2.1.4..v2.4.2 in the opencode checkout (packages/plugin, client,
schema, protocol, server, core) plus the changelog:

- Plugin module shape (`{ id, setup }` default export returning an unload fn)
  and `.opencode/plugins/*.js` discovery (files, directories, symlinks) are
  unchanged — the same loader code ships in both tags
  (packages/core/src/plugin/module.ts, plugin/source-directory.ts,
  config/plugin/source.ts).
- ctx.location.{directory, project.id}, ctx.session.get (endpoint and
  PublicSessionInfo unchanged: directory flat or under location, projectID,
  parentID, title), and ctx.session.hook("prompt") (prompt.text) are
  unchanged.
- ctx.tool.hook("execute.after") is additive only: info and codemode added in
  2.4.0; tool/sessionID/input/status/result intact, status still
  "completed" | "error".
- ctx.event.subscribe({ signal }) is unchanged (options stay types?, signal?,
  onActivity?) and the plugin bus is still server-global with the same
  isPublicEvent filter.
- Every consumed bus event type and payload field is intact at the schema
  level: session.created (flat data incl. location.directory; the data.info
  branch still covers 2.0.x daemons), session.renamed / session.updated
  (v1-legacy only), session.status (status.type), session.execution.started /
  succeeded / failed / interrupted (error.message / error.type), session.idle,
  permission.asked / replied (flat sessionID), form.created / replied /
  cancelled (form.sessionID, metadata.kind), question.* (absent at both tags —
  2.0.x daemons only). Only additions: new event types
  (session.step.first-token, experimental.mcp.event, job.*, push.*,
  artifact.*) and an unused source literal.
- Changelog breaking changes in range (@opencode/ai root now the Promise API,
  ImageResponse.image removed, provider renames, SDK endpoints no longer wait
  5 s for plugins, schedule → automation endpoints) touch nothing the bridge
  imports — it imports no opencode package at all.
- Still true: the npm package ships the Bun binary as bin/opencode.exe on
  every platform, so resolvePaneId's .exe handling stays necessary. Comment
  nit: MCP elicitation forms carry metadata.kind "mcp-elicitation" (both
  2.1.4 and 2.4.2), not "mcp" as the bridge header comment says — mapping is
  unaffected (anything non-"question" maps to elicitation_dialog).
- Live confirmation: this session's activity-log entries landed after the
  2.4.2 daemon restart, so the prompt hook and execute.after path deliver
  end-to-end on the new version.

New primitives in range that upgrade existing findings:

- ctx.session.hook("experimental.idle") (v2.2.1) fires when a main session
  finishes model work, carrying background: SessionIdleBackground[] with
  { kind: "subagent", childSessionID, ... } entries for still-running
  children, and re-fires as each child drains. Cleaner primitive for
  H-bridge-1 than inferring child-turn state from child session.execution.*
  events: hold the stop while a subagent background entry exists, forward
  stop when a later idle fires without one. Caveats: experimental (guard it
  like the legacy question.* path), it does not fire after interruption or
  failure (terminal-event handling stays), and admitting input from it would
  continue execution — observe only, admit nothing.
- ctx.event.subscribe({ types: [...] }) (v2.2.0) filters at the bus; recorded
  under feature opportunities.
- execute.after codemode (2.4.0) could label Code Mode tool calls in the
  activity log (minor).

### Lane 3 — TUI event loop + state layer (batch 2)

- **H-state-1. Startup git-info priming can freeze the sidebar for tens of
  seconds before the first frame.** `init_state` → `refresh()` runs
  synchronously before the first frame; the first snapshot primes the
  anchor→git-info cache inline, one `resolve_pane_git_info` per distinct pane
  cwd, each costing three serial git spawns (`rev-parse`,
  `status --porcelain`, `rev-list @{upstream}`) at 5 s timeout each
  (src/state/refresh.rs:136-162, src/group.rs:201-288, src/git.rs:9). The
  codebase elsewhere treats "git under a hung repo can spend minutes" as a
  reason to move work to worker threads, but the priming violates that on the
  render thread: one stale NFS/autofs cwd among pane directories ≈ 15 s
  white-screen at every sidebar open until the mount heals; several hung
  paths = minutes with zero feedback. Fix: prime behind the existing git-info
  worker (send anchors as the first request, render `PaneGitInfo::default()`
  for one tick) or gate the first frame on a short deadline.
- **M-state-1. PopupOpResult Spawn/Remove carry no operation identity — a
  late result closes or clobbers an unrelated reopened popup.** `confirm
  spawn A → Esc while working (explicitly allowed) → reopen spawn modal B →
  A's worker finishes → the Ok arm matches any open SpawnInput and sets
  `popup = None`, silently closing B; the Err arms inject A's error into B's
  dialog. Same for Remove against a different pane's confirm. Ops are git
  spawns that can legitimately run seconds-minutes
  (src/state/popup.rs:642-685, 590-618; src/app/input.rs:62-70;
  RemoveWarnings already matches on pane_id — Spawn/Remove match on
  nothing). Fix: stamp each result with an op id captured at
  `start_popup_op` and drop results whose popup was closed or replaced.
- **M-state-2. `r` (resume todo) has no in-flight guard — repeated presses
  launch duplicate agent windows.** Every press dispatches a fresh background
  op (window burst + `claude --resume …`); no latch, no debounce
  (src/state/todo.rs:904-956, src/state/popup.rs:625-635). The spawn/remove
  paths got a double-submit guard (audit B1); the resume path, added later,
  did not. Fix: latch `resume_in_flight` cleared in `flash_resume_result`.
- **M-state-3. Git-info worker round-trips the full cache every tick.** Every
  snapshot apply (≥1/s) rebuilds and sends the full anchor set; the worker
  clones the entire cache into a new map and sends it back; the loop
  overwrites the cache wholesale (src/app/workers.rs:214-221,
  src/app.rs:323-341, 416-419) — 2N string clones + a map rebuild per second
  even when nothing is stale, and a serial worker + hung repo means stale
  replies keep overwriting the cache. Fix: return only refreshed entries and
  merge; keep at most one request outstanding.
- **M-state-4. Approval-scan heal probe spawns one tmux read per running agy
  pane every 2 s for data the 1 s snapshot already has.** The `(None, false)`
  arm spawns `show -v @pane_status` just to decide whether to heal
  (src/app/workers.rs:300-312); the queueing parse already knew each pane's
  status. Fix: queue `Vec<(pane_id, was_waiting)>` and heal only
  `was_waiting` targets.
- **L-state-1.** Quit drops shared-option writes queued in the last 120 ms:
  the flushes run after the quit check (src/app.rs:174-185 vs 431-440), so a
  fold/cursor move <120 ms before `q` never reaches tmux and other sidebars
  revert. Run the flushes once before `break Ok(())`.
- **L-state-2.** Group-by-change observer re-anchors the cursor but never
  queues the cursor save (src/app.rs:261-268 vs the changer path
  src/app/input.rs:257-271) — the shared cursor keeps an index into the old
  list shape until the user navigates. Queue the save when the rebuild moved
  the row.
- **L-state-3.** Click row mapping hardcodes the list starting at screen row 1
  (src/state/layout.rs:875, `row.saturating_sub(1)`) — correct only because
  the panel always occupies the whole terminal; if it ever gains a top offset,
  every click lands off by that amount. Use `toolbar_row + 1`.
- **L-state-4.** Chyron display window stretches before the first frame:
  `header_width.unwrap_or(0)` makes every banner overflow, adding `text_w`
  scroll frames to the first banner after launch (src/state/chyron.rs:235-244).
  Fall back to a sane default width when None.
- **L-state-5.** `confirm_repo_popup` writes and broadcasts even when the
  selection matches the current filter — re-confirming the active repo fires
  a `set -g` + SIGUSR1 broadcast to every sidebar for a no-op
  (src/state/popup.rs:241-263). Early-return when unchanged.
- Clean: SIGUSR1 swap/re-latch coalescing (race-free given the 16 ms poll
  cap), scroll clamps, timer cadences (port 10 s, approval 2 s, bg-shell 5 s,
  session poll 10 s, globals floor 5 s), todo undo index math, park.rs
  grammars/SQL escaping/rollback, worktree markers parsing, version compare,
  port detection, time/DST, subprocess timeout + process-group kill, notices
  expiry. Per-tick spawn census: 1× list-panes -a + 1× full-system ps
  (architectural), show -g on signal/5 s floor, lsof 1/10 s, capture-pane per
  agy pane 1/2 s, sqlite3 1/10 s, git per stale path per 10 s TTL.

### Lane 4 — tmux/process/git/IO integration layer (batch 2)

- **H-tmux-1. `set-buffer` without `--` — clipboard copy silently fails for
  text starting with `-`.** The yank payload is passed as a bare positional in
  all four argv shapes (src/clipboard.rs:45-56); verified live on tmux 3.8,
  `tmux set-buffer -w '-w hello'` → `invalid flag -`, exit 1. Yanking a todo
  item written as a dash bullet (`- fix the login bug`) fails both the `-w`
  attempt and the plain fallback — nothing copied, no flash — and the first
  failure pins `W_SUPPORTED=false` for the process lifetime via `OnceLock`,
  degrading every later copy. `paste_to_pane` (src/tmux/commands.rs:240) uses
  `--` correctly. Fix: insert `--` before the payload in all four shapes
  (verified `set-buffer -w -- '-w hello'` succeeds on 3.8).
- **M-tmux-1. Approval worker writes literal `"clear"` into
  `@pane_attention`, which parses as attention-set.** Hook-side
  `set_attention(pane, "clear")` unsets the option, and
  `PaneAttention::from_label` maps any non-empty unknown value — including
  `"clear"` — to `Notification` (src/app/workers.rs:391 vs
  src/cli/mod.rs:110-116, src/tmux/types.rs:28-34). The moment an agy
  permission prompt is answered, the worker re-lights the flag it is trying to
  clear until the turn's Stop or a focus-tick masks it. Fix: use
  `unset_pane_option(pane_id, PANE_ATTENTION)`, matching the hook convention.
- **M-tmux-2. Polling-side teardown misses `@pane_subagent_routes` — stale
  routes leak into the next session on the same pane.** `clear_agent_pane_state`'s
  KEYS list claims to mirror `clear_all_meta` but omits the routes key
  (src/tmux/query.rs:703-724 vs src/cli/hook/context/meta.rs:93): a torn-down
  dead pane's `Type:id` routes survive, and a new session inheriting the pane
  gets straggler async-subagent hook events routed onto its row. Fix: add
  `PANE_SUBAGENT_ROUTES` to the KEYS list.
- **L-tmux-1.** A newline in a tmux-native field silently drops the pane
  record — `#{q:…}` escapes `|`, quotes, braces but not newlines (verified on
  3.8: `pane_current_path` with a real newline splits the record at Rust's
  `lines()` before field splitting; both halves fail MIN_FIELDS and the pane
  vanishes silently) (src/tmux/query.rs:81-121, 394; src/tmux/panes.rs:15-30).
  Detect over-short lines and trace, or use a newline-safe surrogate.
- **L-tmux-2.** SIGUSR1 identity check is TOCTOU: `/proc/<pid>/exe` passes,
  the pid is recycled, and `kill(pid, SIGUSR1)` (default disposition:
  terminate) hits the new owner (src/tmux/signal.rs:36-60). Linux window is
  microseconds; the macOS `ps` fallback window is wider (spawn + up to 2 s).
  pidfd_open + pidfd_send_signal, or re-verify exe immediately before kill.
- **L-tmux-3.** `ps` multi-word-comm spill heuristic misparses args repeating
  argv[0] within 16 chars — `bash -c bash`, `node node …` (src/process.rs:70-86).
  Require `args_cand.starts_with(&comm_cand)`.
- **L-tmux-4.** Drain threads read child output into memory unbounded
  (src/subprocess.rs:87-93) — a wedged/chatty child balloons RSS on a worker
  thread. Cap the drain buffer.
- **L-tmux-5.** port.rs keeps a divergent, smaller shell list missing
  dash/ksh/mksh/nu, so an idle `dash` pane reports `dash` as its command
  instead of walking deeper (src/port.rs:84-89 vs src/tmux/query.rs:743-763).
  Reuse `is_shell_basename`.
- **L-tmux-6.** Version comparison misses the rc→final transition:
  `1.2.3-rc1` vs `1.2.3` compares Equal, no update notice
  (src/version.rs:79-91). Treat a non-numeric suffix as lower than the same
  numeric version.
- **L-tmux-7.** Hook-side layout maintenance pays 3 tmux spawns per firing
  before its cheap early-out (src/cli/layouts.rs:762-815, 975-995) — one
  display-message for the sidebar width first would skip the full probe.
- Clean: paths.rs hardening, todo file locking + parse, park.rs
  parse-never-execute, time/notification escaping, setup shell_quote, no
  format-string injection anywhere, capture boundedness, git parsing
  (detached/unborn HEAD, diverged ahead/behind, slash+UTF-8 branches).

### Lane 0b — install-wizard.sh, shell-exit.sh (audited directly)

- **L-wiz-1. Downloaded binary is not integrity- or authenticity-checked.**
  `download_binary` curls `releases/latest/download/...` straight to
  `mv` (install-wizard.sh:88-114) — no checksum verification, no signature.
  The temp-file swap prevents truncation but a compromised CDN/network yields
  an executable binary the wizard happily installs and `pkill`s the old one
  for. Low practical risk for a personal tool; worth a sha256 pin if the repo
  goes wide.
- **L-wiz-2. `stop_running_instances` kills on full-argv match only after
  install, but the reload path sources tmux.conf with the binary already
  replaced** — mid-reload the running sidebar is killed (post_install_fixups)
  before `tmux source` re-creates it; if the user's tmux.conf does not load
  the plugin, the sidebar never comes back and the window has no pane restore
  (the killed pane's slot is gone). Acceptable for the wizard flow; noting the
  edge.
- **L-shell-1. zsh `preexec` writes synchronously per command.** Unlike the
  bash path (which batches via DEBUG trap + precmd flush), zsh calls
  `_interp` (a `tmux set-option` spawn) in `_tas_preexec_zsh` directly
  (shell-exit.sh:93-94) — one tmux spawn per command entered, on every shell
  in every pane. The bash lane already solved this; mirror the pending-variable
  pattern for zsh.
- **L-shell-2. bash DEBUG trap records pipeline stages, not commands.**
  `_tas_pending_cmd` is overwritten per pipeline stage
  (shell-exit.sh:72-78), so for `foo | bar` the recorded basename is `bar`
  (the last stage). Defensible ("the command that finished is what exited"),
  but it diverges from the preexec-style "the command line you ran" reading
  the zsh path uses (`$1` is the full first line). Cosmetic inconsistency.
- Clean: wizard platform detection, ETXTBSY-avoiding mv swap, tmux.conf
  reload ordering, bash precmd re-entrancy guard, non-tmux guard.


### Lane 1 — Hook/event pipeline (batch 1)

No High findings; concurrency-critical paths (subagent lists, session
binding/title, bg pid, notify stamps) are correctly serialized under per-pane
flocks, and no panic paths on malformed payloads.

- **M-hook-1. Subagent-drain downgrade to idle leaves the run clock running.**
  When the last child's SubagentStop empties `@pane_subagents` and no bg shell
  is live, the handler writes `@pane_status = "idle"` directly without
  clearing `@pane_started_at` (src/cli/hook/handlers/subagent.rs:84-97) —
  unlike `on_stop`, which calls `clear_run_state` before landing idle
  (run.rs:281-287). The Hermes parent-Stop-with-children path keeps the clock
  deliberately (run.rs:992), so after the drain the idle row renders a growing
  "12m30s" elapsed label forever (status.rs:30); the sidebar's own bg-shell
  sweep has the same gap (src/state/refresh.rs:1169-1171). Fix: call
  `clear_run_state(pane)` before the downgrade write.
- **M-hook-2. agy mid-turn PreInvocation restarts the run clock on every model
  call.** The agy guard suppresses promotion for `idle`+empty-prompt or
  `waiting`+pending-reason, but a mid-turn PreInvocation arrives with status
  `running` and a transcript-recovered (never-empty) prompt, so
  `started_stamp = if !prompt.is_empty() …` restamps `@pane_started_at` per
  model call (src/cli/hook/handlers/run.rs:49-72, 87-92) — agy rows show
  elapsed-since-last-model-call, not turn length, contradicting the documented
  intent that the clock spans the turn. Fix: for agy skip the stamp when
  `current_status == "running"`.
- **M-hook-3. Notification-path spawns paid even when nothing can fire.** On
  the hottest lifecycle events (Stop/Notification, run synchronously inside
  the agent's hook): (a) `notification_settings()` runs `tmux show -g` plus a
  `notify-send --version` probe before the handler possibly skips notifying
  (src/cli/hook.rs:145,177,196,214,264; desktop_notification.rs:118-120,
  298-332); (b) `notify_lifecycle` spawns `git rev-parse` (5 s timeout) to
  resolve repo/branch labels before `notify_if_allowed`'s enabled gate
  (src/cli/hook/notifications.rs:48-59, src/cli/hook/context/labels.rs:45-49,
  src/git.rs:9). With notifications off, every Stop still pays all three.
  Fix: resolve settings lazily in the handlers that actually notify; move
  label resolution inside `notify_if_allowed` past the enabled gate.
- **M-hook-4. Pane resolution does not canonicalize the `agy` alias.** The
  raw CLI arg flows into `pick_bound`/`pick_subagent` which compare against
  `@pane_agent` — always written canonically as `antigravity`
  (src/cli/hook/resolve.rs:92-116, 284, 296). A hand-wired `hook agy <event>`
  without `$TMUX_PANE` can never match a bound pane or route entry and drops
  (the identical bug was fixed in `handle_activity_log` per audit 2026-11-16
  #6, activity.rs:107-113). Fix: run the agent name through
  `canonical_agent_name` at the top of `resolve_hook_pane`.
- **L-hook-1.** Per-pane flocks have no deadline while their critical sections
  hold tmux and notify-send spawns (3 s timeouts): a tmux hiccup stacks
  multi-second latency onto every queued hook for that pane, and hooks block
  the agent CLI (src/paths.rs:95-98). Use `LOCK_NB` with bounded retry, or
  move tmux I/O out of the locked window.
- **L-hook-2.** Resume-path write fan-out: `on_tool_start` and
  `handle_activity_log` issue three separate spawns on resume where one
  `apply_pane_options` batch would do; `on_stop` re-reads `@pane_subagents` it
  just wrote and reads the notify stamp three times per Stop
  (run.rs:176-191, 272-300; activity.rs:220-238; meta.rs:120;
  desktop_notification.rs:201).
- **L-hook-3.** Transient tmux read failure is indistinguishable from an
  unclaimed pane: `get_pane_options` maps a failed `display-message` to
  all-empty strings (src/tmux/options.rs:577-598), so one timed-out spawn
  spuriously flips the pane to `running` and restamps the run clock
  (activity.rs:214-238, run.rs:146-176). Surface read failure and skip
  promotion on it.
- **L-hook-4.** `stop_failure_body` formats the raw error verbatim
  ("Task failed: {error}") while the Stop path truncates at 240 chars — a
  multi-KB error becomes a giant notification body and lands untruncated in
  `@pane_wait_reason` (src/cli/hook/notifications.rs:166-172, run.rs:337).
  Route through `truncate_body`.
- **L-hook-5.** Activity-log entries are dropped silently when the flock fails
  (src/cli/hook/activity.rs:43-47) with no `debug::log`, unlike every other
  lock user. One-line debug log.
- **L-hook-6.** The ancestry walk aborts on one unreadable `/proc` link
  instead of continuing to the parent (src/cli/hook/resolve.rs:196, 252-256) —
  a vanished intermediate shim level ends the walk even though `proc_parent`
  would still reach the pane. Fall through on environ-read failure.
- Clean: adapters (no panics on malformed JSON; registrations test-enforced),
  paths.rs O_NOFOLLOW/0600, debug rotation, activity trim off-by-one and
  concurrency, status priority matrix, shell/quoting safety on the hook path
  (no interpolation; `|`-split invariants enforced by write-site sanitization),
  epoch-seconds wire format consistency.


### Lane 2 — UI render layer (batch 1)

- **H-UI-1. Chyron ticker cut ~1.2 s short — width mismatch between display
  math and phase machine.** `visible_banner` computes display frames against
  `state.layout.header_width` (the full header-row width,
  src/state/chyron.rs:235-244), but the renderer scrolls the banner against
  `header_width - 6` (two 3-column buttons subtracted, src/ui/panes.rs:623-633,
  src/ui/panes/chyron.rs:36-43). Every banner overflowing the row has its
  EndHold swallowed and its scroll/fade cut ~1.2 s early, violating the
  documented "scroll must finish before the close" contract (panes.rs:642-647).
  Sub-symptom: a banner fitting `header_width` but overflowing `banner_w` by
  ≤2 columns reaches Rest 1-2 frames before the window closes, and
  `render_header_into` then leaves the stale banner text frozen mid-row
  (panes.rs:629-641 renders the banner only when `render_chyron` returns
  Some). Fix: single-source the width — store `banner_w` in FrameLayout or
  subtract both button widths inside `visible_banner`.
- **M-UI-1. Auto-scroll ping-pongs when the selected row is taller than the
  viewport.** With `@sidebar_preview_lines` up to 10 a full-mode row can reach
  ~17 lines; on a short list area the `first < offset` and
  `last >= offset + visible_h` branches in `compute_scroll_offset`
  (src/ui/panes.rs:667-687) alternate every frame — the viewport bounces at
  ~5 Hz while the cursor rests on the tall row. Fix: when row height exceeds
  `visible_h`, top-anchor once and skip the bottom branch for that row.
- **M-UI-2. ANSI capture parser advances one cell per char — no
  unicode-width handling.** `Perform::print` does `cursor_x += 1` per char
  (src/cli/capture/ansi.rs:116-123), feeding both `capture` output and the
  overview grid (src/cli/overview.rs:919-926). CJK/emoji occupy 2 columns in
  the source pane but land in 1 grid cell, shifting all following chars left
  one column per wide char; combining marks steal a cell instead of merging.
  Wide-char panes render progressively misaligned in overview/capture. Fix:
  advance by `UnicodeWidthChar::width(ch)`, placeholder cell for the trailing
  half, skip zero-width into the previous cell.
- **M-UI-3. Repo popup selection can land on a clipped, invisible row.** Only
  `visible_repo_rows` render with no scrolling (src/ui/panes.rs:577-612), but
  `repo_popup_nav_down/up` clamp to `repo_names().len()`
  (src/app/input.rs:529-542) and `confirm_repo_popup` applies whatever index is
  stored (src/state/popup.rs:241-250). Many repos + short sidebar: `j`/`k`
  move the highlight past the last rendered row, Enter applies a filter the
  user never saw. Fix: clamp to the visible window or add todo-style scrolling
  with a `N more ↓` indicator.
- **M-UI-4. Prompt preview re-wraps the entire prompt every frame.**
  `wrap_text_inner` does `text.chars().collect()` over the whole string
  (src/ui/text.rs:132) via src/ui/panes/row/body.rs:138-142 — at most 2 lines
  render, but the full captured prompt (unbounded, pasted text can be many KB)
  is collected and scanned per pane per frame at 5 fps. Fix: pre-truncate to
  `preview_lines * wrap_width + wrap_width` chars (the tail can never render)
  or memoize per pane like the todo wrap cache.
- **L-UI-1.** Spawn modal inline error invisible on a 12-row viewport:
  `EXP_ERROR_Y = 10`, expanded layout chosen at height ≥ 12, but the clamped
  popup leaves inner height 10 so the reserved error/status row never renders
  (src/ui/panes.rs:118-125, 157-174, 196-206) — exactly where inline feedback
  matters most. Treat height 12 as compact.
- **L-UI-2.** `wrap_flow_offsets` emits an empty first line for text starting
  with a space: break-at-space cuts at `pos+1` and the mid-text `trim_end`
  leaves line 0 empty (src/ui/text.rs:256-276) — a todo edit buffer whose
  first char is a space renders a blank row above the text. Skip leading
  spaces of the first chunk.
- **L-UI-3.** Flash banner color chosen by substring sniffing:
  `text.contains("failed")` paints error red (src/ui/panes.rs:715-719) — a
  success message mentioning "failed" (branch name, "0 failed" summary) renders
  red. Carry severity alongside the message in `set_flash`.
- **L-UI-4.** OSC-8 hyperlink overlay path is dead: `hyperlink_overlays`
  cleared every frame (src/ui/mod.rs:36) and written in render (src/app/render.rs:22-36)
  but nothing ever pushes a `HyperlinkOverlay`. Remove or re-wire.
- **L-UI-5.** `line_edit::visible_window` measures chars not display columns
  (src/line_edit.rs:159-178); latent — grep shows no production consumer
  despite the module doc's claim about src/cli/overview.rs. Drop or fix when a
  consumer appears.
- **L-UI-6.** Per-frame haystack lowercasing during an active search:
  `query_matches_lower` lowercases every matched field per row per frame
  (src/state/search.rs:51-56). Lowercase-once per snapshot or compare
  case-insensitively without allocation.
- **L-UI-7.** Popup-open per-frame rebuilds: help lines rebuilt each frame
  while open (src/ui/help.rs:102-132); menu popup clones the theme, rebuilds
  `repo_names()`, and builds all section lines twice per frame
  (src/ui/panes.rs:455-462); the edited todo item is wrapped twice per frame
  (src/ui/panes/todo.rs:158, 308). Bounded and popup-only — one cleanup pass.
- Clean: wide-char truncation/wrapping (text.rs), click-target ↔ render
  geometry, popup geometry clamps, scroll math, TODO empty/hidden handling,
  render-path purity (no tmux reads, no spawns), chyron phase timing itself.


### Lane 6 — CLI surface: overview, toggle, layouts, capture, setup (batch 3)

No High findings; the scariest paths (layout replay, auto-close guards,
drive-mode shell gate) are guarded by verified predicates, and every tmux call
rides the 3 s-deadline `exec_tmux` choke point.

- **M-cli-1. Overview TUI runs a full synchronous `ps` scan every tick on the
  UI thread.** `refresh_panes` → `build_session_snapshot` arms
  `ProcessSnapshot::scan` whenever any non-agent pane exists server-wide —
  virtually always (src/cli/overview.rs:767 → src/tmux/query.rs:195,
  845-858 → src/process.rs:26-35). The sidebar moved this exact scan to a
  background worker precisely because it blocked rendering (audit J1),
  but the overview reintroduced it inline: one ps spawn + full process-table
  parse every 500 ms, worst case a 5 s freeze under ps contention. Fix: reuse
  the worker pattern or cache the snapshot for N ticks.
- **M-cli-2. toggle-all open sweep fans out ~12 sequential tmux spawns per
  window, including a server-wide `list-panes -a` per window**
  (src/cli/toggle.rs:526-534 and the per-window chain at :35, :75, :225, :144,
  :158, :296, :881, :179, :201, :207, :212) — O(W) full-server scans; a
  20-window server takes ~1 s while the user waits. Fix: hoist the global
  mode read out of the loop; defer dedupe to the single end-of-sweep scan;
  chain each window's writes into one `;`-separated invocation
  (the `select_pane` pattern, commands.rs:273-293).
- **M-cli-3. While driving, every slow tick runs a second full-server
  `list-panes -a` for nothing** — `pane_dimensions()` fires right after
  `refresh_panes` already scanned the server, and its own dims result is
  discarded (src/cli/overview.rs:507 vs 470-475, 796; drive entry 615-617).
  Feed drive dims from the retained map.
- **M-cli-4. auto-close kill-window TOCTOU can destroy a pane created in the
  check window.** `list-panes` proves only sidebar panes remain, then two
  more spawns run before `kill-window -t` (src/cli/toggle.rs:1330-1367) — a
  pane created in between is destroyed with the window. Kill each role-tagged
  sidebar pane individually instead (killing the last pane auto-destroys the
  window; a raced-in pane survives).
- **L-cli-1.** Capture loop busy-spins and can fill the disk for absurd
  `--fps`: `Duration::from_nanos(1e9/fps)` truncates to 0 above 1e9 fps
  (u32 allows ~4.29e9), so `--fps 3000000000` writes ~3M HTML files in a
  tight loop (src/cli/capture/mod.rs:53). Reject fps > 1000 or floor the
  interval at 1 ms.
- **L-cli-2.** Silent success on failed creation: a failed `split-window`
  leaves `sidebar_pane` empty and `open_split_sidebar` still returns 0 — the
  keybinding does nothing and reports success; the toggle-all sweep counts
  the window as serviced (src/cli/toggle.rs:179-198, 217). Same shape in
  layouts passthrough (src/cli/layouts.rs:895-967). Return 1 or debug-trace.
- **L-cli-3.** Bracketed-paste fallback submits line-by-line: without
  `Event::Paste` support in the terminal chain, a multi-line paste into the
  prompt bar arrives as a key run and every forwarded Enter submits a line
  into the agent (src/cli/overview.rs:658-672, 555). Debounce
  consecutive-Enter-within-X-ms as a heuristic or document.
- **L-cli-4.** `--popup-client` swallows a following flag:
  `overview --popup-client --interval-ms 500` consumes the flag as the client
  name (src/cli/overview.rs:283-289). Reject values starting with `--`.
- **L-cli-5.** A typo'd subcommand silently launches the full TUI: unknown
  commands take the same fallthrough as no-args (src/cli/mod.rs:43,
  src/main.rs:102-105). Print usage and exit 2 for unknown first args.
- Cross-ref: ansi.rs wide-char handling (lane 6 re-found it as L3) is already
  recorded as M-UI-2; lane 6 adds that SGR 2/3/9/53 (faint/italic/strike/
  overline) are also dropped by the capture parser (src/cli/capture/ansi.rs:152-227).
- Clean: overview grid mapping (single cell_rects snapshot keyed by pane id),
  capture-pane SGR parsing, prompt bar (Enter → named Return, id-verified
  target, bracketed multi-line paste, shell guard), layout save/restore size
  guards, auto-close predicates, arg validation (no shell interpolation),
  setup output (generated from the same HOOK_REGISTRATIONS the adapters
  parse), exit codes, no reachable unwraps on user input.

### Lane 5 — input handlers + popups + worktree flow (batch 3)

No High findings. Specifically checked and clean: multi-byte slicing (all
edit buffers char-indexed; the byte-slicing sites are boundary-derived or
guarded — no panic path), key-order (Esc exclusively owned per state; Ctrl-C
guarded), worktree env isolation (no env leakage; agent launched via literal
send-keys), path traversal rejection in slug + flow, remove-flow ordering,
concurrent same-slug spawns (git serializes atomically), mouse drag flooding
(no redraw scheduling).

- **M-inp-1. Mouse wheel over an open modal scrolls the hidden background
  list.** `handle_event` dispatches ScrollUp/Down straight to
  `handle_mouse_scroll` with no popup check (src/app/input.rs:16-22,
  src/state/layout.rs:586-598): with the help overlay, repo/spawn/remove
  popup open, wheel events move `scrolls.panes.offset` behind the modal; after
  closing, the viewport has moved with no visible cause, and the help overlay
  itself has no wheel support. Fix: early-return (or route to help) in
  `handle_mouse_scroll` when a popup is open; add wheel→scroll_help while Help
  is open.
- **M-inp-2. The `/` search input stays live when a click hands the keyboard
  to the TODO panel.** The todo-click branch of `handle_mouse_click` never
  touches search state (src/state/layout.rs:832-871): search active + click a
  todo item → focus Todos, but `search.active` stays true and the header
  still renders the live-looking `/query█` input (rendered purely from
  `search.is_active()`); keystrokes now edit the todo panel while Enter opens
  a todo editor instead of confirming the filter. Same family: clicking the
  menu button opens the repo popup with the search line applied underneath.
  Fix: finish or cancel the search when a click moves focus into the todo
  panel or opens a popup.
- **L-inp-1.** `u` undo is silently consumed when the todo file changed since
  the deletion — including this sidebar's own writes inside the 4 s window:
  the stash is taken first, then the stamp check refuses, so the key no-ops
  while the "cleared N completed · u to undo" banner may still be on screen
  (src/state/todo.rs:550-561, 969-977). Show "undo expired — list changed"
  instead of returning silently.
- **L-inp-2.** Per-keystroke search matching allocates per field per row on
  both the rebuild and render pass (src/state/search.rs:51-56,
  src/state/layout.rs:235-244, row_collector.rs:88-94, todo.rs:675-701);
  todo edit mutators rebuild the whole String per char. Pre-lower the matched
  fields once and share one match pass between rebuild and collector.
- **L-inp-3.** A spawn that succeeds after the working modal was dismissed
  lands with zero feedback: the late `Spawn(Ok)` is dropped because the popup
  no longer matches (src/state/popup.rs:644-648) — a worktree + window can
  appear seconds later with no flash. When the op-identity fix lands, flash
  on late Spawn(Ok) instead of dropping.
- Clean: line_edit (char-based, clamped), focus.rs, scroll.rs, markers
  parsing (write-time sanitization), slug/branch derivation (clean failure on
  unicode-only names), todo editing layer, click routing precedence.

## Feature opportunities (recorded by lanes)

From the hook lane: embed TMUX_PANE in hook payloads to drop resolution fallbacks; SubagentStop's discarded last_message as a feed entry; per-line writer pid/sequence in the activity log.

From the UI lane: repo popup scrolling with a `N more ↓` indicator; memoized prompt-preview wraps; attention summary in the toolbar gap; wire or remove the OSC-8 overlay plumbing.

From the state lane: snapshot change-detection (hash raw list-panes output, skip parse/fold on byte-identical); fold the three git spawns per path into one `git status --porcelain -b`; batch pane-option writes (16 unsets → one call).

From the tmux lane: share one ps snapshot between snapshot and port-scan
workers; pidfd-based signaling; push-based shared-option sync via tmux hooks
instead of the 5 s poll.

From the input lane: editing keys (Left/Right/Home/End/Ctrl-w) in the
spawn-modal NAME field, reusing LineEdit like the todo editor and search
line; bracketed paste for sidebar inputs so a multi-line paste lands as one
draft instead of one commit per Enter; unicode-aware slugify fallback
(transliterate or timestamp slug) so a pure non-ASCII task name does not
dead-end on the empty-slug error.

From the CLI lane: move overview captures + the ps scan to a background
worker (the snapshot-worker pattern) so ticks and keystrokes never block on
tmux/ps; toggle-all `;`-chained invocations (~12 spawns/window → 2-3);
overview zoom mode paging through the target pane's scrollback with
`capture-pane -S/-E` offsets (the vte renderer already parses styled grids).

From the upgrade check: bridge event-pump `types` filter —
`ctx.event.subscribe({ types })` (v2.2.0+) filters server-side at the bus;
subscribing with the ~17 handled types keeps message-part deltas and other
high-volume events from crossing into the plugin on a busy daemon. Also
worth noting: the H-bridge-1 fix can now build on the `experimental.idle`
hook's background-subagent list instead of child execution-event inference
(see the Lane 0 upgrade check).

## Summary

Counts: 4 High, 23 Medium, 41 Low across eight lanes (0, 0b, 1-6).

Priority order for a hardening pass, risk-adjusted:

1. H-tmux-1 (`set-buffer --`) — one-line fix, silently breaks the most common
   yank shape (dash-bullet todos) and poisons W_SUPPORTED for the process.
2. H-state-1 (startup git priming on the render thread) — highest-impact
   freeze; violates the codebase's own stated invariant (git.rs:6-8).
3. H-bridge-1 (opencode pane idle while background subagents run) — the
   user-reported daily-workflow miss; the activeChildTurns design, or the
   cleaner `experimental.idle` background-subagent signal added in opencode
   2.2.1 (see the Lane 0 upgrade check).
4. H-UI-1 (chyron width mismatch) — mechanical single-sourcing of one width.
5. The wrong-state family on hot paths: M-tmux-1 (`"clear"` re-lights
   attention), M-tmux-2 (stale subagent routes survive teardown), M-hook-1
   (idle with running clock after subagent drain), M-bridge-2 (listPanes
   exit/close truncation).
6. M-state-1 + M-state-2 + L-inp-3 (popup op identity, resume guard, late
   spawn feedback) — one design pass over popup op lifecycle.
7. M-inp-1 + M-inp-2 (modal wheel bleed, search living through focus changes)
   — one pass over mouse/focus interaction rules.
8. The spawn-waste family: M-hook-3, M-state-3, M-state-4, M-cli-1, M-cli-2,
   M-cli-3 — a perf pass; M-cli-2 and M-state-3 pay off most on busy servers.
9. Everything else by lane, Ls last (many are one-liners: L-hook-4/5,
   L-tmux-5/6, L-cli-4/5, L-state-1/5).

