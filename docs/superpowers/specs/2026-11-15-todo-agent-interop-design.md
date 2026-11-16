# Agent interop for the TODO panel

## Goal

Turn the TODO panel from a passive notepad into the place where work gets
handed to and received from agents. Five features, ordered by payoff, all
sharing one state directory:

1. **Yank** — `y` copies a todo item to the clipboard (kills the manual
   copy/paste of resume commands today).
2. **todo CLI** — a `tmux-agent-sidebar todo` subcommand so agents,
   slash commands, and scripts can append items; the shared file means
   every open sidebar shows them within a second.
3. **Resume-shaped todos** — an item whose text is a resume command
   (`claude --resume <id>`, `codex resume <id>`, `opencode -s <id>`)
   gets a glyph and an `r` key that relaunches it in a new tmux window
   at the right working directory.
4. **Parked sessions** — press `p` on a pane row to durably record
   {agent, session id, cwd, title} in a sidecar file; a Parked view in
   the panel resumes them after the pane is gone. Optional auto-park on
   SessionEnd — the audit docs' "resume-on-death" item.
5. **Dispatch** — `m` sends a todo's text to a running agent as a
   prompt; `S` spawns a new agent with the todo as its initial prompt.

The unifying rule: todos.txt stays exactly what it is. All machine
metadata lives in sidecar files or is *parsed out of* item text — never
stored inside it.

## Background

What exists today, and what it means for this design:

- The TODO list is a plain shared file (src/todo.rs):
  `$XDG_STATE_HOME/tmux-agent-sidebar/todos.txt` (or
  `~/.local/state/tmux-agent-sidebar/`), `[ ]` / `[✓]` lines, flock +
  atomic rename, and every sidebar re-parses it within one refresh tick
  when the `(mtime, len)` stamp changes. Anything that can write the
  file can add todos — no IPC, no daemon.
- The sidebar knows each pane's agent session id (`@pane_session_id`,
  set by every adapter via `EventBase::new`), plus cwd, worktree
  branch, agent type, and last prompt — but all of it lives in tmux
  pane options that die with the pane. Non-Claude panes are actively
  wiped by `clear_agent_pane_state` (src/tmux/query.rs) when the agent
  exits; Claude panes are cleaned by its SessionEnd hook.
- There is no resume capability anywhere: no code constructs
  `--resume`-style commands. docs/audit-2026-10-30.md lists
  "Resume-on-death: capture the resume command per agent, offer
  respawn from the sidebar" as an unbuilt roadmap item.
- Spawn machinery (src/worktree/flow.rs) creates a worktree + window
  and types a command with literal send-keys; `SpawnRequest` carries no
  prompt, and no user text is ever embedded in a command today.
- The CLI is a hand-rolled match (src/cli/mod.rs); a new subcommand is
  one match arm plus a module. Claude's SessionEnd hook is already
  registered and dispatched end-to-end to `on_session_end`
  (src/cli/hook.rs:120-132).
- The agents' own stores survive pane death and are already scanned for
  titles by src/session.rs — the same files carry the cwd a resume
  needs (verified on this machine: `~/.claude/sessions/<pid>.json`
  holds `pid`, `sessionId`, `cwd`, `startedAt`, `name`, `nameSource`).

## Per-agent resume grammar

The heart of features 3 and 4. Each agent resumes differently —
different flag style, different id shape, different case rules — so
everything is keyed per agent. One table, two consumers: the todo-text
recognizer and the parked-session launcher.

| agent | resume command | id shape | case rule |
| --- | --- | --- | --- |
| claude | `claude --resume <id>` (also `-r`) | UUID: hex + dashes, ≥ 8 chars | matched case-insensitively; rebuilt lowercase |
| codex | `codex resume <id>` | UUID, or a thread name: `[A-Za-z0-9._-]+` not starting with `-` | preserved verbatim |
| opencode | `opencode -s <id>` (long form `--session`) | `ses_` + `[A-Za-z0-9]+` | preserved verbatim — ids are mixed-case (`ses_e1eb53dd9ffemw0k3treOa3PBO`), never lowercased |
| agy / antigravity | none known | — | recognizer declines; items stay plain todos |

