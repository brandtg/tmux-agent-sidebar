# Documentation & Website Audit — 2026-11-20

Scope: every document in the repo except AGENTS.md, README.md, and CHANGELOG policy — the
website content tree (21 pages), the reference/design docs under docs/ (17 files), and the
website build config. Motivation: republishing the site on GitHub Pages, plus a general
docs/ cleanup pass (handoff-note-style docs are deletion candidates).

Method: four parallel audit lanes (website reference pages; website guides/features/agents
pages; docs/ reference docs; docs/ specs/plans/todos/handoffs). Every checkable claim was
verified against source. All High-severity findings were then re-verified by hand against
the actual bytes before being recorded here. One lane claim was corrected during
verification (noted in Appendix B).

Ground truth: main @ 8a4aa7c, v1.4.1.

## Summary

- Audited: 38 documents (21 website pages, 17 docs/ files) plus astro.config.mjs and the
  deploy workflow.
- Website: 4 pages in major drift, 9 in minor drift, 8 accurate. Roughly 8 High,
  10 Medium, 17 Low drift items.
- docs/: state-management.md is the standout (6 High drift items — it is one to two
  feature generations behind). tmux-3.8.md is accurate. Nine files are deletion
  candidates, three more are archive candidates.
- Deletion candidates: 9 files (pet page spec + plan, other-windows plan, todo redesign
  working doc, nvim handoff, and all four past audit reports).
- Website build config: healthy — every sidebar slug resolves, no broken internal links,
  assets all exist. The deploy workflow is manual-only (workflow_dispatch) and Pages is
  not yet enabled on the repo, which matches the intent to republish.

Severity scale: High = document contradicts source (nonexistent feature/option/event,
wrong default, broken instruction). Medium = source-added material missing from docs, or
a design-spec/implementation divergence. Low = wording, omissions of minor keys/fields,
stale line references.

---

# Part 1 — Website (publish priority)

Verdicts per page:

| Page | Verdict |
|---|---|
| index.mdx (landing) | MINOR-DRIFT |
| features/agent-pane.mdx | ACCURATE |
| features/git-status.mdx | ACCURATE |
| features/worktree.mdx | ACCURATE |
| features/notifications.mdx | MINOR-DRIFT |
| getting-started/installation.md | MINOR-DRIFT |
| getting-started/fork.md | MINOR-DRIFT |
| getting-started/claude-code.md | ACCURATE |
| getting-started/codex.md | ACCURATE |
| getting-started/opencode.md | ACCURATE |
| getting-started/hermes.md | ACCURATE |
| getting-started/antigravity.md | ACCURATE |
| agents/index.md | MAJOR-DRIFT |
| agents/claude-code.md | MINOR-DRIFT |
| agents/codex.md | MINOR-DRIFT |
| agents/opencode.md | MAJOR-DRIFT |
| agents/hermes.md | MINOR-DRIFT |
| agents/antigravity.md | MINOR-DRIFT |
| reference/keybindings.md | MINOR-DRIFT |
| reference/tmux-options.md | MAJOR-DRIFT |
| reference/scripting.md | MAJOR-DRIFT |

The eight High-severity website findings, in fix-priority order:

| Page | Documented | Actual source | Severity |
|---|---|---|---|
| reference/tmux-options.md | @sidebar_default_todo_hide_completed default off ("completed items stay visible") | agent-sidebar.conf line 133 seeds it to on when unset; a new sidebar starts with the completed section collapsed | High |
| reference/scripting.md | @pane_name = "friendly agent/session name (from /rename on Claude)" | Nothing ever writes @pane_name — not the hooks, not the OpenCode bridge. /rename names reach rows via a polled session-id-to-name map; the hook-written title option is @pane_session_title | High |
| agents/index.md | "Background shell state: Claude only — the others do not document a background flag" | Hermes background shells ARE tracked: adapter/hermes.rs translates detached terminal(background=true) spawns into run_in_background and tracks liveness by PID | High |
| agents/index.md | "Worktree lifecycle tracking: Claude — requires WorktreeCreate / WorktreeRemove hooks" | The feature no longer exists anywhere: no adapter registers those hooks, no AgentEventKind variants; features/worktree.mdx itself says the plugin no longer registers them | High |
| agents/opencode.md | Status driven by session.created / session.status / session.idle | The 2.0.x bridge drives status from session.execution.started / succeeded / failed / interrupted; session.idle is a legacy fallback only | High |
| agents/opencode.md | Waiting from permission.asked and form.created; cleared by form.replied / form.cancelled | The bridge handles question.asked / question.replied / question.rejected; no form.* event exists | High |
| agents/claude-code.md | "PR number (needs gh CLI)" | No gh invocation exists anywhere in src; no PR field in PaneGitInfo or row rendering | High |
| agents/codex.md | "PR number (needs gh CLI)" | Same — feature does not exist | High |

