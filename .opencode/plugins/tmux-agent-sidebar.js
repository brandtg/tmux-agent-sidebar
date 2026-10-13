// OpenCode 2 plugin bridge for tmux-agent-sidebar.
//
// OpenCode 2 plugins default-export a definition with an `id` and a
// `setup(ctx)` that registers hooks on the context domains (a named export
// returning a hooks object is OpenCode 1 and is rejected at load time).
// Mapping from the OpenCode 1 bridge:
//
//   chat.message        → ctx.session.hook("prompt")      (event.prompt.text)
//   event bus           → ctx.event.subscribe()           (flat { type, data })
//   tool.execute.after  → ctx.tool.hook("execute.after")
//
// The bus also renamed its members: session.updated → session.renamed,
// session.error → session.execution.failed, and the question tool's
// question.asked/replied/rejected → form.created/replied/cancelled.

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
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
const hook = (eventName, payload) => {
  hookChain = hookChain.then(
    () =>
      Promise.race([
        new Promise((resolve) => {
          try {
            const child = spawn(HOOK_COMMAND.cmd, [...HOOK_COMMAND.prefix, eventName], {
              stdio: ["pipe", "ignore", "ignore"],
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
      ]),
  );
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
      // are child prompts that must not restart the run clock.
      if (!turnPromptSeen && !childSessions.has(session_id)) {
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
          // the slot either.
          case "session.created": {
            if (sessionIdOf(data.parentID)) {
              if (session_id) {
                childSessions.add(session_id);
              }
              continue;
            }
            if (session_id) {
              mainSession = session_id;
            }
            hook("session-start", { cwd, session_id, source: "startup" });
            hook("session-title", { cwd, session_id, title: data.title ?? "" });
            continue;
          }

          // The session doc's title changes via renames — opencode
          // auto-titles from the first exchange and applies manual renames
          // later. Forward the current title on every rename so the
          // sidebar tracks it; a subagent session's rename is dropped so
          // it cannot take over the row.
          case "session.renamed":
            if (!isMainSession(session_id)) {
              continue;
            }
            hook("session-title", { cwd, session_id, title: data.title ?? "" });
            continue;

          // Status is a union: { type: "idle" | "busy" | "retry", ... }.
          // `busy` is a secondary status-transition signal — the real
          // prompt text is written via the `prompt` hook, which fires with
          // the admitted prompt text. Forward the empty-prompt submission
          // only when a turn starts without a prompt (retry recovery), so
          // a missing run clock still gets stamped without re-stamping one
          // that already spans the turn. `idle` needs no mapping here:
          // opencode publishes `session.idle` alongside every
          // `session.status {idle}`, and that event already routes to
          // `stop` — handling both would fire the stop hook twice per
          // turn end.
          case "session.status": {
            if (!isMainSession(session_id)) {
              continue;
            }
            if (data.status?.type === "busy" && !turnPromptSeen) {
              turnPromptSeen = true;
              hook("user-prompt-submit", { cwd, session_id, prompt: "" });
            }
            continue;
          }

          case "session.idle":
            if (!isMainSession(session_id)) {
              continue;
            }
            turnPromptSeen = false;
            hook("stop", { cwd, session_id, last_message: "" });
            continue;

          // Turn-level failure (OpenCode 1's session.error). Step-level
          // failures are not mapped: they may be retried within the turn.
          case "session.execution.failed":
            if (!isMainSession(session_id)) {
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
            if (!isMainSession(session_id) && !turnPromptSeen) {
              continue;
            }
            hook("notification", { cwd, session_id, wait_reason: "permission" });
            continue;

          // OpenCode's `question` tool surfaces as a form; it blocks the
          // turn on a user answer the same way a permission prompt does.
          // Without this the pane stayed `running` while the agent waited
          // for the answer. The sessionID lives on the form object.
          case "form.created": {
            const form_session = sessionIdOf(data.form?.sessionID);
            if (!isMainSession(form_session) && !turnPromptSeen) {
              continue;
            }
            hook("notification", {
              cwd,
              session_id: form_session,
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
          case "form.replied":
          case "form.cancelled":
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