Settled rules that fall out of this table:

- **Parse, never execute.** The launched command is *rebuilt* from
  (agent, id) via `resume_command()` — canonical flags, no shell
  metacharacters in the output since ids are pattern-validated. The
  item's text is only ever read.
- **Case is per agent.** The recognizer is case-sensitive for opencode
  (`ses_` ids) and case-insensitive only for the hex of claude UUIDs.
  There is no global "lowercase the id" step.
- The table lives beside the existing per-agent command builder
  (`agent_command`, src/worktree/config.rs) conceptually but lands in
  the new src/park.rs, keeping the worktree spawn path pure.
- codex accepts thread names as well as UUIDs, so its id pattern is the
  loosest — but still refuses `-`-leading tokens (those are flags).

## Decisions (settled)

- **todos.txt is untouched.** Same format, same parser, same
  hand-editability. Recognized resume commands are a *view* over item
  text, not a new item type.
- **Sidecar for sessions:** `parked.json` (+ `parked.lock`) in the same
  state directory, keyed by (agent, session_id) with upsert-dedupe and
  a 50-entry cap. JSON via serde_json (already a dependency); same
  flock + atomic-rename + `O_NOFOLLOW`/`0600` policy as todo.rs.
- **Enter stays Google-Tasks.** Enter still edits the selected todo;
  `r` is the actor for resume. Recognition is signaled by a dim `↻`
  glyph on recognized rows, not by changing key semantics.
- **Resume launches a new window in the sidebar's own tmux session** —
  the same session spawn uses, resolved from the sidebar's
  `TMUX_PANE`. cwd comes from data, never guessed as `$HOME`.
- **Parked entries persist until deleted.** A failed resume (stale or
  compacted session id) must not silently destroy the pointer; the
  agent's error is visible in the launched window and the user deletes
  the entry with `d`.
- **Dispatch never interrupts.** The send-to-pane picker lists only
  idle panes (or waiting panes whose reason is not a permission
  prompt), and re-queries the target pane immediately before sending to
  close the 1s-staleness race.
- **Prompts ride the launch command line,** single-quote-escaped — not
  a send-after-start timing game.
- **Auto-park is opt-in** (`@sidebar_auto_park`, default off). Manual
  `p` is the primary flow; auto-park on SessionEnd is the convenience
  layer.
- **The CLI is the agent-write primitive.** No new hook registrations;
  anything that can exec the binary can add a todo. Claude slash
  commands and hooks call it; docs show the snippet.
- **Both new keys respect existing maps.** `p` is free under Panes
  focus (only Ctrl-p is bound); `y`, `r`, `s`, `m`, `S` are free under
  Todos focus.

## Out of scope

- Restructuring todos.txt: priorities, due dates, tags, nesting,
  stable per-item ids (the index-drift limitation is documented and
  unchanged).
- Bridging Claude Code's TodoWrite/task lists into the panel.
- Sync across machines; per-repo or per-session todo lists.
- Notifications for agent-added todos (the existing notification path
  is pane-scoped; todos are global — revisit later).
- agy resume support (unknown grammar — recognizer and launcher simply
  decline until it is known).
- `opencode --fork`, `codex fork`, claude `--fork-session`: no
  forking; resumes continue the original session.

## Feature 1 — Yank (`y`)

Extract the clipboard shims from src/state/notices.rs
(`copy_via_tmux_buffer`, `copy_via_os_clipboard`) into a new
src/clipboard.rs with one entry point:

```rust
pub(crate) fn copy_to_clipboard(text: &str) -> bool
```

Same policy as today: `tmux set-buffer -w` (fallback plain
`set-buffer`) on all platforms; `arboard` additionally on non-Linux.
notices.rs calls the shared helper — behavior and snapshots unchanged.

