// OpenCode 2 plugin bridge for tmux-agent-sidebar.
//
// OpenCode 2 plugins default-export a definition with an `id` and a
// `setup(ctx)` that registers hooks on the context domains (a named export
// returning a hooks object is OpenCode 1 and is rejected at load time).
// Mapping from the OpenCode 1 bridge:
//
//   chat.message        → ctx.session.hook("prompt")      (event.prompt.text)
//   event bus           → ctx.event.subscribe()           ({ type, data })
//   tool.execute.after  → ctx.tool.hook("execute.after")
//
// Bus payloads are flat: every event is `{ type, data: {...} }` and the
// per-type fields live on `data` (sessionID, title, error, ...). The
// session lifecycle events nest the session record under `data.info`
// (2.0.x: `{ sessionID, info: { projectID, directory, parentID, title, ... } }`)
// — reading those fields flat on `data` silently yielded undefined and
// disabled the project gate. Turn lifecycle in 2.0.x is
// `session.execution.started` / `succeeded` / `failed` / `interrupted` —
// the v1-era `session.idle` / `session.status` events are still understood
// (schema keeps them) but a 2.0.27 server never emits them, so
// `session.idle` alone left panes running forever. Title changes arrive as
// `session.renamed` (2.0.32+: flat `data: { sessionID, title }`; 2.0.32 no
// longer emits `session.updated` at all — older daemons titled via
// `session.updated` with `data.info.title`, still handled). Blocking asks
// changed shape in 2.1.x: the dedicated `question.asked` / `question.replied`
// / `question.rejected` events are gone — the `question` tool now asks via
// the form surface (`form.created` with the request nested under `data.form`,
// `metadata.kind: "question"`), MCP elicitations ride the same events
// (`kind: "mcp"`, owned by the non-session `"global"` sentinel), and the
// wait clears via `form.replied` / `form.cancelled`. The legacy `question.*`
// events are still handled so 2.0.x daemons keep working.
//
// The bus is server-global: a plugin instance receives events for every
// session on the service, including other projects. `session.created`
// carries the new session's `data.info.projectID` / `data.info.directory`,
// which gates adoption and forwarding; lifecycle events for sessions we
// have not seen created are attributed with a one-time `session.get`
// lookup (cached per sessionID), so a foreign project's turn can never
// touch the pane — not even after a service restart mid-session.
// A shared projectID is not ownership: the project spans every worktree of
// the repo, so sibling-worktree TUIs all pass the project check. The
// session's own `directory` decides — an instance forwards only sessions
// living in its directory (or a subdirectory of it, for opencode-managed
// worktrees under the launch dir). Without that gate every sibling
// instance forwarded each foreign session's events with its own cwd, and
// resolution's single-path-match shortcut committed the foreign session's
// identity onto that instance's pane — the newest session took over every
// opencode row's title, binding, and run clock.
//
// Pane resolution: unlike Claude Code / Codex, OpenCode 2 does NOT run its
// plugins inside the tmux pane that hosts the session — plugins execute in
// a shared service daemon (`opencode serve --service`) whose inherited
// TMUX_PANE is either unset, stale (its pane may have closed), or points
// at whichever pane happened to boot the daemon. Writing to that env pane
// silently fails ("no such pane") or lands on the wrong row, so every hook
// payload carries `cwd` (the plugin instance's location directory) and
// `session_id`, and `hook()` resolves the real pane per event and injects
// it into the spawned hook's TMUX_PANE. Resolution asks tmux itself:
// panes currently running `opencode` whose working directory matches the
// session's directory (nearest ancestor wins), disambiguated by:
//   1. the @pane_session_id binding this bridge wrote earlier, unless the
//      pane's own title contradicts it (see 2) — a restart, a session
//      switch, or an older buggy bridge can leave a binding naming a
//      session that pane no longer hosts;
//   2. the pane title — the opencode TUI stamps its active session's title
//      into the terminal title ("OC | <title>"), which is per-pane ground
//      truth from the process actually hosting the session; when it names
//      this session it wins over a contradicting binding, and when it
//      contradicts a binding that binding is treated as stale. A stamp the
//      pane's own binding already explains is not evidence — auto-titles
//      share long prefixes, and a prefix hit must never re-point another
//      pane's binding;
//   3. an unbound candidate, so concurrent same-directory sessions each
//      claim their own pane instead of one starving the other;
//   4. a provably-stale binding — the bound session's title no longer
//      matches the pane's current title (the TUI was restarted or switched
//      sessions, and nothing else ever clears @pane_session_id, so without
//      this the pane could never be reclaimed);
//   5. a last-resort first match, which is a GUESS: the spawned hook is
//      flagged TMUX_AGENT_SIDEBAR_BIND_SESSION=0 so the CLI does not record
//      the binding on a pane that may belong to another live session.
// A valid inherited TMUX_PANE is only used when nothing better matches —
// and only when it is a live pane running opencode, never committed — so
// in-pane setups (OpenCode 1 style, or a local server) keep working
// unchanged.

