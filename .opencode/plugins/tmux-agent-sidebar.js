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
// `session.updated` (data.info.title), and the question tool surfaces as
// `question.asked` / `question.replied` / `question.rejected` with a flat
// `data.sessionID`.
//
// The bus is server-global: a plugin instance receives events for every
// session on the service, including other projects. `session.created`
// carries the new session's `data.info.projectID` / `data.info.directory`,
// which gates adoption and forwarding; lifecycle events for sessions we
// have not seen created are attributed with a one-time `session.get`
// lookup (cached per sessionID), so a foreign project's turn can never
// touch the pane — not even after a service restart mid-session.
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
// session's directory (nearest ancestor wins), disambiguated by the
// @pane_session_id binding this bridge wrote earlier. A valid inherited
// TMUX_PANE is only used when nothing better matches, so in-pane setups
// (OpenCode 1 style, or a local server) keep working unchanged.

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
// cleared it. Each link resolves when the spawned hook exits (bounded by a
// timeout so a wedged spawn cannot stall the chain), and OpenCode still
// gets its fire-and-forget dispatch semantics — the chain drains in the
// background.
let hookChain = Promise.resolve();

// One `tmux list-panes -a` snapshot, tab-separated so fields parse cleanly:
// pane_id, pane_current_command, pane_current_path, @pane_session_id.
// Resolves with "" when tmux is unavailable; never rejects.
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
          "#{pane_id}\t#{pane_current_command}\t#{pane_current_path}\t#{@pane_session_id}",
        ],
        { stdio: ["ignore", "pipe", "ignore"] },
      );
      let out = "";
      child.stdout.on("data", (chunk) => {
        out += chunk;
      });
      child.on("error", () => finish(""));
      child.on("exit", () => finish(out));
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

// Map a hook payload (session location + session id) to the tmux pane that
// hosts it. Preference order:
//   1. panes running `opencode` whose working directory equals the session
//      directory (or is its nearest ancestor — sessions can run inside
//      worktrees or subdirectories of the launched project), most specific
//      first;
//   2. when that match is ambiguous, the candidate whose @pane_session_id
//      equals the payload's session (a binding an earlier event wrote);
//   3. when nothing matches, the inherited TMUX_PANE — but only if tmux
//      confirms it still exists (the shared daemon's value is often stale).
// The cache only collapses bursts of events for the same session; every
// miss re-queries so a reopened or moved pane self-heals.
let paneCache = { key: "", pane: "", at: 0 };
const PANE_CACHE_MS = 2000;
async function resolvePaneId(cwd, sessionId) {
  if (typeof cwd !== "string" || cwd === "") return "";
  const key = `${cwd}|${sessionId}`;
  const now = Date.now();
  if (paneCache.key === key && now - paneCache.at < PANE_CACHE_MS) {
    return paneCache.pane;
  }
  const out = await listPanes();
  const panes = out
    .split("\n")
    .filter((line) => line !== "")
    .map((line) => line.split("\t"));
  const agent = process.env?.OPENCODE_PANE_COMMAND ?? "opencode";
  const candidates = panes.filter((p) => p[1] === agent);

  let realCwd = "";
  try {
    realCwd = realpathSync(cwd);
  } catch {}

  const pathMatches = candidates
    .filter((p) => {
      const panePath = p[2] ?? "";
      if (panePath === "") return false;
      return (
        panePath === cwd ||
        (realCwd !== "" && panePath === realCwd) ||
        cwd.startsWith(`${panePath}/`) ||
        (realCwd !== "" && realCwd.startsWith(`${panePath}/`))
      );
    })
    // Nearest ancestor first.
    .sort((a, b) => (b[2]?.length ?? 0) - (a[2]?.length ?? 0));

  let chosen = "";
  if (pathMatches.length === 1) {
    chosen = pathMatches[0][0];
  } else if (pathMatches.length > 1) {
    const bound =
      sessionId !== ""
        ? pathMatches.find((p) => p[3] === sessionId)
        : undefined;
    chosen = (bound ?? pathMatches[0])[0];
  } else if (sessionId !== "") {
    const bound = candidates.filter((p) => p[3] === sessionId);
    if (bound.length === 1) chosen = bound[0][0];
  }

  if (
    chosen === "" &&
    typeof process.env?.TMUX_PANE === "string" &&
    process.env.TMUX_PANE !== "" &&
    panes.some((p) => p[0] === process.env.TMUX_PANE)
  ) {
    chosen = process.env.TMUX_PANE;
  }

  paneCache = { key, pane: chosen, at: now };
  return chosen;
}