Under Todos focus, `y` copies the selected item's text (checkbox
stripped) and flashes `copied`. Under the Parked view, `y` copies the
*rebuilt resume command* for the selected entry (useful for pasting
into scripts). No selection → no-op.

## Feature 2 — todo CLI subcommand

New src/cli/todo.rs, one dispatch arm in src/cli/mod.rs
(`"todo" => todo::cmd_todo(rest)`). Follows the house pattern
(src/cli/spawn.rs): `usage` → exit 2, runtime failure → exit 1 with a
stderr line, success → exit 0. No `TMUX_PANE` requirement — the list
is per-user, not per-pane.

| command | behavior |
| --- | --- |
| `todo add <text...>` | join args with spaces, trim, reject empty (usage). Pushes one open item. Silent on success. |
| `todo list` | prints `1. [ ] text` per line, 1-based, file order. |
| `todo done <n>` | marks item n done (n matches `todo list` output). |
| `todo open <n>` | marks item n open. |
| `todo rm <n>` | deletes item n. |

- All mutations go through `todo::update` (already locked + atomic).
- Indexing is positional against the file *at the time of the call*;
  concurrent edits between a `list` and a `done` can shift numbers.
  Documented; acceptable for a hand-scale list.
- Sidebar pickup is free: the stamp poll reloads within a tick. No
  signaling in v1.
- Note the dispatch gotcha: an unrecognized first arg falls through to
  launching the TUI, so `"todo"` must match exactly.
- README gets a snippet showing a Claude Code slash command / hook
  invoking `tmux-agent-sidebar todo add "..."` — this is how agents
  discover the capability.

## Feature 3 — Resume-shaped todos (`r`)

### Recognizer

A pure function over the trimmed text after the checkbox:

```
recognize_resume(text) -> Option<ResumeRef { agent, id, cwd_hint: Option<PathBuf> }>
```

- The first whitespace-delimited token must be an agent binary name
  (`claude`, `codex`, `opencode`, `agy`, `antigravity` — the last two
  canonicalize to antigravity, which then declines).
- The resume token and id must match the per-agent grammar in the
  table above. `codex resume` is a subcommand (token 2); the others
  are flags anywhere in the remaining tokens, with the id as the next
  token. A missing or malformed id means *not recognized* — no
  half-matches.
- Optional cwd hint: a trailing `@ <path>` suffix — the last occurrence
  of ` @ ` whose suffix contains no whitespace and starts with `~`,
  `/`, `.`, or `$`. `~` expands against `$HOME`. This is a documented
  text convention, still a plain hand-editable todo line:
  `[ ] claude --resume f4ac0121-… @ ~/Projects/wisy-cloud`.

### cwd resolution

`r` needs a working directory; resolution order:

1. The item's `@ <path>` hint (explicit user intent wins).
2. parked.json — the sidebar's own freshest record, matched by
   (agent, session_id).
3. The agent's own store probe (below).
4. Fail: flash `no cwd for <id> — append "@ /path" or park the pane`.
   Never silently launch in `$HOME`.

Store probes (new, src/park.rs; each takes a home-dir parameter for
testability and uses the `sqlite3 -json -readonly` probe pattern from
src/session.rs):

- claude: scan `~/.claude/sessions/*.json` (skip the `.key` files that
  live alongside), match `.sessionId`, read `.cwd`.
- codex: `~/.codex/state_5.sqlite`, `SELECT id, cwd FROM threads WHERE
  id = ?`. (Fallback `session_index.jsonl` has no cwd — skip.)
- opencode: `~/.local/share/opencode/opencode.db`, `SELECT id,
  directory FROM session_v2 WHERE id = ?` (probe a plain `session`
  table too — the schema name varies by version).
- hermes: `~/.hermes/state.db` `sessions` — probe for a cwd-like
  column at implementation time; skip if absent.

### Launch

On `r` with a recognized item:

1. Resolve cwd. If it required a store probe, the whole
   resolve-and-launch runs in the popup-op worker thread (the spawn
   modal's channel pattern, src/state/popup.rs:487-515) so a wedged
   sqlite3 probe (5s deadline, same bound as session.rs) cannot stall
   the UI. The result lands as a flash banner: `resumed <agent> <id8>`
   or the failure text.
2. Build the command with `resume_command(agent, id)` — never the item
   text.
3. `tmux new-window -t <sidebar session> -c <cwd> -n <slug>` — window
   name from `slugify` of the item text (src/worktree/slug.rs),
   fallback `resume`. Duplicate window names are fine (tmux keys
   windows by id).
4. Stamp window provenance: `@agent-sidebar-resumed=<session_id>`,
   `@agent-sidebar-resume-agent=<agent>` (mirrors the spawn markers,
   src/worktree/markers.rs) so a future `x`-style flow can recognize
   these windows.
5. `send-keys -l <command>` then a separate `Enter` — the established
   two-call pattern (src/tmux/commands.rs:177-180).

Unrecognized item + `r` → flash `not a resume command`. No state
changes either way; the todo item is never modified or auto-checked.

## Feature 4 — Parked sessions

### Storage

src/park.rs. `parked.json` in the state directory:

```rust
pub struct ParkedSession {
    pub agent: String,          // canonical label (claude, codex, …)
    pub session_id: String,     // the agent's own id
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub title: Option<String>,
    pub prompt: Option<String>, // last prompt, for context at a glance
    pub parked_at: u64,         // epoch seconds
    pub origin: ParkOrigin,     // Manual | Auto
}
```

- Keyed by (agent, session_id): re-parking upserts in place, so a
  session cannot accumulate duplicate rows.
- Writes: serde_json pretty, flock on parked.lock, temp file
  `O_NOFOLLOW` `0600`, atomic rename — the todo.rs recipe.
- `MAX_PARKED = 50`: inserting past the cap drops the oldest by
  parked_at. A const, not an option.

### Park action

`p` under Panes focus parks the selected pane:

- Requires `PaneInfo.session_id` — without it, flash `no session id
  yet (agent has not reported in)` and do nothing.
- Captures from the already-queried PaneInfo: canonical agent label,
  session_id, path (`@pane_cwd`/launch cwd), `@pane_worktree_branch`,
  title (hook-provided `@pane_session_title` or the session-name map
  from src/session.rs), and `@pane_prompt`.
- Writes the sidecar and flashes `parked <title>`.

This is the durable version of the flag (`f`) workflow: flags die with
the pane by design; parked sessions survive it.

### Auto-park on SessionEnd

- New options: live `@sidebar_auto_park` (1/0) seeded by
  `@sidebar_default_auto_park` (default off), following the
  `@sidebar_todo_visible` seeding pattern.
- In `handlers::on_session_end`, *before* pane options are cleared
  (capture-then-wipe ordering is the whole trick): if the option is on
  and the payload carries a session_id, snapshot cwd (payload, else
  `@pane_cwd`), title, and prompt, and write a Parked-origin-Auto
  entry.
- Off by default: agents restart often, and an unpruned auto-feed
  would be noise. With the cap + dedupe it stays bounded even when on.

### Panel sub-view

TodoState gains a per-instance `view: TodoView { Todos, Parked }`
(like selection — not shared across sidebars). `s` under Todos focus
toggles.

- Header: `─ parked · N` (accent + bold while focused, same contract
  as the todos header).
- Rows: `◈ <title>` with a dim ` <agent> · <relative time>` suffix;
  entries without a title show `<agent> <id8>`. Text wraps with the
  same 4-column continuation indent as todos. No add line.
- Keys in the Parked view: `j`/`k`/`Up`/`Down`/PageUp/PageDown
  navigate (shared), `r` and `Enter` resume, `d`/`x` delete, `y`
  yanks the rebuilt resume command, `s`/`t`/`Esc`/`q`/globals behave
  as in Todos view. `a`/`e`/`Space`/`D`/`H` are inert here.
- The parked view lives in the same todo_area; height rules and the
  10-row cap are unchanged.

### Resume from Parked

`r`/`Enter` on an entry: cwd is already stored, so this path is
launch-only (steps 2-5 of the recognizer launch, plus the store probe
when `parked.cwd` is None — same worker). Success keeps the entry and
flashes `resumed <title>`; failure flashes the error and keeps the
entry. The relaunched pane re-registers via hooks and appears in the
agent list on its own; nothing links it back programmatically.

## Feature 5 — Dispatch

### Send to a running agent (`m`)

Under Todos focus, `m` opens a pane-picker popup:

- Lists the sidebar's own agent rows filtered to eligible targets:
  status `idle`, or `waiting` with a non-permission reason (reuse
  `is_permission_wait_reason`, src/cli/hook/handlers/status_priority.rs
  — typing into a permission dialog would corrupt it).
- `j`/`k` navigate, `Enter` sends, `Esc` cancels.
- Send: re-query the target pane (one scoped `list-panes`) to close
  the 1s-staleness race; if it is no longer eligible, flash `pane
  busy`. Otherwise `send-keys -l <todo text>` + `Enter` — the text
  lands in the agent TUI's input box, not a shell.
- Flash `sent to <title>`. The item is *not* auto-checked (open
  question below).

### Spawn with prompt (`S`)

Under Todos focus, `S` opens the spawn modal prefilled: Task seeded
with a truncated item text (it is slugified anyway) and the full text
held as the prompt.

- `SpawnRequest` (src/worktree/flow.rs) gains
  `prompt: Option<String>`.
- The command builder gains the prompt: `claude [flags] '<prompt>'`
  and `codex [flags] '<prompt>'` take positional prompts;
  `opencode --prompt '<prompt>'` takes the flag (verified);
  antigravity takes none — spawn bare and flash a note.
- Quoting: one helper, `shell_single_quote` (wrap in `'`, replace `'`
  with `'\''`). The command is still delivered via literal send-keys.
- Repo picker: the modal currently pins one repo from its invoking
  context. From the todo panel there is none, so the modal gains a
  Repo field (new `SpawnField::Repo`) cycling the deduped repo roots
  present in the current pane list; if there are none, flash an error.
  Opening from Panes focus pins the selected row's repo as today.

## Shared infrastructure

- src/clipboard.rs (new): the extracted copy shims; AGENTS.md already
  names this module.
- Popup-op worker: reused for the recognizer path's probe+launch; the
  parked path (cwd known) launches inline like other tmux writes.
- Flash banners: all feedback rides the existing 4s banner
  (render_flash_banner_into, src/ui/panes.rs) — no new UI chrome.
- Help popup (src/ui/help.rs): rows for `p` (park), and the Todos
  group gains `y`, `r`, `s`, `m`, `S`; the Parked view lists its keys
  in its header hint if width allows.

## Options

```rust
/// Live auto-park flag (1/0), shared across sidebars. Default off.
pub const SIDEBAR_AUTO_PARK: &str = "@sidebar_auto_park";
/// Landing value for a freshly started tmux server (on/off, default off).
pub const SIDEBAR_DEFAULT_AUTO_PARK: &str = "@sidebar_default_auto_park";
```

Seeded in agent-sidebar.conf and documented in the README configuration
table and website reference/tmux-options.md. Everything else in this
spec is a const or per-instance state, deliberately.

## Testing

- Unit: the recognizer grammar (per-agent tables, including the
  mixed-case `ses_` id and codex thread names; `-`-leading tokens
  rejected; `@ path` extraction), `shell_single_quote`,
  `resume_command` per agent, park store round-trip / upsert-dedupe /
  cap / concurrent writers, cwd probes against fixture home dirs,
  CLI exit codes and `todo list` numbering.
- UI (insta snapshots, per the repo's UI test rule): Parked view rows,
  the `↻` recognition glyph, the dispatch picker, the spawn modal with
  Repo field + prefilled prompt, the `copied` flash. With the panel
  hidden, every existing snapshot stays byte-identical.
- Env seams: cwd probes and the launch path take a home-dir / env
  parameter (the SpawnEnv pattern) so tests never touch real agent
  stores or tmux.
- Manual smoke: resume a real claude session from a pasted todo; park
  a pane, kill its window, resume from Parked; `todo add` from a
  script appearing in ≤ 1s; auto-park firing on a Claude exit.

## Phases

1. **Yank.** Extract src/clipboard.rs; `y` in Todos view; flash.
   Trivial, immediate relief.
2. **CLI.** src/cli/todo.rs + dispatch arm + README snippet.
3. **Recognizer.** Grammar + cwd probes + worker launch + `r` + `↻`
   glyph. This alone retires the copy/paste workflow.
4. **Parked sessions.** src/park.rs, `p`, Parked sub-view,
   resume_command, auto-park option.
5. **Dispatch.** Pane picker + `m`; SpawnRequest.prompt + quoting +
   Repo field + `S`.

Each phase ships alone; 1-2 are one-liners by comparison, 3 is the
user-facing core, 4 is the durable system, 5 is the most novel.

## Risks and open questions

- **agy resume grammar is unknown.** The recognizer declines
  antigravity until someone pins down the flag; the Parked view still
  records agy sessions (title/cwd context is useful) but `r` on one
  flashes `resume for <agent> not supported yet`.
- **sqlite schema drift.** codex's `threads` table and opencode's
  `session`/`session_v2` naming vary across versions; probes try known
  shapes and degrade to "no cwd" (the `@ path` suffix is the manual
  escape hatch).
- **Stale session ids.** Compacted or deleted sessions launch a window
  in which the agent itself prints the error. Acceptable and visible;
  the pointer survives by design.
- **Dispatch staleness.** Pane rows are up to 1s old; the pre-send
  re-query narrows the race to milliseconds. A mid-turn send remains
  possible if the agent starts a turn in that window — the flash
  cannot undo it, but the text is visible in the transcript either way.
- **`codex` positional prompt** should be re-verified against the
  installed version at implementation time (the resume-form positional
  prompt is documented; the fresh-spawn form is assumed to match).
- **Auto-park noise** when agents churn: mitigated by default-off,
  dedupe, and the 50-entry cap. If it still annoys, a per-agent
  allowlist is the follow-up.
- **Two sidebars, two views.** The Todos/Parked sub-view is
  per-instance, so instances can disagree about which view is shown
  while sharing the same data. Matches how selection already behaves.
- **parked.json is less hand-editable** than todos.txt. Deliberate:
  it is machine-managed (cap-pruned, deduped) and its data is
  mechanical; todos.txt remains the human surface.
- **Key discoverability.** `r` on an unrecognized item is inert; the
  `↻` glyph plus a help-popup row must carry the discoverability load,
  same bet the todo panel made on `t`.

## Success criteria

- cargo test, cargo clippy, and cargo fmt --check pass; hidden-panel
  snapshots are byte-identical.
- `y` copies the selected todo text; `[copied]`-style flash confirms.
- `tmux-agent-sidebar todo add "note"` appears in every open sidebar
  within ~1s; `todo list` / `done` / `rm` round-trip against the same
  file the TUI edits.
- A todo line `claude --resume <uuid>` shows the glyph and `r`
  opens a new window in the resolved cwd running the rebuilt command;
  the same works for `codex resume <id>` and `opencode -s <id>` with
  id case preserved.
- `p` on a pane with a session id parks it; the Parked view lists it
  with title and agent; `r` there relaunches it after the original
  window is killed; `d` removes the entry.
- With `@sidebar_auto_park on`, a Claude session ending parks itself
  before its pane options are wiped.
- `m` sends a todo's text to an idle agent pane and flashes the
  target; permission-waiting panes are not offered.
- `S` spawns a worktree agent whose launch line embeds the quoted
  todo prompt; agy spawns without one and says so.