import { spawn } from "node:child_process";
import { existsSync, realpathSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const resolveHookScript = () => {
  let dir = dirname(fileURLToPath(import.meta.url));
  for (let i = 0; i < 4; i += 1) {
    const candidate = resolve(dir, "hook.sh");
    if (existsSync(candidate)) {
      return candidate;
    }
    const parent = dirname(dir);
    if (parent === dir) {
      break;
    }
    dir = parent;
  }
  return null;
};

const HOOK_COMMAND = (() => {
  const hookScript = resolveHookScript();
  return hookScript
    ? { cmd: "bash", prefix: [hookScript, "opencode"] }
    : { cmd: "tmux-agent-sidebar", prefix: ["hook", "opencode"] };
})();

// Hook dispatch is serialized in program order: hook callbacks fire without
// awaiting the spawned process, so fire-and-forget spawns would race their
// tmux writes — e.g. a first prompt's session-start (idle + clock clear) and
// user-prompt-submit (running + clock stamp) landing interleaved, or a
// queued follow-up's prompt stamp landing after the previous turn's stop
// cleared it. Each link resolves when the spawned hook exits, and the WHOLE
// link body — session RPCs included — races a timeout so a wedged spawn or
// a hung `ctx.session.get` cannot stall the chain for the daemon's
// lifetime (audit 2026-11-25 M-bridge-1); a spawn that outlives its own
// timeout is killed so its late tmux writes cannot race the next link
// (M-bridge-3). OpenCode still gets its fire-and-forget dispatch semantics
// — the chain drains in the background.
let hookChain = Promise.resolve();

// Bus subscription filter (opencode 2.2.0+): only the event types the
// bridge handles, dropped at the bus so high-volume deltas (message parts,
// tool input streams) never reach the pump on a busy daemon. Every type
// here exists in the daemon's server manifest. `session.updated` and the
// legacy `question.*` events are deliberately absent: they are not in the
// 2.2+ server manifest and no longer exist as events there, and daemons
// old enough to emit them predate the types option entirely — the
// unfiltered delivery on those keeps the legacy handlers below working.
const BUS_EVENT_TYPES = [
  "session.created",
  "session.renamed",
  "session.status",
  "session.idle",
  "session.execution.started",
  "session.execution.succeeded",
  "session.execution.failed",
  "session.execution.interrupted",
  "permission.asked",
  "permission.replied",
  "form.created",
  "form.replied",
  "form.cancelled",
];

// One `tmux list-panes -a` snapshot, tab-separated so fields parse cleanly:
// pane_id, pane_current_command, pane_current_path, @pane_session_id,
// @pane_status, pane_title. Resolves with "" when tmux is unavailable;
// never rejects. The status field feeds the staleness sweep's guarded stop
// (a stale turn may only stop a pane that still shows running).
function listPanes() {
  return new Promise((resolvePromise) => {
    let settled = false;
    const finish = (value) => {
      if (settled) return;
      settled = true;
      resolvePromise(value);
    };
    try {
      const child = spawn(
        "tmux",
        [
          "list-panes",
          "-a",
          "-F",
          "#{pane_id}\t#{pane_current_command}\t#{pane_current_path}\t#{@pane_session_id}\t#{@pane_status}\t#{pane_title}",
        ],
        { stdio: ["ignore", "pipe", "ignore"] },
      );
      let out = "";
      child.stdout.on("data", (chunk) => {
        out += chunk;
      });
      child.on("error", () => finish(""));
      // Resolve on `close`, not `exit`: exit can fire before the pipe's
      // buffered data is delivered, truncating the pane table under load
      // and sending resolution down fallback paths (audit 2026-11-25
      // M-bridge-2). close fires after the stdio streams have flushed.
      child.on("close", () => finish(out));
      // A wedged tmux client must not stall the serialized hook chain.
      setTimeout(() => {
        try {
          child.kill();
        } catch {}
        finish("");
      }, 1500).unref?.();
    } catch {
      finish("");
    }
  });
}

// Parse one list-panes record. Titles may contain tabs; everything after
// the fifth field is the title.
function parsePaneLine(line) {
  const [id, command, path, boundSession, status, ...rest] = line.split("\t");
  return {
    id,
    command,
    path: path ?? "",
    sessionId: boundSession ?? "",
    status: status ?? "",
    title: rest.join("\t"),
  };
}

// Map a hook payload (session location + session id) to the tmux pane that
// hosts it. Preference order:
//   1. panes running `opencode` whose working directory equals the session
//      directory (or is its nearest ancestor — sessions can run inside
//      worktrees or subdirectories of the launched project), most specific
//      first;
//   2. when that match is ambiguous, the candidate whose @pane_session_id
//      equals the payload's session (a binding an earlier event wrote) —
//      or, when no candidate is bound to it, an unbound candidate, so
//      concurrent same-directory sessions each claim their own pane
//      instead of one starving the other;
//   3. when nothing matches, the inherited TMUX_PANE — but only if it is a
//      live pane running opencode, and never committed (the shared daemon's
//      value is often stale or points at an unrelated pane).
// The cache only collapses bursts of events for the same session; every
// miss re-queries so a reopened or moved pane self-heals. A committed
// resolution is about to write a session binding: cached resolutions
// pointing at that same pane are dropped when the spawn goes out, so a
// second same-directory session cannot ride a pre-write cache entry into
// double-claiming the pane.
// `cwd|session_id` → { pane, committed, at }. A map, not a single slot:
// concurrent sessions in one directory interleave events, and a single
// entry would thrash (a list-panes spawn per event). Insertion-ordered and
// capped, with the oldest entries evicted. `committed` is whether the CLI
// may record the session binding on the resolved pane (see below).
const paneCache = new Map();
const PANE_CACHE_MS = 2000;
const PANE_CACHE_MAX = 64;

// Set by setup(): async sessionID → the session's current title ("" when
// unknown). Backed by the sessionProjects cache and a one-time server
// lookup, so repeated resolutions never re-query.
let resolveSessionTitle = null;

// Set by setup(): maps a sessionID to the id the pane hooks should see.
// Child (subagent) sessions share the parent pane, and events forwarded
// with the child's own id would rebind `@pane_session_id` to the child on
// every forwarded permission ask / question / tool result — clearing
// `@pane_session_title` on each flip (a mismatched rebind drops it), with
// the parent's next event flipping the binding back and clearing the title
// again. An auto-titled session never emits another `session.renamed`, so
// the title would stay gone for the pane's lifetime. Routing child events
// under the parent's identity keeps the binding (and the title) stable.
let routeSessionId = null;

async function sessionTitleOf(sessionId) {
  if (sessionId === "" || typeof resolveSessionTitle !== "function") return "";
  try {
    return await resolveSessionTitle(sessionId);
  } catch {
    return "";
  }
}

// The opencode TUI stamps its pane title with the active session's title
// ("OC | <title>"), ellipsis-truncated to the terminal width. Strip the
// prefix and any trailing ellipsis so a truncated pane title prefix-matches
// the full session title. An unparseable title simply never matches.
function paneTitlePart(rawTitle) {
  let title = (rawTitle ?? "").trim();
  if (title.startsWith("OC | ")) {
    title = title.slice("OC | ".length);
  }
  return title.replace(/\u2026$/, "").replace(/\.\.\.$/, "").trim();
}

function titlesMatch(paneTitle, sessionTitle) {
  const pane = paneTitlePart(paneTitle);
  const session = (sessionTitle ?? "").trim();
  if (pane === "" || session === "") return false;
  if (pane === session) return true;
  // Prefix matches need enough characters to be meaningful — a truncated
  // pane title is a prefix of the full session title. Claiming sites add
  // the explained-stamp check on top (see stampExplainedByOwnBinding): a
  // prefix hit alone must never re-point another pane's binding.
  return pane.length >= 8 && session.startsWith(pane);
}

// A pane whose stamp is already explained by its own binding — the TUI is
// displaying its bound session's title — is not title evidence for another
// session, however long the shared prefix. Auto-titled sessions in one
// directory routinely share heads ("Refactor the auth flow" / "Refactor
// the auth flow in the API"); treating such a stamp as evidence routed the
// second session onto the first pane and re-pointed its binding, leaving
// two rows animating with each other's state until the titles diverged.
async function stampExplainedByOwnBinding(pane, sessionId) {
  if (pane.sessionId === "" || pane.sessionId === sessionId) return false;
  const boundTitle = await sessionTitleOf(pane.sessionId);
  return boundTitle !== "" && titlesMatch(pane.title, boundTitle);
}

async function resolvePaneId(cwd, sessionId) {
  if (typeof cwd !== "string" || cwd === "") {
    return { pane: "", committed: true };
  }
  const key = `${cwd}|${sessionId}`;
  const now = Date.now();
  const cached = paneCache.get(key);
  if (cached && now - cached.at < PANE_CACHE_MS) {
    return cached;
  }
  const out = await listPanes();
  const panes = out
    .split("\n")
    .filter((line) => line !== "")
    .map(parsePaneLine);
  // opencode's bundled bun binary keeps an `.exe` suffix on every platform
  // (`@opencode/cli/bin/opencode.exe`), and tmux reports the pane's
  // foreground process name — so on macOS a live TUI pane reads
  // `opencode.exe` while Linux wrappers surface plain `opencode`. Accept
  // both spellings or the pane is unresolvable on macOS and every hook
  // falls back to the (stale) daemon TMUX_PANE. Override with
  // OPENCODE_PANE_COMMAND.
  const agent = process.env?.OPENCODE_PANE_COMMAND ?? "opencode";
  const agentNames = new Set([agent, `${agent}.exe`]);
  const candidates = panes.filter((p) =>
    agentNames.has((p.command ?? "").replace(/\.exe$/i, "")),
  );

  let realCwd = "";
  try {
    realCwd = realpathSync(cwd);
  } catch {}

  const pathMatches = candidates
    .filter((p) => {
      const panePath = p.path ?? "";
      if (panePath === "") return false;
      return (
        panePath === cwd ||
        (realCwd !== "" && panePath === realCwd) ||
        cwd.startsWith(`${panePath}/`) ||
        (realCwd !== "" && realCwd.startsWith(`${panePath}/`))
      );
    })
    // Nearest ancestor first.
    .sort((a, b) => (b.path?.length ?? 0) - (a.path?.length ?? 0));

  // `committed` marks resolutions the spawned hook may record: a bound
  // pane, a title-matched pane, a free claim, or a provably-stale reclaim.
  // The last-resort first match is a guess — the pane may belong to another
  // live session — and recording the binding there would permanently
  // misroute the session that owns it (its next event could never find a
  // binding of its own). Guesses are flagged so the CLI skips the write.
  // The inherited TMUX_PANE fallback is likewise never committed.
  let chosen = "";
  let committed = true;
  if (pathMatches.length === 1) {
    // A single path match is normally THE pane. But when that pane's own
    // title stamp proves it is still hosting its bound session — a session
    // other than this event's — the event is a foreign one (a stale cache
    // entry, or a session the ownership gate could not attribute). Route it
    // there so it still lands somewhere, but uncommitted: recording the
    // binding would re-point the pane onto the foreign session and
    // permanently misroute the session that actually lives here. The pane's
    // own session restores the binding on its next event (its stamp no
    // longer explains the foreign one), or the reclaim paths below take the
    // pane once the bound session is provably gone.
    const only = pathMatches[0];
    if (await stampExplainedByOwnBinding(only, sessionId)) {
      chosen = only.id;
      committed = false;
    } else {
      chosen = only.id;
    }
  } else if (pathMatches.length > 1) {
    // Title evidence first: the TUI actually hosting this session stamps
    // its title into the pane title. Sessions are titled lazily (auto-title
    // after the first exchange), so this is often absent for a brand-new
    // session's first event.
    const myTitle = await sessionTitleOf(sessionId);
    // Title evidence must be near-exact and not already explained by the
    // pane's own binding (see stampExplainedByOwnBinding). An exact stamp
    // wins outright; a truncated one (the TUI ellipsis-truncates the
    // session title at the terminal width) is the only loose match
    // accepted — and a stamp the pane's own binding accounts for is never
    // evidence, however long the shared prefix.
    let titled;
    if (myTitle !== "") {
      for (const p of pathMatches) {
        if (
          paneTitlePart(p.title) === myTitle &&
          !(await stampExplainedByOwnBinding(p, sessionId))
        ) {
          titled = p;
          break;
        }
      }
      if (!titled) {
        for (const p of pathMatches) {
          if (!titlesMatch(p.title, myTitle)) continue;
          if (await stampExplainedByOwnBinding(p, sessionId)) continue;
          titled = p;
          break;
        }
      }
    }
    const bound =
      sessionId !== ""
        ? pathMatches.find((p) => p.sessionId === sessionId)
        : undefined;

    if (titled && (!bound || bound.id === titled.id)) {
      // No binding, or the binding agrees with the pane's own title.
      chosen = titled.id;
    } else if (bound && !titled) {
      // Bound pane with no contradicting title evidence (session untitled,
      // or no pane title matches): the binding is the best signal there is.
      chosen = bound.id;
    } else if (titled) {
      // A binding exists but points at a pane whose TUI displays a
      // different session — a restart or session switch (or a binding
      // written by an older, buggy bridge) — while another pane's title
      // names this one. The pane title is per-pane ground truth, so it
      // wins; the binding rewrite below re-points it. But never when the
      // bound pane's own stamp still confirms the binding: a prefix hit
      // on another pane (two auto-titles sharing a head) must not re-point
      // a binding the bound pane's TUI itself displays.
      const boundPane = pathMatches.find((p) => p.id === bound.id);
      if (boundPane && titlesMatch(boundPane.title, myTitle)) {
        chosen = bound.id;
      } else {
        chosen = titled.id;
      }
    } else {
      // 1. No binding for this session yet (its first event, or the bound
      // pane closed): claim an unbound pane rather than stealing a bound
      // one. A plain first-match tie-break routed every event to one pane
      // and starved the other same-directory session (which could never
      // acquire a binding of its own). Equal-path matches are checked in
      // list-panes order; fall back to the first match when every
      // candidate is already bound.
      const free = pathMatches.find((p) => p.sessionId === "");
      if (free) {
        chosen = free.id;
      } else {
        // 2. Reclaim a provably-stale binding. Nothing ever clears
        // @pane_session_id (opencode has no session-end hook event), so a
        // pane whose TUI was restarted or switched sessions keeps the dead
        // session's binding forever — unclaimable by both the bound and
        // the free checks above, which collapsed every new same-directory
        // session onto the first pane. The pane's own title is the proof
        // of staleness: the bound session's title no longer matches what
        // its TUI displays. Sessions whose title is unknown are left
        // alone.
        for (const p of pathMatches) {
          if (p.sessionId === "" || p.title.trim() === "") continue;
          const boundTitle = await sessionTitleOf(p.sessionId);
          if (boundTitle !== "" && !titlesMatch(p.title, boundTitle)) {
            chosen = p.id;
            break;
          }
        }
      }
      // 3. Last resort: the first match, uncommitted.
      if (chosen === "") {
        chosen = pathMatches[0].id;
        committed = false;
      }
    }
  } else if (sessionId !== "") {
    const bound = candidates.filter((p) => p.sessionId === sessionId);
    if (bound.length === 1) chosen = bound[0].id;
  }

  if (
    chosen === "" &&
    typeof process.env?.TMUX_PANE === "string" &&
    process.env.TMUX_PANE !== ""
  ) {
    // The daemon's inherited TMUX_PANE is at best a guess — it may be the
    // pane that booted `opencode serve --service`, long since repurposed
    // or gone. Only accept it when it is a live pane running opencode,
    // and never let the CLI record a binding on it: an unguarded commit
    // here is the permanent-misroute case TMUX_AGENT_SIDEBAR_BIND_SESSION
    // exists to defuse. A non-opencode inherited pane (an in-pane setup's
    // shell) gets no injection at all — env passthrough keeps those
    // setups working unchanged.
    const inherited = candidates.find((p) => p.id === process.env.TMUX_PANE);
    if (inherited) {
      chosen = inherited.id;
      committed = false;
    }
  }

  paneCache.set(key, { pane: chosen, committed, at: now });
  if (paneCache.size > PANE_CACHE_MAX) {
    const oldest = paneCache.keys().next().value;
    if (oldest !== undefined) paneCache.delete(oldest);
  }
  return { pane: chosen, committed };
}

// Promise.race that never rejects: `run()` keeps running after the timeout
// wins the race, but the caller moves on. `onTimeout` fires when the
// timeout wins so the caller can clean up (kill a started spawn).
function raceTimeout(ms, onTimeout, run) {
  return new Promise((resolve) => {
    let settled = false;
    const settle = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve();
    };
    const timer = setTimeout(() => {
      if (settled) return;
      try {
        onTimeout?.();
      } catch {}
      settle();
    }, ms);
    timer.unref?.();
    run().then(settle, settle);
  });
}