## Per-page details

### reference/tmux-options.md — MAJOR-DRIFT

| Section | Documented | Actual source | Severity |
|---|---|---|---|
| todo_hide_completed default | off | conf seeds on (line 133) | High |
| @sidebar_color_running / waiting / error | "paints ... the done/waiting/failed chyron flash" and "version banner" | Chyron flash colors are fixed ramps in src/lib.rs (CHYRON_DONE/WAITING/FAILED), not derived from these options; no version banner exists (ChyronKind is only Done/Waiting/Failed) | Medium |
| Completeness claim ("every option the plugin reads") | — | Five color options missing: @sidebar_color_flag (208), @sidebar_color_commit_hash (221), @sidebar_color_file_change (221), @sidebar_color_pr_link (117), @sidebar_color_activity_timestamp (109) | Medium |
| Sidebar behavior table | — | @sidebar_default_show_windows (landing default for the w toggle) undocumented | Medium |
| Internal options table | last_window_width, auto_hidden | Also @sidebar_saved_layout, @sidebar_pid, @agent_sidebar_dir, and the sync flags (@sidebar_cursor, @sidebar_repo_filter, @sidebar_compact, @sidebar_collapsed_groups, @sidebar_todo_*, @sidebar_show_windows) | Low |
| nerd icon glyph list | includes nf-fa-bars; running listed as nf-fa-bars-shifted list | Actual: running nf-fa-circle, done nf-fa-check (missing from doc), background nf-fa-bullseye, waiting nf-fa-adjust, idle nf-fa-circle-o, error nf-fa-times, unknown nf-fa-circle-thin; nf-fa-bars is unused | Low |

Everything else verified exact: all option names, all default values (width 15%, position
left, preview_lines 2 clamped 1-10, popup_max_width 100, auto_hide on, notifications on,
agents allow-list, spawn defaults, branch prefix agent/, worktree dir .worktrees, every
color and icon default), hex and bare RRGGBB parsing, popup narrow-client behavior,
toggle-all-force-split behavior.

### reference/scripting.md — MAJOR-DRIFT

| Section | Documented | Actual source | Severity |
|---|---|---|---|
| @pane_name | written by /rename | Never written by anything; use @pane_session_title | High |
| @pane_subagents | "comma-separated subagent labels" | Entries are Type:id pairs, e.g. Explore:sub-1 | Low |
| @pane_worktree_name/branch | "when the pane was spawned from the sidebar" | Written from worktree info in any hook payload, including manual worktrees the agent reports | Low |
| @pane_started_at | "epoch seconds of the last UserPromptSubmit" | Also seeded by the first tool event when missing; empty-prompt synthetic submits do not reset it | Low |