const hook = (eventName, payload) => {
  hookChain = hookChain.then(async () => {
    // The daemon has no trustworthy TMUX_PANE; inject the resolved one into
    // the spawned hook's environment. With no match, inherit the inherited
    // environment unchanged so non-daemon setups behave exactly as before.
    const paneId = await resolvePaneId(payload?.cwd, payload?.session_id ?? "");
    const env = paneId === "" ? undefined : { ...process.env, TMUX_PANE: paneId };
    await Promise.race([
      new Promise((resolve) => {
        try {
          const child = spawn(HOOK_COMMAND.cmd, [...HOOK_COMMAND.prefix, eventName], {
            stdio: ["pipe", "ignore", "ignore"],
            env,
          });
          child.on("error", () => resolve());
          child.on("exit", () => resolve());
          child.stdin.on("error", () => {});
          child.stdin.end(JSON.stringify(payload));
        } catch {
          // OpenCode should keep running even if the bridge is missing
          // or the sidebar binary is unavailable.
          resolve();
        }
      }),
      new Promise((resolve) => setTimeout(resolve, 3000)),
    ]);
  });
};

const sessionIdOf = (value) => (typeof value === "string" && value ? value : "");

export default {
  id: "tmux-agent-sidebar",
  async setup(ctx) {
    const cwd = sessionIdOf(ctx?.location?.directory);

    // OpenCode runs every `task` subagent in a child session (its own
    // sessionID, `parentID` pointing at the main session) whose bus events
    // and prompt hook all reach this plugin instance. Only the main
    // session's lifecycle may drive the pane's run state: a child's
    // session.idle would stop the clock mid-turn (and flag the pane done),
    // a child's prompt / session.status busy would restart it, and a
    // child's tool results arriving after the main turn's stop would
    // re-arm a cleared clock — a timer that never stops.
    //
    // `mainSession` is learned from top-level signals: a `session.created`
    // without a parentID, or a prompt while no turn is active from a
    // session not known to be a child (the prompt hook also fires for
    // subagent prompts, but a background subagent can prompt between turns
    // — so adoption must exclude sessions whose created event carried a
    // parentID). `turnPromptSeen` marks an active main turn: set by the
    // turn's prompt signal, cleared by stop/error.
    let mainSession = "";
    let turnPromptSeen = false;
    const childSessions = new Set();

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
      if (projectID && myProjectID) return projectID === myProjectID;
      if (projectID || myProjectID) return false; // only one side known: cannot confirm
      if (!directory || !myDirectory) return true;
      return directory === myDirectory || directory.startsWith(`${myDirectory}/`);
    };
    const lookupSessionInfo = async (session_id) => {
      if (sessionProjects.has(session_id)) return sessionProjects.get(session_id);
      let info = null;
      try {
        const session = await ctx.session.get({ sessionID: session_id });
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
          };
        }
      } catch {}
      // Cache misses too, so a deleted/foreign session is not re-queried
      // on every event.
      sessionProjects.set(session_id, info);
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

    // While the main session is unknown every event passes through (the
    // pre-gating behavior); once learned, foreign sessions are dropped.
    const isMainSession = (session_id) =>
      mainSession === "" || (session_id !== "" && session_id === mainSession);

    // Prompts admitted in the main session start (or restart) the turn;
    // child-session prompts never do. The hook receives the admitted
    // prompt text directly — no parts extraction like OpenCode 1.
    await ctx.session.hook("prompt", async (event) => {
      const session_id = sessionIdOf(event?.sessionID);
      // Subagent prompts route through the same hook as user prompts
      // (task tool → subagent admission), and a background subagent can
      // prompt between turns. With no active turn, a non-child session's
      // message is a fresh or switched-to main session and (re)adopts the
      // slot; known child prompts never do, and mid-turn foreign messages
      // are child prompts that must not restart the run clock. A session
      // this instance knows belongs to another project never adopts the
      // slot (the bus delivers every project's prompts).
      if (
        !turnPromptSeen &&
        !childSessions.has(session_id) &&
        (await knownSessionBelongs(session_id))
      ) {
        mainSession = session_id;
      }
      if (session_id !== mainSession) {
        return;
      }
      turnPromptSeen = true;
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
    // Child-session tool activity is shown while the main turn is active
    // (it is what the subagent is doing for the user), but dropped between
    // turns: the activity-log handler flips a non-running pane back to
    // running and stamps a missing run clock, so a background subagent's
    // results landing after the main turn's stop would leave the elapsed
    // timer ticking forever.
    await ctx.tool.hook("execute.after", async (event) => {
      const session_id = sessionIdOf(event?.sessionID);
      if (!(await knownSessionBelongs(session_id))) {
        return;
      }
      if (!isMainSession(session_id) && !turnPromptSeen) {
        return;
      }
      hook("activity-log", {
        cwd,
        session_id,
        tool_name: typeof event?.tool === "string" ? event.tool : "",
        tool_input: event?.input ?? {},
        tool_response: event?.status === "completed" ? event?.result ?? null : null,
      });
    });

    // The bus is a plain subscription in OpenCode 2; abort it on unload.
    const controller = new AbortController();
    void (async () => {
      for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
        if (!event || !event.type) continue;
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
            sessionProjects.set(session_id, {
              projectID: typeof info?.projectID === "string" ? info.projectID : "",
              directory:
                typeof info?.directory === "string"
                  ? info.directory
                  : typeof info?.location?.directory === "string"
                    ? info.location.directory
                    : "",
            });
            const parentID =
              sessionIdOf(info?.parentID) || sessionIdOf(data.parentID);
            if (parentID) {
              if (session_id) {
                childSessions.add(session_id);
              }
              continue;
            }
            if (!belongsToInstance(sessionProjects.get(session_id))) {
              continue;
            }
            if (session_id) {
              mainSession = session_id;
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
          // later. `session.updated` carries the refreshed session info;
          // forward its current title so the sidebar tracks it. A
          // subagent session's rename is dropped so it cannot take over
          // the row.
          case "session.updated": {
            if (!isMainSession(session_id)) {
              continue;
            }
            const title = data.info?.title ?? data.title;
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
            if (data.status?.type === "busy" && !turnPromptSeen) {
              turnPromptSeen = true;
              hook("user-prompt-submit", { cwd, session_id, prompt: "" });
            }
            continue;
          }

          // Turn lifecycle on 2.0.x. `started` marks the run clock even
          // when the prompt hook was missed (retry recovery); the
          // terminal events clear it and stop the pane. `interrupted`
          // covers user aborts and shutdowns — the turn is over either
          // way. `session.idle` is kept below as a legacy fallback for
          // servers that still publish it; a double stop dedups on the
          // notification side.
          case "session.execution.started":
            if (!(await knownSessionBelongs(session_id)) || !isMainSession(session_id)) {
              continue;
            }
            turnPromptSeen = true;
            hook("user-prompt-submit", { cwd, session_id, prompt: "" });
            continue;

          case "session.execution.succeeded":
          case "session.execution.interrupted":
          case "session.idle":
            if (!(await knownSessionBelongs(session_id)) || !isMainSession(session_id)) {
              continue;
            }
            turnPromptSeen = false;
            hook("stop", { cwd, session_id, last_message: "" });
            continue;

          // Turn-level failure (OpenCode 1's session.error). Step-level
          // failures are not mapped: they may be retried within the turn.
          case "session.execution.failed":
            if (!(await knownSessionBelongs(session_id)) || !isMainSession(session_id)) {
              continue;
            }
            turnPromptSeen = false;
            hook("stop-failure", {
              cwd,
              session_id,
              error:
                data.error?.message || data.error?.type || "session.execution.failed",
            });
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
            if (!isMainSession(session_id) && !turnPromptSeen) {
              continue;
            }
            hook("notification", { cwd, session_id, wait_reason: "permission" });
            continue;

          // OpenCode's `question` tool blocks the turn on a user answer
          // the same way a permission prompt does. Without this the pane
          // stayed `running` while the agent waited for the answer. The
          // sessionID is flat on the event data.
          case "question.asked": {
            if (!(await knownSessionBelongs(session_id))) {
              continue;
            }
            if (!isMainSession(session_id) && !turnPromptSeen) {
              continue;
            }
            hook("notification", {
              cwd,
              session_id,
              wait_reason: "question",
            });
            continue;
          }

          // The user answered (or rejected) a permission/question prompt.
          // Re-submit an empty prompt to clear the wait and mark the pane
          // running again — the same synthetic-resume signal the `busy`
          // branch uses for retry recovery. The activity-log handler keeps
          // a pending-action wait sticky, so a concurrent tool's result
          // cannot clear it; this reply is what resumes the pane.
          case "permission.replied":
          case "question.replied":
          case "question.rejected":
            if (!(await knownSessionBelongs(session_id))) {
              continue;
            }
            if (!isMainSession(session_id) && !turnPromptSeen) {
              continue;
            }
            hook("user-prompt-submit", { cwd, session_id, prompt: "" });
            continue;
        }
      }
    })();

    return () => controller.abort();
  },
};