const HOOK_SPAWN_TIMEOUT_MS = 3000;
// The whole link body — session RPCs included — must stay bounded
// (audit 2026-11-25 M-bridge-1); the spawn keeps its own tighter budget so
// a slow-but-working resolution does not eat into it.
const HOOK_LINK_TIMEOUT_MS = 5000;

// One hook spawn, raced against its own timeout. The child is KILLED when
// the timeout wins: a timed-out spawn left alive keeps running and its
// late tmux writes would race the next link's writes, breaking the
// serialized-ordering invariant the chain exists for (audit 2026-11-25
// M-bridge-3).
function runHookSpawn(eventName, payload, env) {
  const state = { child: null };
  let killer = null;
  const done = new Promise((resolve) => {
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      if (killer) clearTimeout(killer);
      resolve();
    };
    killer = setTimeout(() => {
      try {
        state.child?.kill();
      } catch {}
      finish();
    }, HOOK_SPAWN_TIMEOUT_MS);
    killer.unref?.();
    try {
      state.child = spawn(HOOK_COMMAND.cmd, [...HOOK_COMMAND.prefix, eventName], {
        stdio: ["pipe", "ignore", "ignore"],
        env,
      });
      state.child.on("error", () => finish());
      state.child.on("exit", () => finish());
      state.child.stdin.on("error", () => {});
      state.child.stdin.end(JSON.stringify(payload));
    } catch {
      // OpenCode should keep running even if the bridge is missing
      // or the sidebar binary is unavailable.
      finish();
    }
  });
  return {
    done,
    kill: () => {
      try {
        state.child?.kill();
      } catch {}
    },
  };
}