Gaps (the page's stated purpose is scripting, and the scriptable surface is absent):
- The CLI subcommands: set-status, todo (add/list/done/open/rm), spawn, capture (with
  --session/--window/--frame-out/--frames-out/--duration-ms/--fps/--crop-rows/--crop-cols),
  setup, hook, toggle, toggle-all, auto-close, maintain-width, notify-focus, overview,
  version — none documented.
- Exit-code convention (0 ok / 1 runtime failure / 2 usage).
- Activity log path and line format (epoch-seconds|tool|label, 200-line cap).
- Missing options worth documenting: @pane_session_title, @sidebar_flagged,
  @pane_last_cmd / @pane_last_exit, @pane_launch_cwd.

### reference/keybindings.md — MINOR-DRIFT

| Section | Documented | Actual source | Severity |
|---|---|---|---|
| Navigation | j/k/Down/Up only | Ctrl-n / Ctrl-p are aliases at Panes focus and in the repo popup | Medium |
| Z | "expand every collapsed group" | Z toggles: folds everything when fully expanded, expands when anything is folded | Medium |
| Notices popup | standalone popup opened via the ⓘ badge | No separate notices popup: the header button is a merged menu; notices are a section of the repo popup (same r / Esc handling) | Medium |
| Help popup | ? / Esc only | Also scrollable (j/k/PageUp/PageDown) | Low |
| Search matching | session title, tmux session name, git branch | Also matches the agent label and group name | Low |
| TODO panel | undo only from panel focus | u/Ctrl-z work from any non-text-input focus; q/c/w/?// stay live while the panel has focus; editor keys (Home/End, Ctrl-a/e, Ctrl-Left/Right, Delete, Ctrl-u, Ctrl-w) undocumented | Low |
| Mouse | not documented | Left-click rows/headers/checkboxes/menu, wheel scroll | Low |

The rest of the table verified exact, including the TODO table, close-pane modal, spawn
modal, resume command shapes, and the 4-second undo window.

### agents/index.md — MAJOR-DRIFT

Two High rows above (background shell, worktree lifecycle). The rest of the feature
matrix verified accurate: status/waiting/failure rows, permission badges, git branch
sources, task progress (Claude only), notifications, sub-agent display (Claude + Hermes),
activity log caveats.

### agents/opencode.md — MAJOR-DRIFT

Three High rows above (v1-era status events, form.* events). Also: the "worktree
lifecycle not available" row's rationale is stale sidebar-wide (Medium), and session
titles (bridge session-title event / session.renamed / opencode.db poll) are implemented
but unmentioned while agent-pane.mdx advertises them.

### agents/claude-code.md, agents/codex.md — MINOR-DRIFT

The PR-number claim (High, listed above). codex.md additionally: the StopFailure
"Claude-only" parenthetical is stale (OpenCode and Antigravity also wire it, Low) and the
worktree not-available rationale is stale (Medium).

### agents/hermes.md, agents/antigravity.md — MINOR-DRIFT

- hermes.md: "Background shell state — Hermes background processes have no
  sidebar-visible flag" contradicts the implemented PID-based tracking (High, listed
  above). Worktree rationale stale (Medium).
- antigravity.md: worktree not-available rationale stale (Medium). Everything else
  verified, including the capture-scan wait detection patterns and the 2s cadence.

### features/notifications.mdx — MINOR-DRIFT

| Claim | Actual | Severity |
|---|---|---|
| stop fires for "Claude, Codex, OpenCode" | Fires for all five agents (Hermes post_llm_call, Antigravity Stop) | Medium |
| notification fires for "Claude, OpenCode" | Hermes on_human_input_request also fires it | Medium |
| stop_failure for "Claude, OpenCode" | Antigravity also wires it | Medium |

This contradicts the site's own agents/index.md. Gap: the 120s per-fingerprint cooldown
is undocumented.

### features/agent-pane.mdx, git-status.mdx, worktree.mdx — ACCURATE

No contradictions. Optional polish: add the · unknown icon, note Z toggles both
directions, mention the ☰ overview header button (agent-pane); note unnamed worktrees
render as + <branch> (git-status). Everything checkable matched, including status
priority order, chyron timing constants, spawn/close modal semantics, slugify rules, and
the git worker's TTL behavior.

### index.mdx (landing) — MINOR-DRIFT

- Hero tagline names only Claude Code, Codex, OpenCode; the feature card on the same
  page (correctly) lists all five agents. Low.
- "Hermes and Antigravity merge a generated hook block into their configs" —
  Antigravity's primary path is automatic wiring on every plugin load (entry script
  generates the bundle and symlinks it); the merge is the manual fallback. Low.
- Quick start, fork attribution, and all asset references verified.

### getting-started/installation.md — MINOR-DRIFT

Instructions all verified (TPM flow, wizard, update flow, manual clone, prebuilt asset
names, debug env var). Gaps: Next steps links only Claude/Codex/OpenCode — the Hermes and
Antigravity setup pages exist but are unlinked; no mention that plugin load
auto-creates the OpenCode bridge symlink and the agy bundle.

