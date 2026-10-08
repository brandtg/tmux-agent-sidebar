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

// Hook dispatch is serialized in program order: OpenCode fires the `event`
// hook without awaiting it, so fire-and-forget spawns would race their tmux
// writes — e.g. a first prompt's session-start (idle + clock clear) and
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

const pickFirstString = (value, keys) => {
  for (const key of keys) {
    const candidate = value?.[key];
    if (typeof candidate === "string" && candidate) {
      return candidate;
    }
  }
  return "";
};

const errorMessage = (err) => {
  if (!err) return "";
  if (typeof err === "string") return err;
  if (typeof err === "object") {
    return pickFirstString(err, ["message", "name"]) || JSON.stringify(err);
  }
  return String(err);
};

const extractPromptText = (parts) => {
  if (!Array.isArray(parts)) return "";
  const chunks = [];
  for (const part of parts) {
    if (!part || part.type !== "text") continue;
    if (part.synthetic || part.ignored) continue;
    if (typeof part.text === "string" && part.text) {
      chunks.push(part.text);
    }
  }
  return chunks.join("\n");
};

export const TmuxAgentSidebar = async ({ directory }) => {
  const cwd = typeof directory === "string" ? directory : "";

  // OpenCode runs every `task` subagent in a child session (its own
  // sessionID, `parentID` pointing at the main session) whose session.*
  // bus events and chat.message hook all reach this plugin instance.
  // Only the main session's lifecycle may drive the pane's run state:
  // a child's session.idle would stop the clock mid-turn (and flag the
  // pane done), a child's chat.message / session.status busy would
  // restart it, and a child's tool results arriving after the main
  // turn's stop would re-arm a cleared clock — a timer that never stops.
  //
  // `mainSession` is learned from top-level signals: a `session.created`
  // without a parentID, or a `chat.message` while no turn is active from a
  // session not known to be a child (the hook also fires for subagent
  // prompts, but a background subagent can prompt between turns — so
  // adoption must exclude sessions whose created event carried a
  // parentID). `turnPromptSeen` marks an active main turn: set by the
  // turn's prompt signal, cleared by stop/error.
  let mainSession = "";
  let turnPromptSeen = false;
  const childSessions = new Set();

  // While the main session is unknown every event passes through (the
  // pre-gating behavior); once learned, foreign sessions are dropped.
  const isMainSession = (session_id) =>
    mainSession === "" || (session_id !== "" && session_id === mainSession);

  const isChildSession = (info) =>
    Boolean(pickFirstString(info ?? {}, ["parentID", "parentId", "parent_id"]));

  return {
    "chat.message": async (input, output) => {
      const session_id =
        typeof input?.sessionID === "string" ? input.sessionID : "";
      // Subagent prompts route through the same hook as user prompts
      // (task tool → ops.prompt → createUserMessage), and a background
      // subagent can prompt between turns. With no active turn, a
      // non-child session's message is a fresh or switched-to main
      // session and (re)adopts the slot; known child prompts never do,
      // and mid-turn foreign messages are child prompts that must not
      // restart the run clock.
      if (!turnPromptSeen && !childSessions.has(session_id)) {
        mainSession = session_id;
      }
      if (session_id !== mainSession) {
        return;
      }
      turnPromptSeen = true;
      const prompt = extractPromptText(output?.parts);
      hook("user-prompt-submit", { cwd, session_id, prompt });
    },

    event: async ({ event }) => {
      if (!event || !event.type) return;
      const props = event.properties ?? {};
      const session_id = pickFirstString(props, ["sessionID", "sessionId", "session_id"]);

      switch (event.type) {
        case "session.created":
          // Child sessions must not claim the pane's identity or adopt
          // the main-session slot. Remember them so a later prompt (a
          // background subagent prompting between turns) cannot adopt
          // the slot either.
          if (isChildSession(props.info)) {
            if (session_id) {
              childSessions.add(session_id);
            }
            return;
          }
          if (session_id) {
            mainSession = session_id;
          }
          hook("session-start", { cwd, session_id, source: "startup" });
          hook("session-title", { cwd, session_id, title: props.info?.title ?? "" });
          return;

        // The session doc (including its title) changes outside of
        // session.created — opencode auto-titles from the first
        // exchange and applies manual renames later. Forward the
        // current title on every update so the sidebar tracks it,
        // but never let a subagent session's title take over the row.
        case "session.updated":
          if (isChildSession(props.info)) {
            return;
          }
          hook("session-title", { cwd, session_id, title: props.info?.title ?? "" });
          return;

        case "session.status": {
          // Status is a union: { type: "idle" | "busy" | "retry", ... }.
          // `busy` is a secondary status-transition signal — the real prompt
          // text is written via the `chat.message` hook, which fires with
          // the UserMessage parts. Forward the empty-prompt submission only
          // when a turn starts without a chat.message (retry recovery), so
          // a missing run clock still gets stamped without re-stamping one
          // that already spans the turn. `idle` needs no mapping: opencode
          // publishes `session.idle` alongside every
          // `session.status {idle}`, and that event already routes to
          // `stop` — handling both would fire the stop hook twice per
          // turn end.
          if (!isMainSession(session_id)) {
            return;
          }
          if (props.status?.type === "busy" && !turnPromptSeen) {
            turnPromptSeen = true;
            hook("user-prompt-submit", { cwd, session_id, prompt: "" });
          }
          return;
        }

        case "session.idle":
          if (!isMainSession(session_id)) {
            return;
          }
          turnPromptSeen = false;
          hook("stop", { cwd, session_id, last_message: "" });
          return;

        case "session.error":
          if (!isMainSession(session_id)) {
            return;
          }
          turnPromptSeen = false;
          hook("stop-failure", {
            cwd,
            session_id,
            error: errorMessage(props.error) || "session.error",
          });
          return;

        // Permission and question asks never touch the run clock, so they
        // pass more freely: a subagent asking mid-turn still flips the pane
        // to waiting (the whole turn is blocked on the answer), while
        // between turns only main-session asks are forwarded so a
        // background subagent cannot yank an idle pane around.
        case "permission.asked":
          if (!isMainSession(session_id) && !turnPromptSeen) {
            return;
          }
          hook("notification", { cwd, session_id, wait_reason: "permission" });
          return;

        // OpenCode's `question` tool blocks the turn on a user answer the
        // same way a permission prompt does. Without this the pane stayed
        // `running` while the agent waited for the answer.
        case "question.asked":
          if (!isMainSession(session_id) && !turnPromptSeen) {
            return;
          }
          hook("notification", { cwd, session_id, wait_reason: "question" });
          return;

        // The user answered (or rejected) a permission/question prompt.
        // Re-submit an empty prompt to clear the wait and mark the pane
        // running again — the same synthetic-resume signal the `busy`
        // branch uses for retry recovery. The activity-log handler keeps a
        // pending-action wait sticky, so a concurrent tool's result cannot
        // clear it; this reply is what resumes the pane.
        case "permission.replied":
        case "question.replied":
        case "question.rejected":
          if (!isMainSession(session_id) && !turnPromptSeen) {
            return;
          }
          hook("user-prompt-submit", { cwd, session_id, prompt: "" });
          return;
      }
    },

    // Dedicated hook for tool execution results. `event` bus does not carry
    // tool.execute.* — those are surfaced only through these trigger hooks.
    //
    // Child-session tool activity is shown while the main turn is active
    // (it is what the subagent is doing for the user), but dropped between
    // turns: the activity-log handler flips a non-running pane back to
    // running and stamps a missing run clock, so a background subagent's
    // results landing after the main turn's stop would leave the elapsed
    // timer ticking forever.
    "tool.execute.after": async (input, output) => {
      const session_id = input?.sessionID ?? "";
      if (!isMainSession(session_id) && !turnPromptSeen) {
        return;
      }
      hook("activity-log", {
        cwd,
        session_id,
        tool_name: input?.tool ?? "",
        tool_input: input?.args ?? {},
        tool_response: {
          title: output?.title ?? "",
          output: output?.output ?? "",
          metadata: output?.metadata ?? null,
        },
      });
    },
  };
};