const hook = (eventName, payload, opts = {}) => {
  const spawnRun = { kill: null };
  hookChain = hookChain.then(() =>
    // The WHOLE link body races a timeout — not just the final spawn. A
    // hung `routeSessionId` → `lookupSessionInfo` → `ctx.session.get`
    // (or a hung title lookup inside resolvePaneId) used to wedge the
    // chain forever, stalling every subsequent opencode hook event for
    // the daemon's lifetime (audit 2026-11-25 M-bridge-1). On expiry the
    // link yields and any spawn it already started is killed, so late
    // tmux writes cannot race the next link (M-bridge-3). The race never
    // rejects, so a thrown body cannot leave the chain permanently dead.
    raceTimeout(
      HOOK_LINK_TIMEOUT_MS,
      () => spawnRun.kill?.(),
      async () => {
        // Child-session events ride under the parent's identity so they cannot
        // rebind the pane or wipe its title (see routeSessionId). Unknown ids
        // pass through unchanged.
        if (
          payload &&
          typeof payload.session_id === "string" &&
          payload.session_id !== "" &&
          typeof routeSessionId === "function"
        ) {
          payload = { ...payload, session_id: await routeSessionId(payload.session_id) };
        }
        // The daemon has no trustworthy TMUX_PANE; inject the resolved one into
        // the spawned hook's environment. With no match, inherit the inherited
        // environment unchanged so non-daemon setups behave exactly as before.
        // A guessed resolution (every candidate bound, none provably stale)
        // flags the spawn so the CLI does not record the session binding on a
        // pane that may belong to another live session. `opts.unbind` forces
        // the same flag for payloads whose session id is not a real session
        // (the form surface's "global" elicitation owner): resolution may pick
        // the pane by cwd, but the CLI must never record that id as the pane's
        // binding.
        const resolved = await resolvePaneId(payload?.cwd, payload?.session_id ?? "");
        // A committed resolution is about to write this session's binding on
        // the pane: cached resolutions captured before that write are stale
        // for every other session that resolved to the same pane — two
        // same-directory sessions would otherwise both ride the 2s cache into
        // claiming the first unbound pane. Drop them so the next event
        // re-resolves against the fresh pane table; the chain is serialized,
        // so the spawned hook — binding write included — has fully exited
        // before the next link resolves.
        if (
          resolved.committed &&
          resolved.pane !== "" &&
          typeof payload?.cwd === "string" &&
          payload.cwd !== ""
        ) {
          const ownKey = `${payload.cwd}|${payload.session_id ?? ""}`;
          for (const [key, cached] of [...paneCache]) {
            if (key !== ownKey && cached.pane === resolved.pane) {
              paneCache.delete(key);
            }
          }
        }
        const env =
          resolved.pane === ""
            ? undefined
            : {
                ...process.env,
                TMUX_PANE: resolved.pane,
                ...(resolved.committed && !opts.unbind
                  ? {}
                  : { TMUX_AGENT_SIDEBAR_BIND_SESSION: "0" }),
              };
        const run = runHookSpawn(eventName, payload, env);
        spawnRun.kill = run.kill;
        await run.done;
      },
    ),
  );
};

    const sessionIdOf = (value) => (typeof value === "string" && value ? value : "");