### getting-started/fork.md — MINOR-DRIFT

| Claim | Actual | Severity |
|---|---|---|
| New options list includes @sidebar_default_view | No such option exists; the real defaults are @sidebar_default_compact_view, @sidebar_default_show_windows, @sidebar_default_todo_visible, @sidebar_default_todo_hide_completed | Medium |

Everything else in the fork diff summary verified.

### getting-started/{claude-code,codex,opencode,hermes,antigravity}.md — ACCURATE

All hook mappings match the adapters' HOOK_REGISTRATIONS tables exactly (Claude 15
triggers, Codex 4, Hermes 9 rows, Antigravity 5), settings paths and consent notes match
setup output, the Codex feature flag matches the shipped prompt, the OpenCode symlink
instructions match the entry script (which now does the symlink automatically — worth a
one-line note), and the Antigravity bundle paths match the entry script.

### Build config — healthy

- Every sidebar slug in astro.config.mjs resolves to a content file (features pages are
  .mdx).
- No broken internal links found (the reference pages link to
  /tmux-agent-sidebar/features/notifications/, which exists).
- All referenced assets exist (public/brands/, src/assets/captures/hero.png).
- deploy-website.yml is workflow_dispatch-only with a comment that Pages is not enabled
  yet — flip the trigger when republishing.

---

# Part 2 — docs/

| Doc | Classification | High drift | Currency | Recommendation |
|---|---|---|---|---|
| state-management.md | reference | 6 | Half-current; one-two feature generations behind | KEEP-UPDATE |
| tmux-3.8.md | living plan | 0 | Current (nothing started; inventory accurate) | KEEP-AS-IS |
| oss-hygiene.md | completed track | 0 | Finished; durable output self-documenting in repo | ARCHIVE or DELETE |
| overview-grid-spike.md | spike/design note | 1 | Mostly current; header framing stale | KEEP-UPDATE |
| superpowers/specs/2026-04-19-pet-page-design.md | spec | total | Feature shipped then removed | DELETE |
| superpowers/plans/2026-04-19-pet-page.md | plan | total | Same | DELETE |
| superpowers/specs/2026-10-07-other-windows-design.md | spec | structural | Implemented, grown beyond spec | ARCHIVE |
| superpowers/plans/2026-10-07-other-windows.md | plan | stale snippets | Fully implemented | DELETE |
| superpowers/specs/2026-10-31-todo-list-design.md | spec | superseded UI model | Storage sections still accurate | ARCHIVE |
| superpowers/specs/2026-11-15-agent-chyron-design.md | spec | 0 | Tracks implementation faithfully | KEEP |
| superpowers/specs/2026-11-15-todo-agent-interop-design.md | spec | 0 | Phases 1-3 shipped, 4-5 are the live roadmap | KEEP |
| todo-list-2026-11-12.md | working doc | 0 | Fully implemented | DELETE |
| nvim-program-detail-handoff.md | handoff | n/a | Nothing built | DELETE |
| audit-2026-10-16.md | past audit | n/a | All findings verified fixed by the 11-16 pass | DELETE |
| audit-2026-10-30.md | design consult | n/a | Search shipped and documented | DELETE |
| audit-2026-11-16.md | past audit | n/a | Findings fixed | DELETE |
| audit-2026-11-19.md | past audit | n/a | All 20 findings fixed in #148/#149 | DELETE |

### docs/state-management.md — KEEP-UPDATE (the main repair job)

Six High drift items, each a contradiction with source:

| Claim | Actual | Severity |
|---|---|---|
| status_filter has a last_saved_* marker in GlobalState | No status_filter exists anywhere in src (removed by the chyron change); GlobalState has only cursor/repo_filter/collapsed_groups markers | High |
| Scroll is re-anchored to the top on SIGUSR1 reload (reset_pane_scroll) | reset_pane_scroll does not exist; app.rs deliberately preserves cursor and scroll (documented in code, audit 2026-11-16) | High |
| refresh() every 1s calls tmux::query_sessions() | Per-tick path is worker-fed: SnapshotRequest → snapshot_poll_loop → fetch_pane_listing → apply_snapshot. query_sessions() is off the per-tick path | High |
| refresh_port_data() and scan_session_process_snapshot() | Neither function exists; actual: queue_port_scan_if_due and apply_process_snapshot with dead_scan_streak teardown | High |
| FrameLayout.repo_button_col | Field is menu_button_col (the ⓘ button became the merged menu) | High |
| NoticesState.button_col | Does not exist — same menu-button rename | High |
| RefreshTimers.last_filter_click | Does not exist | High |
| PopupState.SpawnInput { mode_idx } | Field is kind_idx; also missing Help popup variant, Repo.repo_start, RemoveConfirm rework | High |

Medium drift: the per-pane option table lists claude/codex/opencode only (five agents
tracked) and omits ~9 options (@pane_name, @pane_role, @sidebar_flagged, the notification
and pending-worktree options, @pane_last_cmd/exit); the Global table omits the
show_windows/todo_* globals and their defaults; PaneRuntimeState, FocusState, and
ScrollStates key-type lists are missing fields; the sessions.names scan now also covers
Antigravity and OpenCode; the update-cycle summary omits the TODO reload, approval scan
(2s), bg-shell sweep, and chyron diff buckets.

Low: activity-log trim detail (200 with rewrite at 210).

What is still accurate: the three-scope model, the tmux-option sync mechanics, the
invariants, most per-pane option names, and the module map. The skeleton is sound — it
needs a rewrite pass, not a restart.

Cross-cutting: several of these errors (status_filter, repo_button_col, button_col) also
exist as stale comments inside src (src/state/search.rs:5, src/state.rs:60,
src/ui/panes/row_collector.rs:323) — the doc was written from those comments, so fixing
the doc alone will not remove the drift source. Fix the comments in the same pass.

### docs/tmux-3.8.md — KEEP-AS-IS

A living plan for tmux 3.8 compat (batches T0-T5). Status "not started" is still true —
no floating-pane, pane-command, client_colours, or capture-pane -I usage anywhere. The
"already handles passively" inventory all verifies (layout JSON handling at
toggle.rs:340-362, show -g escape decoding at options.rs:393+, the conf 3.2 gate, the
OnceLock version probe, the 36-field listing). Only line-number citations have drifted
slightly.

### docs/oss-hygiene.md — ARCHIVE or DELETE