export default {
  id: "tmux-agent-sidebar",
  async setup(ctx) {
    const cwd = sessionIdOf(ctx?.location?.directory);

    // OpenCode runs every `task` subagent in a child session (its own
    // sessionID, `parentID` pointing at the main session) whose bus events
    // and prompt hook all reach this plugin instance. Only main-session
    // lifecycle may drive a pane's identity and adoption: a child's prompt
    // must not adopt the main slot, and a child's events must not rebind
    // `@pane_session_id` or wipe the pane's title (they ride under the
    // parent's identity — see routeSessionId). Run state is different: a
    // background subagent keeps working after the main turn's model work
    // is done, and the pane must stay lit until the last child drains
    // (H-bridge-1) — child turns are tracked in `activeChildTurns` below.
    //
    // One instance can host several concurrent main sessions (two TUIs in
    // one project share this plugin), so mains are tracked as a set and
    // turn state per session — never a single slot. A single slot made
    // concurrent sessions steal each other's events, landing one session's
    // prompt text and run clock on the other pane and starving the loser
    // from its very first event. Main sessions are learned from top-level
    // signals: a `session.created` without a parentID, or a prompt from a
    // session not known to be a child (the prompt hook also fires for
    // subagent prompts, but a background subagent can prompt between turns
    // — so adoption excludes sessions whose parentID is known, from
    // session.created or a one-time session.get lookup). `activeTurns`
    // holds the main sessions with a turn in flight: added by the turn's
    // prompt / busy / started signal, cleared by stop / error.
    const mainSessions = new Set();
    // Main sessions with a turn in flight → last-activity epoch ms. The
    // timestamp bounds stale entries (L-bridge-2): a terminal event lost
    // to a stream hiccup or a plugin reload would otherwise keep the
    // session "active" forever, forwarding child tool activity between
    // turns — the timer-ticking-forever shape the execute.after gate
    // exists to prevent.
    const activeTurns = new Map();
    // Child (subagent) sessions with a turn in flight →
    // { parent, root, at }: the direct parent session, the main session
    // the work belongs to (whose pane stays lit), and the last time the
    // child showed activity (the same staleness bound as activeTurns).
    // Fed by child session.execution.started / terminal events and —
    // authoritatively — by the experimental.idle hook's background list
    // (opencode 2.2.1+), which re-fires as each child drains.
    const activeChildTurns = new Map();
    const childSessions = new Set();
    // child sessionID → parent sessionID, from session.created and the
    // prompt-time session.get lookup.
    const parents = new Map();

    // One-time server lookups are bounded so a hung RPC cannot wedge the
    // pump or the hook chain (audit 2026-11-25 M-bridge-1).
    const LOOKUP_TIMEOUT_MS = 3000;

    // sessionID → { projectID, directory }, learned from session.created
    // and from a one-time `session.get` lookup for sessions this instance
    // never saw created (the service restarts mid-session, sessions
    // predate the plugin load). The bus is server-global, so lifecycle
    // events for sessions belonging to other projects must not drive the
    // pane.
    const sessionProjects = new Map();
    const myProjectID = ctx?.location?.project?.id ?? "";
    const myDirectory = typeof ctx?.location?.directory === "string" ? ctx.location.directory : "";
    const belongsToInstance = (info) => {
      if (!info) return true; // unknown session: keep the pre-gating behavior
      const { projectID, directory } = info;
      if (projectID && myProjectID && projectID !== myProjectID) return false;
      // A shared projectID is not ownership: it spans every worktree of the
      // repo, so sibling-worktree TUIs all pass the project check. The
      // session's own directory decides — an instance only forwards
      // sessions living in its directory (or a subdirectory of it, for
      // opencode-managed worktrees under the launch dir). Without this,
      // every sibling instance forwarded each foreign session's events
      // with its own cwd, and the single-path-match resolution committed
      // the foreign session's identity onto that instance's pane — the
      // newest session took over every opencode row's title and clock.
      if (directory && myDirectory) {
        return (
          directory === myDirectory || directory.startsWith(`${myDirectory}/`)
        );
      }
      if (projectID && myProjectID) return true; // same project, directory unknown on one side
      if (projectID || myProjectID) return false; // only one side known: cannot confirm
      return true;
    };
    const lookupSessionInfo = async (session_id) => {
      if (sessionProjects.has(session_id)) return sessionProjects.get(session_id);
      let info = null;
      let answered = false;
      try {
        // Bounded like every other RPC consumer (audit 2026-11-25
        // M-bridge-1): a hung `session.get` must not wedge the pump (this
        // gate) or the serialized hook chain (routeSessionId /
        // resolveSessionTitle) for the daemon's lifetime. A timeout is
        // treated exactly like a thrown lookup — retried on the next
        // event, never cached as a miss.
        const outcome = await Promise.race([
          ctx.session.get({ sessionID: session_id }).then(
            (value) => ({ settled: true, value }),
            () => ({ settled: false }),
          ),
          new Promise((resolve) => {
            const timer = setTimeout(() => resolve({ settled: false }), LOOKUP_TIMEOUT_MS);
            timer.unref?.();
          }),
        ]);
        if (outcome?.settled) {
          answered = true;
          const session = outcome.value;
          if (session && typeof session === "object") {
            // The session record carries `directory` flat (SessionV1.Info);
            // some shapes wrap it in `location`. Read both.
            const directory =
              typeof session.directory === "string"
                ? session.directory
                : typeof session.location?.directory === "string"
                  ? session.location.directory
                  : "";
            info = {
              projectID: typeof session.projectID === "string" ? session.projectID : "",
              directory,
              parentID: sessionIdOf(session.parentID),
              title: typeof session.title === "string" ? session.title : "",
            };
          }
        }
      } catch {}
      // Cache only answers the server actually gave: a real miss (a
      // deleted session) is cached so it is not re-queried on every
      // event, but a THROWN lookup (service restarting, transient RPC
      // failure) must retry on the next event. Caching the throw used
      // to default-allow that session for its whole life, because
      // belongsToInstance keeps the pre-gating behavior for unknown
      // sessions — one transient failure opened the default-allow
      // window permanently (audit 2026-11-16).
      if (answered) {
        sessionProjects.set(session_id, info);
      }
      return info;
    };
    // Synchronous check for already-known sessions; null = not seen yet.
    const cachedBelongs = (session_id) => {
      if (!session_id || !sessionProjects.has(session_id)) return null;
      const cached = sessionProjects.get(session_id);
      return cached ? belongsToInstance(cached) : true;
    };
    // Awaitable gate: resolves unknown sessions once via the server and
    // caches the answer, so cross-project events are dropped even when
    // this instance never observed the session's creation.
    const knownSessionBelongs = async (session_id) => {
      if (!session_id) return true;
      const cached = cachedBelongs(session_id);
      if (cached !== null) return cached;
      const info = await lookupSessionInfo(session_id);
      return info ? belongsToInstance(info) : true;
    };

    // sessionID → current title, for pane-title matching in resolvePaneId.
    // Served from the sessionProjects cache (session.created + renames via
    // session.renamed / session.updated) with a one-time session.get
    // fallback, so repeated resolutions never re-query.
    resolveSessionTitle = async (session_id) => {
      if (session_id === "") return "";
      const info = await lookupSessionInfo(session_id);
      return info?.title ?? "";
    };

    // Child sessions route under their parent's id in forwarded hook
    // payloads (see routeSessionId at module scope). A never-seen session
    // (the daemon restarted mid-turn and its session.created was missed)
    // resolves parentage once via the server; lookupSessionInfo caches the
    // answer either way.
    routeSessionId = async (session_id) => {
      const knownParent = parents.get(session_id);
      if (knownParent) return knownParent;
      if (mainSessions.has(session_id)) return session_id;
      const info = await lookupSessionInfo(session_id);
      return info?.parentID || session_id;
    };

    // The bus is server-global: every session ever created on the daemon
    // (other projects included) reaches this instance, and nothing ever
    // leaves these caches on its own. Cap them so a long-lived daemon
    // cannot grow them without bound (audit 2026-11-25 L-bridge-1);
    // entries a live turn still needs are never evicted.
    const SESSION_CACHE_MAX = 512;
    const pruneSessionCaches = () => {
      const protectedIds = new Set();
      for (const id of activeTurns.keys()) protectedIds.add(id);
      for (const [child, entry] of activeChildTurns) {
        protectedIds.add(child);
        protectedIds.add(entry.parent);
        protectedIds.add(entry.root);
      }
      const evictOldest = (collection) => {
        while (collection.size > SESSION_CACHE_MAX) {
          const oldest = collection.keys().next().value;
          if (oldest === undefined) break;
          collection.delete(oldest);
        }
      };
      if (sessionProjects.size > SESSION_CACHE_MAX) {
        for (const id of sessionProjects.keys()) {
          if (sessionProjects.size <= SESSION_CACHE_MAX) break;
          if (protectedIds.has(id)) continue;
          sessionProjects.delete(id);
        }
        evictOldest(sessionProjects);
      }
      if (mainSessions.size > SESSION_CACHE_MAX) {
        for (const id of mainSessions) {
          if (mainSessions.size <= SESSION_CACHE_MAX) break;
          if (protectedIds.has(id)) continue;
          mainSessions.delete(id);
        }
        evictOldest(mainSessions);
      }
      if (childSessions.size > SESSION_CACHE_MAX) {
        // parents is keyed by the same child ids; evict the pair together.
        for (const id of childSessions) {
          if (childSessions.size <= SESSION_CACHE_MAX) break;
          if (protectedIds.has(id)) continue;
          childSessions.delete(id);
          parents.delete(id);
        }
        evictOldest(childSessions);
        // Defensive: parents only grows paired with childSessions, but a
        // cap violation there would leak just the same.
        if (parents.size > SESSION_CACHE_MAX) evictOldest(parents);
      }
    };

    // While no main session is known every event passes through (the
    // pre-gating behavior); once one is learned, foreign sessions are
    // dropped. Concurrent mains all pass — each routes to its own pane.
    const isMainSession = (session_id) =>
      mainSessions.size === 0 ||
      (session_id !== "" && mainSessions.has(session_id));

    // ── Child-turn tracking (audit 2026-11-25 H-bridge-1) ───────────────
    //
    // A background subagent keeps working after the main turn's model work
    // is done. Its lifecycle events used to be dropped outright (the
    // !isMainSession gates), so the pane flipped idle at the main turn's
    // stop and stayed there: no tool results, no permission asks, and no
    // stop when the child finally drained. The tracker keeps the pane lit
    // for the whole family:
    //
    //   - a child's execution.started records an active child turn and
    //     forwards an activity event (the activity handler flips a
    //     non-running pane back to running and stamps a missing run clock
    //     — needed when the child starts after the main stop already
    //     landed);
    //   - the main turn's terminal event holds its stop while a child turn
    //     is active for it, and the last child's terminal event forwards
    //     the stop — only when the main turn is inactive and no sibling
    //     child remains, so a mid-turn child finishing never stops the
    //     clock under a running main turn;
    //   - the experimental.idle hook (registered below) syncs the tracker
    //     against the daemon's authoritative background list and forwards
    //     the stop when a later idle fires with the list drained. It does
    //     not fire after interruption or failure, so the terminal-event
    //     handling stays as the fallback.

    // The main session a session's work belongs to: walk the parent chain
    // to the first known main. Unknown lineage maps to the session itself.
    const rootMainOf = (session_id) => {
      let current = session_id;
      for (let i = 0; i < 8; i += 1) {
        if (mainSessions.size === 0 || mainSessions.has(current)) return current;
        const parent = parents.get(current);
        if (!parent) return current;
        current = parent;
      }
      return current;
    };

    const rootHasActiveChildren = (root) => {
      for (const entry of activeChildTurns.values()) {
        if (entry.root === root) return true;
      }
      return false;
    };

    // Refresh a turn's staleness timestamp. A live turn constantly
    // produces events (tool results, asks, replies, busy transitions), so
    // only a turn whose events were all lost ever reaches the bound.
    const touchTurn = (session_id) => {
      if (activeTurns.has(session_id)) activeTurns.set(session_id, Date.now());
      const entry = activeChildTurns.get(session_id);
      if (entry) entry.at = Date.now();
    };

    // A child turn started: record it and light the pane. The forwarded
    // session is the root main so the routed payload resolves against the
    // pane's own binding (the child never owns one).
    const trackChildTurnStart = async (child) => {
      let parent = parents.get(child);
      if (!parent) {
        // The created event was missed (daemon restart mid-turn): resolve
        // parentage once via the server; lookupSessionInfo caches it.
        const info = await lookupSessionInfo(child);
        parent = info?.parentID || "";
        if (!parent) return;
        childSessions.add(child);
        parents.set(child, parent);
        pruneSessionCaches();
      }
      const root = rootMainOf(parent);
      const existing = activeChildTurns.get(child);
      if (existing) {
        existing.at = Date.now();
        return;
      }
      activeChildTurns.set(child, { parent, root, at: Date.now() });
      hook("activity-log", {
        cwd,
        session_id: root,
        tool_name: "Agent",
        tool_input: {},
        tool_response: null,
      });
    };

    // A child turn ended: release it and forward the held stop when the
    // whole family drained — root turn inactive, no sibling child left.
    const trackChildTurnEnd = (child) => {
      const entry = activeChildTurns.get(child);
      if (!entry) return;
      activeChildTurns.delete(child);
      const root = entry.root;
      if (activeTurns.has(root) || rootHasActiveChildren(root)) return;
      hook("stop", { cwd, session_id: root, last_message: "" });
    };

    // Staleness bound for turn state (audit 2026-11-25 L-bridge-2). A turn
    // whose every event was lost (stream hiccup, plugin reload) would stay
    // active forever. The bound drops the entry; when the pane still shows
    // `running` — the missed-stop shape — the stop is forwarded so the
    // elapsed timer finally lands. A `waiting` pane is left alone: a
    // permission ask pending past the bound is live state, and a stop
    // would clobber the prompt the user is looking at.
    const TURN_STALE_MS =
      Number.parseInt(process.env?.TMUX_AGENT_SIDEBAR_TURN_STALE_MS ?? "", 10) ||
      60 * 60 * 1000;
    const stopIfPaneRunning = async (session_id) => {
      const out = await listPanes();
      const pane = out
        .split("\n")
        .filter((line) => line !== "")
        .map(parsePaneLine)
        .find((p) => p.sessionId === session_id);
      if (!pane || pane.status !== "running") return;
      hook("stop", { cwd, session_id, last_message: "" });
    };
    const sweepStaleTurns = async () => {
      const now = Date.now();
      for (const [child, entry] of [...activeChildTurns]) {
        if (now - entry.at < TURN_STALE_MS) continue;
        activeChildTurns.delete(child);
        const root = entry.root;
        if (activeTurns.has(root) || rootHasActiveChildren(root)) continue;
        await stopIfPaneRunning(root);
      }
      for (const [id, at] of [...activeTurns]) {
        if (now - at < TURN_STALE_MS) continue;
        activeTurns.delete(id);
        await stopIfPaneRunning(id);
      }
    };

    // Whether a turn is active that this session's activity belongs to:
    // the session's own tracked child turn counts (a background subagent's
    // results and asks keep flowing after the main turn's model work is
    // done — H-bridge-1), then the nearest main session's turn up the
    // parent chain; unknown lineage falls back to "any main turn is
    // active" (the pre-per-session behavior).
    const turnActiveFor = (session_id) => {
      let current = session_id;
      for (let i = 0; i < 8; i += 1) {
        if (activeChildTurns.has(current)) return true;
        if (mainSessions.has(current)) return activeTurns.has(current);
        const parent = parents.get(current);
        if (!parent) break;
        current = parent;
      }
      return activeTurns.size > 0;
    };

    // Prompts admitted in a main session start (or restart) that session's
    // turn; child-session prompts never do. The hook receives the admitted
    // prompt text directly — no parts extraction like OpenCode 1.
    await ctx.session.hook("prompt", async (event) => {
      const session_id = sessionIdOf(event?.sessionID);
      // Subagent prompts route through the same hook as user prompts
      // (task tool → subagent admission), and a background subagent can
      // prompt between turns. A session not known to be a child adopts as
      // a main; known child prompts never do. Adoption also resolves
      // parentID via the cached session.get lookup, so a subagent whose
      // created event was never seen is remembered as a child instead of
      // stealing a main slot. A session this instance knows belongs to
      // another project never adopts (the bus delivers every project's
      // prompts).
      if (session_id && !childSessions.has(session_id)) {
        const info = await lookupSessionInfo(session_id);
        if (info && info.parentID) {
          childSessions.add(session_id);
          parents.set(session_id, info.parentID);
        } else if (belongsToInstance(info)) {
          mainSessions.add(session_id);
        }
        pruneSessionCaches();
      }
      // Strict membership: unlike the bus gates (which pass everything
      // while no main is known), an admitted prompt must belong to a
      // learned main session — a known-foreign prompt is dropped here.
      if (!mainSessions.has(session_id)) {
        return;
      }
      activeTurns.set(session_id, Date.now());
      hook("user-prompt-submit", {
        cwd,
        session_id,
        prompt: typeof event?.prompt?.text === "string" ? event.prompt.text : "",
      });
    });

    // Dedicated hook for tool execution results. The event bus carries
    // tool.* deltas, but results land here: `result` holds the V2
    // `{ output, content, metadata }` shape the activity log's label
    // extractors read (`content[].text`).
    //
    // Child-session tool activity is shown while it belongs to a live
    // turn — the main session's own turn, or the child's own tracked turn
    // (H-bridge-1: a background subagent's results keep flowing after the
    // main turn's model work is done, until the child drains) — and
    // dropped between turns: the activity-log handler flips a non-running
    // pane back to running and stamps a missing run clock, so results
    // landing with nothing alive would leave the elapsed timer ticking
    // forever.
    await ctx.tool.hook("execute.after", async (event) => {
      const session_id = sessionIdOf(event?.sessionID);
      if (!(await knownSessionBelongs(session_id))) {
        return;
      }
      if (!isMainSession(session_id) && !turnActiveFor(session_id)) {
        return;
      }
      touchTurn(session_id);
      hook("activity-log", {
        cwd,
        session_id,
        tool_name: typeof event?.tool === "string" ? event.tool : "",
        tool_input: event?.input ?? {},
        tool_response: event?.status === "completed" ? event?.result ?? null : null,
      });
    });

    // Experimental idle hook (opencode 2.2.1+): fires when a session
    // finishes model work, just before it would go idle, carrying the
    // background work still running for the session or one of its
    // descendants — and re-fires as each piece drains. That list is the
    // authoritative "still working" signal for H-bridge-1: sync the child
    // tracker against it, and forward the held stop once a later idle
    // fires with the subagent entries drained. Observe only — admitting
    // input here would continue the session's execution, so the handler
    // returns nothing and never throws (a hook failure must not break the
    // session runner). The hook does not fire after interruption or
    // failure; the terminal-event handling above stays as the fallback.
    // Older daemons never trigger the hook (registering an unknown name is
    // inert there); the registration is guarded like the legacy question.*
    // path regardless.
    try {
      await ctx.session.hook("experimental.idle", async (event) => {
        try {
          const session_id = sessionIdOf(event?.sessionID);
          if (
            session_id === "" ||
            !(await knownSessionBelongs(session_id)) ||
            !isMainSession(session_id)
          ) {
            return;
          }
          const background = Array.isArray(event?.background) ? event.background : [];
          const listed = new Set();
          for (const item of background) {
            if (!item || item.kind !== "subagent") continue;
            const child = sessionIdOf(item.childSessionID);
            if (child === "") continue;
            listed.add(child);
            if (!parents.has(child)) {
              parents.set(child, sessionIdOf(item.sessionID) || session_id);
              childSessions.add(child);
            }
            const existing = activeChildTurns.get(child);
            if (existing) {
              existing.at = Date.now();
            } else {
              activeChildTurns.set(child, {
                parent: parents.get(child) || session_id,
                root: session_id,
                at: Date.now(),
              });
            }
          }
          // The list is authoritative: a tracked child it no longer
          // reports has drained, even if its terminal event was lost.
          for (const [child, entry] of [...activeChildTurns]) {
            if (entry.root === session_id && !listed.has(child)) {
              activeChildTurns.delete(child);
            }
          }
          pruneSessionCaches();
          // Every background subagent drained and the main turn is over:
          // forward the stop the terminal event held. (A second stop after
          // the child-terminal drain is harmless — the notification dedups
          // and the idle write is idempotent.) With the main turn still
          // active, its own terminal event stops the pane.
          if (
            listed.size === 0 &&
            !activeTurns.has(session_id) &&
            !rootHasActiveChildren(session_id)
          ) {
            hook("stop", { cwd, session_id, last_message: "" });
          }
        } catch {}
        return;
      });
    } catch {}

    // The bus is a plain subscription in OpenCode 2; abort it on unload.
    const controller = new AbortController();
    void (async () => {
      // The pump must never surface as an unhandled rejection: some
      // transports throw from the iterator on abort (unload), and a dead
      // pump is a daemon-log matter, not a plugin crash (audit 2026-11-25
      // L-bridge-3). Per-event failures are contained the same way — one
      // malformed event must not kill the pump.
      try {
        for await (const event of ctx.event.subscribe({
          types: BUS_EVENT_TYPES,
          signal: controller.signal,
        })) {
          if (!event || !event.type) continue;
          try {
            // Bounded cleanup pass for lost terminal events (L-bridge-2);
            // a no-op while no turn is tracked.
            if (activeTurns.size > 0 || activeChildTurns.size > 0) {
              await sweepStaleTurns();
            }
            const data = event.data ?? {};
            const session_id = sessionIdOf(data.sessionID);

            switch (event.type) {
              // Child sessions must not claim the pane's identity or adopt
              // the main-session slot. Remember them so a later prompt (a
              // background subagent prompting between turns) cannot adopt
              // the slot either. Every created session is recorded with its
              // project so later lifecycle events can be attributed; only
              // sessions belonging to this instance adopt/forward.
              case "session.created": {
                const session_id = sessionIdOf(data.sessionID);
                // The 2.0.x public bus nests the session record under `info`
                // (`{ sessionID, info: { projectID, directory, parentID, ... } }`);
                // flat fields on `data` were never present, so reading them there
                // left every session's project unknown and the project gate
                // default-allowing — foreign sessions adopted the main slot and
                // the real session's lifecycle events were dropped. Older/internal
                // shapes carry the fields flat; read both.
                const info =
                  data?.info && typeof data.info === "object" ? data.info : data;
                const parentID =
                  sessionIdOf(info?.parentID) || sessionIdOf(data.parentID);
                sessionProjects.set(session_id, {
                  projectID: typeof info?.projectID === "string" ? info.projectID : "",
                  directory:
                    typeof info?.directory === "string"
                      ? info.directory
                      : typeof info?.location?.directory === "string"
                        ? info.location.directory
                        : "",
                  parentID,
                  title: typeof info?.title === "string" ? info.title : "",
                });
                pruneSessionCaches();
                if (parentID) {
                  if (session_id) {
                    childSessions.add(session_id);
                    parents.set(session_id, parentID);
                  }
                  continue;
                }
                if (!belongsToInstance(sessionProjects.get(session_id))) {
                  continue;
                }
                if (session_id) {
                  mainSessions.add(session_id);
                  pruneSessionCaches();
                }
                hook("session-start", { cwd, session_id, source: "startup" });
                hook("session-title", {
                  cwd,
                  session_id,
                  title: info?.title ?? "",
                });
                continue;
              }
    
              // The session doc's title changes via renames — opencode
              // auto-titles from the first exchange and applies manual renames
              // later. 2.0.32+ daemons emit `session.renamed` with a flat
              // `data: { sessionID, title }` and no longer fire
              // `session.updated` at all; older daemons carry the refreshed
              // session info on `session.updated` (`data.info.title`). Handle
              // both shapes so titles track across daemon upgrades. A
              // subagent session's rename is dropped so it cannot take over
              // the row.
              case "session.renamed":
              case "session.updated": {
                if (!isMainSession(session_id)) {
                  continue;
                }
                const title =
                  event.type === "session.renamed"
                    ? data.title
                    : (data.info?.title ?? data.title);
                // Track renames so pane-title matching in resolvePaneId stays
                // current: the TUI restamps its pane title with the new title,
                // and the cached session record must follow.
                const cached = sessionProjects.get(session_id);
                if (cached && typeof title === "string" && title !== "") {
                  cached.title = title;
                }
                hook("session-title", {
                  cwd,
                  session_id,
                  title: typeof title === "string" ? title : "",
                });
                continue;
              }
    
              // Status is a union: { type: "idle" | "busy" | "retry", ... }.
              // `busy` is a secondary status-transition signal — the real
              // prompt text is written via the `prompt` hook, which fires with
              // the admitted prompt text. Forward the empty-prompt submission
              // only when a turn starts without a prompt (retry recovery), so
              // a missing run clock still gets stamped without re-stamping one
              // that already spans the turn. `idle` needs no mapping here:
              // terminal turn state routes through the session.execution.*
              // events below (and session.idle on older servers), and handling
              // it too would fire the stop hook twice per turn end — harmless
              // (the notification dedups) but pointless.
              case "session.status": {
                if (!(await knownSessionBelongs(session_id)) || !isMainSession(session_id)) {
                  continue;
                }
                if (data.status?.type === "busy") {
                  // busy fires per assistant response — refresh the turn's
                  // staleness timestamp, and forward the empty-prompt
                  // submission only when a turn starts without a prompt (retry
                  // recovery), so a missing run clock still gets stamped
                  // without re-stamping one that already spans the turn.
                  const resume = !activeTurns.has(session_id);
                  activeTurns.set(session_id, Date.now());
                  if (resume) {
                    hook("user-prompt-submit", { cwd, session_id, prompt: "" });
                  }
                }
                continue;
              }
    
              // Turn lifecycle on 2.0.x. `started` marks the run clock even
              // when the prompt hook was missed (retry recovery); the
              // terminal events clear it and stop the pane. `interrupted`
              // covers user aborts and shutdowns — the turn is over either
              // way. `session.idle` is kept below as a legacy fallback for
              // servers that still publish it; a double stop dedups on the
              // notification side. A child session's lifecycle events feed
              // the child-turn tracker instead (H-bridge-1).
              case "session.execution.started":
                if (!(await knownSessionBelongs(session_id))) {
                  continue;
                }
                if (isMainSession(session_id)) {
                  activeTurns.set(session_id, Date.now());
                  hook("user-prompt-submit", { cwd, session_id, prompt: "" });
                  continue;
                }
                // A child turn started: track it and light the pane — the
                // activity forward flips a pane the main stop already landed
                // on back to running and stamps the missing clock.
                await trackChildTurnStart(session_id);
                continue;
    
              case "session.execution.succeeded":
              case "session.execution.interrupted":
              case "session.idle":
                if (!(await knownSessionBelongs(session_id))) {
                  continue;
                }
                if (isMainSession(session_id)) {
                  activeTurns.delete(session_id);
                  // Hold the stop while a child turn is still running for
                  // this session's pane (H-bridge-1): the last child's
                  // terminal event — or the idle hook's drained re-fire —
                  // sends it. A shutdown interrupt kills the children with
                  // the daemon, so no drain stop would ever come — that
                  // one always lands.
                  const shutdown =
                    event.type === "session.execution.interrupted" &&
                    data.reason === "shutdown";
                  if (shutdown || !rootHasActiveChildren(session_id)) {
                    hook("stop", { cwd, session_id, last_message: "" });
                  }
                  continue;
                }
                // A child's terminal event: release its turn and stop the
                // pane once the whole family drained.
                trackChildTurnEnd(session_id);
                continue;
    
              // Turn-level failure (OpenCode 1's session.error). Step-level
              // failures are not mapped: they may be retried within the turn.
              case "session.execution.failed":
                if (!(await knownSessionBelongs(session_id))) {
                  continue;
                }
                if (isMainSession(session_id)) {
                  activeTurns.delete(session_id);
                  hook("stop-failure", {
                    cwd,
                    session_id,
                    error:
                      data.error?.message || data.error?.type || "session.execution.failed",
                  });
                  continue;
                }
                // A failed child releases its turn like any terminal event;
                // the drain lands a plain stop — the failure is the child's,
                // and the main turn already reported its own state.
                trackChildTurnEnd(session_id);
                continue;
    
              // Permission and question (form) asks never touch the run clock,
              // so they pass more freely: a subagent asking mid-turn still
              // flips the pane to waiting (the whole turn is blocked on the
              // answer), while between turns only main-session asks are
              // forwarded so a background subagent cannot yank an idle pane
              // around.
              case "permission.asked":
                if (!(await knownSessionBelongs(session_id))) {
                  continue;
                }
                if (!isMainSession(session_id) && !turnActiveFor(session_id)) {
                  continue;
                }
                touchTurn(session_id);
                hook("notification", { cwd, session_id, wait_reason: "permission" });
                continue;
    
              // Blocking asks. 2.0.x dedicated question events (`question.asked`
              // with a flat `data.sessionID`) are still handled below for older
              // daemons; 2.1.x asks through the form surface instead —
              // `form.created` nests the request under `data.form`, where the
              // `question` tool sets `metadata.kind: "question"` and MCP
              // elicitations arrive as `kind: "mcp"` owned by the non-session
              // `"global"` sentinel. Both block the turn on the user exactly
              // like a permission ask, so they map to the same waiting
              // notification; a mid-turn subagent ask still flags the pane
              // (the whole turn is blocked on the answer), while between turns
              // only main-session asks are forwarded so a background subagent
              // cannot yank an idle pane around.
              case "form.created": {
                const form =
                  data.form && typeof data.form === "object" ? data.form : data;
                const form_session = sessionIdOf(form?.sessionID);
                if (!(await knownSessionBelongs(form_session))) {
                  continue;
                }
                if (!isMainSession(form_session) && !turnActiveFor(form_session)) {
                  continue;
                }
                touchTurn(form_session);
                // The question tool's form is a question; every other form
                // (MCP elicitation, provider auth) is a dialog waiting on a
                // selection. The `"global"` owner is not a real session — the
                // spawn is flagged unbind so resolution may still pick the pane
                // by cwd, but the CLI never records that id as the pane's
                // session binding.
                hook(
                  "notification",
                  {
                    cwd,
                    session_id: form_session,
                    wait_reason:
                      form?.metadata?.kind === "question"
                        ? "question"
                        : "elicitation_dialog",
                  },
                  { unbind: form_session === "" || form_session === "global" },
                );
                continue;
              }
    
              // OpenCode's `question` tool blocks the turn on a user answer
              // the same way a permission prompt does (2.0.x wire shape; 2.1.x
              // replaced it with `form.created` above). Without this the pane
              // stayed `running` while the agent waited for the answer. The
              // sessionID is flat on the event data.
              case "question.asked": {
                if (!(await knownSessionBelongs(session_id))) {
                  continue;
                }
                if (!isMainSession(session_id) && !turnActiveFor(session_id)) {
                  continue;
                }
                touchTurn(session_id);
                hook("notification", {
                  cwd,
                  session_id,
                  wait_reason: "question",
                });
                continue;
              }
    
              // The user answered (or rejected / dismissed) a permission,
              // question, or form ask. Re-submit an empty prompt to clear the
              // wait and mark the pane running again — the same
              // synthetic-resume signal the `busy` branch uses for retry
              // recovery. The activity-log handler keeps a pending-action wait
              // sticky, so a concurrent tool's result cannot clear it; this
              // reply is what resumes the pane. A `"global"` form reply is not
              // a real session's: flagged unbind like its ask above.
              case "permission.replied":
              case "question.replied":
              case "question.rejected":
              case "form.replied":
              case "form.cancelled":
                if (!(await knownSessionBelongs(session_id))) {
                  continue;
                }
                if (!isMainSession(session_id) && !turnActiveFor(session_id)) {
                  continue;
                }
                touchTurn(session_id);
                hook(
                  "user-prompt-submit",
                  { cwd, session_id, prompt: "" },
                  { unbind: session_id === "global" },
                );
                continue;
            }
          } catch {}
        }
      } catch {
        // Abort on unload throws from the iterator on some transports;
        // swallow it (and any transport death) here so unload never
        // becomes an unhandled rejection (audit 2026-11-25 L-bridge-3).
      }
    })();

    return () => controller.abort();
  },
};