Every item (H1-H4) is done and verified against the repo: macOS CI leg, clippy -D
warnings, cargo-audit gate (mechanism later reworked in #149), shellcheck job, ignored
integration tests in CI, website build job, CONTRIBUTING.md + templates + SECURITY.md,
metadata, hook/toggle pipeline tests. The track is complete and its outputs are
self-documenting. One superseded progress-log detail (#149 rework). For a public repo
this is the handoff-note shape the cleanup targets.

### docs/overview-grid-spike.md — KEEP-UPDATE

One High item: the header says "spike — not wired into the plugin yet," but the overview
grid shipped (o key, ☰ header button, overview subcommand, help-overlay entry) — the
doc's own later sections describe the shipped behavior, so it contradicts itself. The
design rationale (never attach, capture-pane -p -e pipeline, theme normalization,
selection by pane id) and the ansi.rs bug findings (pinned by regression tests quoting
the same bytes) are durable reference material. Fix: update the status header to
"shipped." Note src/cli/overview.rs:1-3 points at this file, so it must stay.

### docs/superpowers/ — two KEEP, two ARCHIVE, three DELETE

- KEEP: 2026-11-15-agent-chyron-design.md (implementation notes track the shipped
  behavior; referenced by state-management.md:230; the only record of the chyron design
  rationale) and 2026-11-15-todo-agent-interop-design.md (phases 1-3 shipped; phases 4-5
  — parked sessions, dispatch — exist nowhere else and the code's own docstrings point
  at this spec).
- ARCHIVE: 2026-10-07-other-windows-design.md (feature shipped and grown: OtherWindow has
  15 fields vs the spec's 9; grouping is now a toggle with a session-mode counterpart;
  the finished-flash is magenta, not the documented green diamond; the StatusFilter gate
  is obsolete) and 2026-10-31-todo-list-design.md (storage/concurrency design still
  accurate; the interaction model was superseded the next day by the 2026-11-12
  redesign).
- DELETE: 2026-04-19-pet-page-design.md and 2026-04-19-pet-page.md (the pet feature and
  its docs page were shipped in #59/#45 and deleted in 5b9cc06 — every symbol, file,
  option, and anchor the docs name is gone) and 2026-10-07-other-windows.md (consumed
  execution plan whose inline snippets embed stale signatures that would mislead anyone
  following them today).

### Standalone docs/

- DELETE: todo-list-2026-11-12.md — fully consumed working doc (all phases ticked, all
  polish findings resolved with dated notes); user-facing behavior is documented in
  README, the website keybindings page, and the help popup. Its two-sentence Deferred
  section (stable IDs, drag-to-reorder) is the only forward content, and it is negligible.
- DELETE: nvim-program-detail-handoff.md — nothing was built (confirmed: no @pane_program,
  no setup nvim, no lua emitter anywhere). One caveat before deleting: its standing
  UI-contract bullets (text_inactive reserved for agent text; magenta = process vs green
  = agent) are only partially captured in row.rs comments — fold them into
  state-management.md first if the contract matters in prose.
- DELETE: the four past audit reports (2026-10-16, 10-30, 11-16, 11-19). Each documents
  findings that were subsequently fixed and verified fixed (the 11-16 pass re-verified
  all ten October items; #148 resolved all twenty 11-19 findings; #149 reworked the
  audit CI job). The durable value lives in git history and commit messages. The 10-30
  file is really a design consultation for / search, which shipped and is documented.

### Reference integrity check

- AGENTS.md points at docs/state-management.md (KEEP) — safe.
- src/cli/overview.rs:3 points at docs/overview-grid-spike.md (KEEP-UPDATE) — safe.
- state-management.md:230 points at the chyron spec (KEEP) — safe.
- Nothing in src/ or tests/ references any file on the deletion list. The todo spec is
  referenced only by todo-list-2026-11-12.md, which is itself deleted. No dangling
  references would result.

---

# Part 3 — Proposed action plan

Nothing has been changed yet. Proposed sequence:

1. Docs cleanup (mechanical, low risk)
   - Delete the 9 deletion-candidate files listed in Part 2.
   - Move the 3 archive candidates (other-windows spec, todo-list spec, oss-hygiene) to
     docs/history/ (or delete, if no archive is wanted).
   - Result: docs/ shrinks from 17 files to 5 living files + this report.
2. Website fix pass (the republish blocker)
   - Fix the 8 High items first (defaults, nonexistent features, stale event names).
   - Then the Medium items: chyron-flash color attribution, missing color options,
     notifications agent columns, worktree-rationale rows, fork.md option name,
     installation next-steps links.
   - Then the Low/polish items and the scripting page's CLI-surface section.
3. state-management.md rewrite pass
   - Remove the 8 stale symbols, add the TODO/show_windows globals, the Help popup
     variant, the missing key-type fields, and re-describe the refresh pipeline as
     worker-fed. Fix the three stale src comments in the same pass.
4. Small doc touch-ups
   - overview-grid-spike.md: status header to "shipped."
   - Fold the nvim handoff's UI-contract bullets into state-management.md (if wanted)
     before deleting it.
5. Republish
   - Rebuild the site locally (npm run build in website/) to confirm, then enable Pages
     and flip deploy-website.yml to push-triggered.

## Appendix A — Verification notes

High-severity findings were re-verified against source bytes before inclusion:
- agent-sidebar.conf:133 seeds @sidebar_default_todo_hide_completed on.
- @pane_name appears only as a read-side constant and format placeholder; no writer
  exists in src/ or the bridge.
- No gh invocation in src/git.rs; no PR field in PaneGitInfo or row rendering.
- src/adapter/hermes.rs:79-82 translates background:true into run_in_background.
- .opencode/plugins/tmux-agent-sidebar.js drives status from session.execution.* with
  session.idle as a legacy fallback, and handles question.asked/replied/rejected.

## Appendix B — Corrected lane claim

One audit lane reported that website/src/content/docs/features/ does not exist, making
the Notifications links in the reference pages "dead." That is wrong: the features pages
exist as .mdx files (agent-pane, git-status, notifications, worktree) and the links
resolve. The finding is excluded from the counts and fix list.
