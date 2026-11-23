// Regression tests for the OpenCode bridge's session gating.
//
// OpenCode runs every `task` subagent in a child session whose bus events
// and prompt hook reach this plugin. The bridge must forward only the main
// session's lifecycle to the pane hooks, or a child's idle stops the
// elapsed clock mid-turn, a child's prompt restarts it, and a child's tool
// results re-arm a cleared clock after the turn's stop (a timer that never
// stops).
//
// Run: node --test .opencode/plugins/tmux-agent-sidebar.test.mjs
//
// The plugin resolves hook.sh by walking up from its own directory, so the
// harness copies it into a temp tree with a fake hook.sh that records every
// forwarded event instead of invoking the real binary. Each test gets a
// fresh temp tree, and therefore a fresh module instance with clean
// session-gating state.
//
// Since the bridge now resolves the target pane itself (OpenCode 2 runs
// plugins in a shared daemon whose TMUX_PANE cannot be trusted), the harness
// also shadows `tmux` on PATH with a script that prints a canned
// `list-panes -a` snapshot. Set `TMUX_RECORD_ENV=1` in a test to make the
// fake hook.sh additionally record the TMUX_PANE it was spawned with.
//
// The plugin is driven through its OpenCode 2 surface: a fake plugin
// context capturing the `prompt` / `execute.after` hooks and an event
// subscription backed by a FIFO queue, mirroring
// `ctx.session.hook`, `ctx.tool.hook`, and `ctx.event.subscribe`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath, pathToFileURL } from "node:url";

const PLUGIN_SRC = fileURLToPath(new URL("./tmux-agent-sidebar.js", import.meta.url));

async function loadPlugin({ panes = "", titles = {}, failFirstLookups = 0 } = {}) {
  const root = mkdtempSync(join(tmpdir(), "oc-bridge-test-"));
  const deep = join(root, "a", "b", "plugins");
  mkdirSync(deep, { recursive: true });
  writeFileSync(join(deep, "tmux-agent-sidebar.js"), readFileSync(PLUGIN_SRC, "utf8"), { mode: 0o755 });
  writeFileSync(
    join(root, "hook.sh"),
    `#!/usr/bin/env bash
payload=$(cat)
if [ "\${TMUX_RECORD_ENV-}" = "1" ]; then
  printf '%s %s env=%s bind=%s\\n' "$2" "$payload" "\${TMUX_PANE-unset}" "\${TMUX_AGENT_SIDEBAR_BIND_SESSION-unset}" >> ${JSON.stringify(join(root, "events.log"))}
else
  printf '%s %s\\n' "$2" "$payload" >> ${JSON.stringify(join(root, "events.log"))}
fi
`,
    { mode: 0o755 },
  );
  // Shadow tmux with a script printing the canned pane snapshot, so the
  // bridge's pane resolution is exercised without a real tmux server. The
  // snapshot lives in a file so a test can land a binding write
  // mid-scenario (setPanes) — the bridge re-reads it on its next resolve.
  const bindir = join(root, "bin");
  mkdirSync(bindir, { recursive: true });
  const panesFile = join(bindir, "panes.txt");
  writeFileSync(panesFile, panes);
  writeFileSync(
    join(bindir, "tmux"),
    `#!/usr/bin/env sh\ncat ${JSON.stringify(panesFile)}\n`,
    { mode: 0o755 },
  );
  const savedPath = process.env.PATH;
  process.env.PATH = `${bindir}:${savedPath ?? ""}`;
  let remainingLookupFails = failFirstLookups;

  const mod = import(pathToFileURL(join(deep, "tmux-agent-sidebar.js")));
  // Setup's cleanup (set by init) aborts the bus subscription; without it
  // the fake subscribe's poll timer would keep the event loop alive.
  let cleanup = null;
  return {
    async init(directory, projectID, sessionDirectory) {
      const { default: definition } = await mod;

      // Fake plugin context: capture registered hooks, queue bus events.
      const promptHooks = [];
      const toolHooks = [];
      const queue = [];
      const ctx = {
        location: projectID
          ? { directory, project: { id: projectID } }
          : { directory },
        session: {
          hook: async (name, cb) => {
            if (name === "prompt") promptHooks.push(cb);
          },
          // Server lookup for sessions this instance never saw created:
          // the test harness resolves them to `sessionDirectory` (or 404s)
          // and `titles[sessionID]` when a test needs a known title.
          // `failFirstLookups` makes the first N lookups throw, simulating
          // a transient RPC failure right after a daemon restart.
          get: async ({ sessionID }) => {
            if (remainingLookupFails > 0) {
              remainingLookupFails -= 1;
              throw new Error("transient lookup failure");
            }
            if (!sessionDirectory) throw new Error("not found");
            return {
              id: sessionID,
              projectID: projectID ?? "",
              location: { directory: sessionDirectory },
              title: titles[sessionID] ?? "",
            };
          },
        },
        tool: {
          hook: async (name, cb) => {
            if (name === "execute.after") toolHooks.push(cb);
          },
        },
        event: {
          subscribe({ signal } = {}) {
            let alive = true;
            signal?.addEventListener("abort", () => {
              alive = false;
            });
            return {
              [Symbol.asyncIterator]() {
                return {
                  next: async () => {
                    while (alive) {
                      if (queue.length > 0) {
                        return { value: queue.shift(), done: false };
                      }
                      await new Promise((resolve) => setTimeout(resolve, 5));
                    }
                    return { value: undefined, done: true };
                  },
                };
              },
            };
          },
        },
      };

      cleanup = await definition.setup(ctx);

      if (promptHooks.length !== 1 || toolHooks.length !== 1) {
        throw new Error("plugin did not register the prompt/execute.after hooks");
      }

      const pause = () => new Promise((resolve) => setTimeout(resolve, 120));
      // The hook spawns are fire-and-forget and their bash processes
      // append to the log in completion order. Serialize the driver with
      // a pause per invocation so the log reflects program order.
      return {
        async prompt(event) {
          await promptHooks[0](event);
          await pause();
        },
        async tool(event) {
          await toolHooks[0](event);
          await pause();
        },
        async event(event) {
          queue.push(event);
          await pause();
        },
      };
    },
    events() {
      try {
        return readFileSync(join(root, "events.log"), "utf8").trim().split("\n").filter(Boolean);
      } catch {
        return [];
      }
    },
    setPanes(next) {
      writeFileSync(panesFile, next);
    },
    dispose() {
      Promise.resolve(cleanup).then((fn) => fn?.());
      process.env.PATH = savedPath;
      rmSync(root, { recursive: true, force: true });
    },
  };
}

// Drain the plugin's fire-and-forget spawns: wait until the log stops
// growing or the deadline passes.
async function settle(h, ms = 2000) {
  const start = Date.now();
  let last = -1;
  while (Date.now() - start < ms) {
    const count = h.events().length;
    if (count === last && count > 0) return;
    last = count;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

const MAIN = "ses_main";
const CHILD = "ses_child";

test("subagent lifecycle never touches the main session's run state", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");

    // Main session established at launch, then a user turn starts.
    await plugin.event({ type: "session.created", data: { sessionID: MAIN, title: "Main" } });
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "fix the bug" } });
    // Subagent spins up: its own session + prompt + status + tools.
    await plugin.event({ type: "session.created", data: { sessionID: CHILD, parentID: MAIN, title: "explore" } });
    await plugin.prompt({ sessionID: CHILD, prompt: { text: "subagent prompt" } });
    await plugin.event({ type: "session.status", data: { sessionID: CHILD, status: { type: "busy" } } });
    await plugin.tool({ sessionID: CHILD, tool: "read", input: {}, status: "completed", result: { output: "o", metadata: null } });
    await plugin.event({ type: "session.idle", data: { sessionID: CHILD } });
    // Parent loop continues and finishes the turn.
    await plugin.event({ type: "session.status", data: { sessionID: MAIN, status: { type: "busy" } } });
    await plugin.event({ type: "session.idle", data: { sessionID: MAIN } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: MAIN, source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: MAIN, title: "Main" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "fix the bug" }),
      // The child's prompt, busy, and idle are all dropped; its mid-turn
      // tool result is kept (that is what the subagent is doing) and rides
      // under the parent's session id so it cannot rebind the pane.
      "activity-log " + JSON.stringify({ cwd: "/repo", session_id: MAIN, tool_name: "read", tool_input: {}, tool_response: { output: "o", metadata: null } }),
      // Exactly one stop fires — for the main session.
      "stop " + JSON.stringify({ cwd: "/repo", session_id: MAIN, last_message: "" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("turn-scoped busy events do not re-submit an empty prompt", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await plugin.event({ type: "session.status", data: { sessionID: MAIN, status: { type: "busy" } } });
    await plugin.event({ type: "session.status", data: { sessionID: MAIN, status: { type: "retry" }, next: 5 } });
    await plugin.event({ type: "session.status", data: { sessionID: MAIN, status: { type: "busy" } } });
    await plugin.event({ type: "session.idle", data: { sessionID: MAIN } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: MAIN, last_message: "" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("busy without a prompt still stamps the clock (retry recovery)", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.event({ type: "session.status", data: { sessionID: MAIN, status: { type: "busy" } } });
    await plugin.event({ type: "session.idle", data: { sessionID: MAIN } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "" }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: MAIN, last_message: "" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("background subagent events between turns are dropped", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await plugin.event({ type: "session.created", data: { sessionID: CHILD, parentID: MAIN, title: "bg" } });
    await plugin.event({ type: "session.idle", data: { sessionID: MAIN } });
    // Everything below happens after the main turn's stop.
    await plugin.event({ type: "session.status", data: { sessionID: CHILD, status: { type: "busy" } } });
    await plugin.prompt({ sessionID: CHILD, prompt: { text: "bg prompt" } });
    await plugin.tool({ sessionID: CHILD, tool: "bash", input: {}, status: "completed", result: { output: "o", metadata: null } });
    await plugin.event({ type: "session.idle", data: { sessionID: CHILD } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: MAIN, last_message: "" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("a prompt after the turn ends re-adopts a switched-to session", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "first" } });
    await plugin.event({ type: "session.idle", data: { sessionID: MAIN } });
    // User switches to another session (no session.created observed).
    await plugin.prompt({ sessionID: "ses_other", prompt: { text: "second" } });
    await plugin.event({ type: "session.idle", data: { sessionID: "ses_other" } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "first" }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: MAIN, last_message: "" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: "ses_other", prompt: "second" }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: "ses_other", last_message: "" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("subagent permission asks pass mid-turn but not between turns", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.event({ type: "session.created", data: { sessionID: MAIN, title: "Main" } });
    // The task tool creates the child session (with parentID) before its
    // events flow, so the bridge knows the parentage when the ask arrives.
    await plugin.event({ type: "session.created", data: { sessionID: CHILD, parentID: MAIN, title: "subagent" } });
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await plugin.event({ type: "permission.asked", data: { sessionID: CHILD, id: "per_1" } });
    await plugin.event({ type: "session.idle", data: { sessionID: MAIN } });
    await plugin.event({ type: "permission.asked", data: { sessionID: CHILD, id: "per_2" } });
    await plugin.event({ type: "permission.asked", data: { sessionID: MAIN, id: "per_3" } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: MAIN, source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: MAIN, title: "Main" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }),
      // The child's ask keeps its wait_reason but carries the parent's
      // session id — a child-id payload would rebind @pane_session_id to
      // the child and wipe the pane's title on every flip.
      "notification " + JSON.stringify({ cwd: "/repo", session_id: MAIN, wait_reason: "permission" }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: MAIN, last_message: "" }),
      "notification " + JSON.stringify({ cwd: "/repo", session_id: MAIN, wait_reason: "permission" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("question asks map to a waiting notification like permissions", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.event({ type: "session.created", data: { sessionID: MAIN, title: "Main" } });
    await plugin.event({ type: "session.created", data: { sessionID: CHILD, parentID: MAIN, title: "subagent" } });
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    // Mid-turn child question passes (the whole turn is blocked).
    await plugin.event({ type: "question.asked", data: { sessionID: CHILD, id: "que_1" } });
    await plugin.event({ type: "session.idle", data: { sessionID: MAIN } });
    // Between turns a child question is dropped, a main question passes.
    await plugin.event({ type: "question.asked", data: { sessionID: CHILD, id: "que_2" } });
    await plugin.event({ type: "question.asked", data: { sessionID: MAIN, id: "que_3" } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: MAIN, source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: MAIN, title: "Main" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }),
      // Same parent-id routing as permission asks.
      "notification " + JSON.stringify({ cwd: "/repo", session_id: MAIN, wait_reason: "question" }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: MAIN, last_message: "" }),
      "notification " + JSON.stringify({ cwd: "/repo", session_id: MAIN, wait_reason: "question" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("permission and question replies resume the pane", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await plugin.event({ type: "permission.replied", data: { sessionID: MAIN, requestID: "per_1", reply: "once" } });
    await plugin.event({ type: "question.replied", data: { sessionID: MAIN, requestID: "que_1", answers: [] } });
    await plugin.event({ type: "question.rejected", data: { sessionID: MAIN, requestID: "que_2" } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }),
      // Each reply re-submits an empty prompt: the synthetic resume that
      // clears the sticky wait and flips the pane back to running.
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "" }),
    ]);
  } finally {
    h.dispose();
  }
});

// 2.1.x removed the dedicated question events: the `question` tool now
// asks via `form.created` (request nested under `data.form`, metadata.kind
// "question") and clears via `form.replied` / `form.cancelled`. Without the
// form mapping the pane stayed `running` (solid/flashing green) for as
// long as the agent waited on the answer — the ask was invisible.
test("2.1.x form asks map to waiting notifications like the question tool", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.event({ type: "session.created", data: { sessionID: MAIN, title: "Main" } });
    await plugin.event({ type: "session.created", data: { sessionID: CHILD, parentID: MAIN, title: "subagent" } });
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    // Mid-turn child question form passes (the whole turn is blocked).
    await plugin.event({
      type: "form.created",
      data: {
        form: {
          id: "frm_1",
          sessionID: CHILD,
          title: "Questions",
          metadata: { kind: "question", tool: { messageID: "msg_1", id: "call_1" } },
          fields: [],
        },
      },
    });
    await plugin.event({ type: "session.idle", data: { sessionID: MAIN } });
    // Between turns a child form is dropped, a main form passes.
    await plugin.event({
      type: "form.created",
      data: { form: { id: "frm_2", sessionID: CHILD, title: "Questions", metadata: { kind: "question" }, fields: [] } },
    });
    await plugin.event({
      type: "form.created",
      data: { form: { id: "frm_3", sessionID: MAIN, title: "Questions", metadata: { kind: "question" }, fields: [] } },
    });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: MAIN, source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: MAIN, title: "Main" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }),
      // Same parent-id routing as permission asks.
      "notification " + JSON.stringify({ cwd: "/repo", session_id: MAIN, wait_reason: "question" }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: MAIN, last_message: "" }),
      "notification " + JSON.stringify({ cwd: "/repo", session_id: MAIN, wait_reason: "question" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("non-question forms flag an elicitation dialog and never bind the global owner", async () => {
  const h = await loadPlugin({ panes: "%2\topencode\t/repo\t\n" });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    // MCP elicitations are owned by the non-session "global" sentinel:
    // mid-turn they still flag the pane (the turn is blocked on the
    // answer) but must never record "global" as the pane's session
    // binding (bind=0), or every later real-session event would have to
    // heal the pane's identity.
    await plugin.event({
      type: "form.created",
      data: { form: { id: "frm_1", sessionID: "global", title: "Authorize", metadata: { kind: "mcp" }, fields: [] } },
    });
    // A real-session non-question form (provider auth) binds normally.
    await plugin.event({
      type: "form.created",
      data: { form: { id: "frm_2", sessionID: MAIN, title: "Connect", metadata: { kind: "provider" }, fields: [] } },
    });
    // The "global" form's reply is likewise unbind-flagged.
    await plugin.event({ type: "form.replied", data: { sessionID: "global", id: "frm_1", answer: { authorization: "ok" } } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }) + " env=%2 bind=unset",
      "notification " + JSON.stringify({ cwd: "/repo", session_id: "global", wait_reason: "elicitation_dialog" }) + " env=%2 bind=0",
      "notification " + JSON.stringify({ cwd: "/repo", session_id: MAIN, wait_reason: "elicitation_dialog" }) + " env=%2 bind=unset",
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: "global", prompt: "" }) + " env=%2 bind=0",
    ]);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    h.dispose();
  }
});

test("form replies and cancellations resume the pane", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await plugin.event({ type: "form.replied", data: { sessionID: MAIN, id: "frm_1", answer: { q0: ["once"] } } });
    await plugin.event({ type: "form.cancelled", data: { sessionID: MAIN, id: "frm_2" } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }),
      // Each reply re-submits an empty prompt: the synthetic resume that
      // clears the sticky wait and flips the pane back to running.
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("session.execution.failed ends the turn and re-arms gating for the next one", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await plugin.event({ type: "session.execution.failed", data: { sessionID: MAIN, error: { type: "provider", message: "boom" } } });
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "again" } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }),
      "stop-failure " + JSON.stringify({ cwd: "/repo", session_id: MAIN, error: "boom" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "again" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("renames arrive via session.updated and child renames never take over the row", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.event({ type: "session.created", data: { sessionID: MAIN, title: "Main" } });
    await plugin.event({ type: "session.updated", data: { sessionID: CHILD, info: { title: "child" } } });
    await plugin.event({ type: "session.updated", data: { sessionID: MAIN, info: { title: "Renamed" } } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: MAIN, source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: MAIN, title: "Main" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: MAIN, title: "Renamed" }),
    ]);
  } finally {
    h.dispose();
  }
});

// Regression: opencode 2.0.32 changed the title contract — `session.updated`
// no longer fires at all; titles arrive as `session.renamed` with a flat
// `data: { sessionID, title }`. The bridge must forward the flat title
// (not `data.info?.title`) and drop a subagent session's rename.
test("renames arrive via session.renamed (2.0.32) and child renames never take over the row", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.event({ type: "session.created", data: { sessionID: MAIN, title: "Main" } });
    await plugin.event({ type: "session.renamed", data: { sessionID: CHILD, title: "child" } });
    await plugin.event({ type: "session.renamed", data: { sessionID: MAIN, title: "Renamed" } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: MAIN, source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: MAIN, title: "Main" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: MAIN, title: "Renamed" }),
    ]);
  } finally {
    h.dispose();
  }
});

// Regression: OpenCode 2.0.x ends turns with session.execution.* — it never
// publishes session.idle / session.status, so the v1-era mapping left the
// pane running (solid status dot) forever after the agent finished.
test("a turn that ends with session.execution.succeeded stops the pane", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "fix the bug" } });
    await plugin.event({ type: "session.execution.started", data: { sessionID: MAIN } });
    await plugin.event({ type: "session.execution.succeeded", data: { sessionID: MAIN } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "fix the bug" }),
      // started re-submits an empty prompt: idempotent running signal that
      // stamps a missing clock without restarting the prompt's own one.
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "" }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: MAIN, last_message: "" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("an interrupted turn stops the pane", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await plugin.event({ type: "session.execution.interrupted", data: { sessionID: MAIN, reason: "user" } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: MAIN, last_message: "" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("execution.started without a prompt still stamps the running state", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.event({ type: "session.execution.started", data: { sessionID: MAIN } });
    await plugin.event({ type: "session.execution.failed", data: { sessionID: MAIN, error: { type: "provider", message: "boom" } } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "" }),
      "stop-failure " + JSON.stringify({ cwd: "/repo", session_id: MAIN, error: "boom" }),
    ]);
  } finally {
    h.dispose();
  }
});

// The event bus is server-global: sessions from other projects must not
// claim the pane's identity or rename it. Adoption and forwarding gate on
// the projectID learned from session.created.
test("a foreign project's session.created is ignored entirely", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo", "proj_mine");
    // Real 2.0.x bus shape: the session record nests under `info`.
    await plugin.event({
      type: "session.created",
      data: { sessionID: "ses_foreign", info: { projectID: "proj_other", directory: "/other", title: "Other project" } },
    });
    await plugin.event({ type: "session.execution.started", data: { sessionID: "ses_foreign" } });
    await plugin.event({ type: "session.execution.succeeded", data: { sessionID: "ses_foreign" } });
    // Our own session still works normally.
    await plugin.event({
      type: "session.created",
      data: { sessionID: MAIN, info: { projectID: "proj_mine", directory: "/repo", title: "Main" } },
    });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: MAIN, source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: MAIN, title: "Main" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("a known foreign session cannot adopt the main slot via the prompt hook", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo", "proj_mine");
    await plugin.event({
      type: "session.created",
      data: { sessionID: "ses_foreign", info: { projectID: "proj_other", directory: "/other", title: "Other project" } },
    });
    await plugin.prompt({ sessionID: "ses_foreign", prompt: { text: "foreign turn" } });
    // The main session is only adopted once a belonging prompt arrives.
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "mine" } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "mine" }),
    ]);
  } finally {
    h.dispose();
  }
});

// Regression: 2.0.x nests the session record under `data.info`. Reading
// `data.projectID` flat left every session's project unknown, so the gate
// default-allowed and the newest created session stole `mainSession` in
// every instance — the real session's lifecycle events (session-start /
// user-prompt-submit / stop) were then dropped and its pane never left
// "Waiting for prompt…", while tool activity kept flowing.
test("a foreign session.created nested under info cannot steal the main slot", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo", "proj_mine");
    // Foreign project's session created first, ours second (newest on the
    // bus must not win the slot).
    await plugin.event({
      type: "session.created",
      data: { sessionID: "ses_foreign", info: { projectID: "proj_other", directory: "/other", title: "Foreign" } },
    });
    await plugin.event({
      type: "session.created",
      data: { sessionID: MAIN, info: { projectID: "proj_mine", directory: "/repo", title: "Main" } },
    });
    await plugin.prompt({ sessionID: "ses_foreign", prompt: { text: "foreign turn" } });
    await plugin.tool({ sessionID: "ses_foreign", tool: "read", input: {}, status: "completed", result: { output: "o", metadata: null } });
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "mine" } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: MAIN, source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: MAIN, title: "Main" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "mine" }),
    ]);
  } finally {
    h.dispose();
  }
});

// Shared-daemon flavor: the instance has no project identity, only a
// directory. A foreign session carrying its projectID must be dropped
// entirely (it used to be adopted and misroute events into panes of the
// wrong project), while a project-less record can still be attributed by
// directory.
test("a projectless instance gates nested creations by directory", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.event({
      type: "session.created",
      data: { sessionID: "ses_foreign", info: { projectID: "proj_other", directory: "/other", title: "Foreign" } },
    });
    await plugin.prompt({ sessionID: "ses_foreign", prompt: { text: "foreign turn" } });
    await plugin.tool({ sessionID: "ses_foreign", tool: "read", input: {}, status: "completed", result: { output: "o", metadata: null } });
    // No projectID on the record: the directory gate attributes it.
    await plugin.event({
      type: "session.created",
      data: { sessionID: MAIN, info: { directory: "/repo", title: "Main" } },
    });
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "mine" } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: MAIN, source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: MAIN, title: "Main" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "mine" }),
    ]);
  } finally {
    h.dispose();
  }
});

// Older shapes carried the attribution flat on `data`; the hedge must
// keep gating on it.
test("a flat-shape foreign session.created is still gated", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo", "proj_mine");
    await plugin.event({
      type: "session.created",
      data: { sessionID: "ses_foreign", projectID: "proj_other", title: "Other project" },
    });
    await plugin.prompt({ sessionID: "ses_foreign", prompt: { text: "foreign turn" } });
    await settle(h);

    assert.deepEqual(h.events(), []);
  } finally {
    h.dispose();
  }
});

// Sessions that predate the plugin load (service restart mid-session) are
// never seen in session.created; the bridge attributes them with a one-time
// session.get lookup instead of trusting every unknown sessionID.
test("an unknown session is attributed via a server lookup before adoption", async () => {
  const h = await loadPlugin();
  try {
    // Lookups resolve to a directory outside the instance's project tree.
    const plugin = await h.init("/repo", undefined, "/elsewhere");
    await plugin.prompt({ sessionID: "ses_foreign", prompt: { text: "foreign turn" } });
    await plugin.event({ type: "session.execution.succeeded", data: { sessionID: "ses_foreign" } });
    await settle(h);

    assert.deepEqual(h.events(), []);
  } finally {
    h.dispose();
  }
});

test("an unknown session resolving inside the instance directory is adopted", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo/sub", undefined, "/repo/sub");
    await plugin.prompt({ sessionID: "ses_recovered", prompt: { text: "after restart" } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo/sub", session_id: "ses_recovered", prompt: "after restart" }),
    ]);
  } finally {
    h.dispose();
  }
});

// A thrown lookup (service restarting, transient RPC failure) used to be
// cached as a permanent miss — and since belongsToInstance keeps the
// pre-gating behavior for unknown sessions, one transient failure
// default-allowed that session for its whole life. A failed lookup must
// retry on the next event so attribution eventually lands: the first
// event rides the pre-gating pass (nothing is cached), the second is
// finally attributed and dropped.
test("a transient lookup failure is retried, not cached as a miss", async () => {
  const h = await loadPlugin({ failFirstLookups: 1 });
  try {
    const plugin = await h.init("/repo", undefined, "/elsewhere");
    await plugin.event({ type: "session.execution.succeeded", data: { sessionID: "ses_foreign" } });
    await settle(h);
    const afterFirst = h.events();

    await plugin.event({ type: "session.execution.succeeded", data: { sessionID: "ses_foreign" } });
    await settle(h);

    assert.deepEqual(h.events(), afterFirst, "the attributed foreign session must be dropped");
  } finally {
    h.dispose();
  }
});

test("failed tool results log the call with a null response", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await plugin.tool({ sessionID: MAIN, tool: "bash", input: {}, status: "error", error: { message: "nope" } });
    await plugin.event({ type: "session.idle", data: { sessionID: MAIN } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }),
      // A failed tool is still activity; only the response is absent.
      "activity-log " + JSON.stringify({ cwd: "/repo", session_id: MAIN, tool_name: "bash", tool_input: {}, tool_response: null }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: MAIN, last_message: "" }),
    ]);
  } finally {
    h.dispose();
  }
});

// The daemon's inherited TMUX_PANE must never win over a pane the bridge
// resolved from the pane table itself.
const STALE_PANES = "%1\tbash\t/repo\t\n%2\topencode\t/elsewhere\t\n";

test("a valid inherited TMUX_PANE is kept when no opencode pane matches better", async () => {
  const h = await loadPlugin({ panes: STALE_PANES });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    process.env.TMUX_PANE = "%1";
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await settle(h);

    // %2 is an opencode pane but in the wrong directory; %1 is not an
    // opencode pane at all, so nothing is injected and the inherited
    // environment passes through untouched (this is what keeps plain
    // in-pane setups working — and no binding can be recorded on a shell).
    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }) +
        " env=%1 bind=unset",
    ]);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    delete process.env.TMUX_PANE;
    h.dispose();
  }
});

test("pane resolution injects the matching pane's id into the hook env", async () => {
  const h = await loadPlugin({ panes: "%9\tbash\t/repo\t\n%2\topencode\t/repo\t\n" });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    process.env.TMUX_PANE = "%9";
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await settle(h);

    assert.match(h.events()[0], / env=%2 bind=unset$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    delete process.env.TMUX_PANE;
    h.dispose();
  }
});

test("pane resolution matches opencode panes in ancestor directories", async () => {
  const h = await loadPlugin({ panes: "%7\topencode\t/repo\t\n" });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    // Session running in a subdirectory (or worktree) of the launched
    // project; the /repo pane is the nearest match.
    const plugin = await h.init("/repo/sub");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await settle(h);

    assert.match(h.events()[0], / env=%7 bind=unset$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    h.dispose();
  }
});

test("pane resolution uses the session binding to disambiguate same-directory panes", async () => {
  const h = await loadPlugin({
    panes: "%2\topencode\t/repo\t\n%4\topencode\t/repo\tses_main\n",
  });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await settle(h);

    // Both panes match /repo; @pane_session_id=ses_main pins %4.
    assert.match(h.events()[0], / env=%4 bind=unset$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    h.dispose();
  }
});

test("a valid inherited TMUX_PANE is kept when no pane matches", async () => {
  const h = await loadPlugin({ panes: "" });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    process.env.TMUX_PANE = "%5";
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await settle(h);

    // No panes at all: the inherited value cannot be validated, so it is
    // left untouched (in-pane setups keep working).
    assert.match(h.events()[0], / env=%5 bind=unset$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    delete process.env.TMUX_PANE;
    h.dispose();
  }
});

// Regression: with a plain first-match tie-break, two same-directory
// sessions both resolved to the first pane in list-panes order. The second
// session never received an event (so it could never acquire a binding of
// its own) and the first pane carried the wrong session's prompt and clock.
test("a second same-directory session claims the unbound pane instead of stealing a bound one", async () => {
  const h = await loadPlugin({
    panes: "%2\topencode\t/repo\tses_a\n%4\topencode\t/repo\t\n",
  });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: "ses_b", prompt: { text: "go" } });
    await settle(h);

    // %2 is bound to ses_a; the unbound %4 must take session B's events.
    assert.match(h.events()[0], / env=%4 bind=unset$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    h.dispose();
  }
});

// Regression: a single main-session slot made concurrent sessions in one
// project steal it from each other — the newest session.created or the
// mid-turn prompt won, and the loser's turns were dropped entirely (its
// pane never went green, and its prompt text landed on the wrong row).
test("concurrent main sessions route their turns independently", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.event({ type: "session.created", data: { sessionID: "ses_a", info: { directory: "/repo", title: "A" } } });
    await plugin.event({ type: "session.created", data: { sessionID: "ses_b", info: { directory: "/repo", title: "B" } } });
    await plugin.prompt({ sessionID: "ses_a", prompt: { text: "turn a" } });
    // B prompts while A's turn is still active — must not be dropped.
    await plugin.prompt({ sessionID: "ses_b", prompt: { text: "turn b" } });
    await plugin.event({ type: "session.execution.succeeded", data: { sessionID: "ses_a" } });
    await plugin.event({ type: "session.execution.succeeded", data: { sessionID: "ses_b" } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: "ses_a", source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: "ses_a", title: "A" }),
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: "ses_b", source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: "ses_b", title: "B" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: "ses_a", prompt: "turn a" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: "ses_b", prompt: "turn b" }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: "ses_a", last_message: "" }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: "ses_b", last_message: "" }),
    ]);
  } finally {
    h.dispose();
  }
});

// The opencode TUI stamps the active session's title into the pane title
// ("OC | <title>"). When no binding exists yet, that per-pane ground truth
// routes the session even if every candidate is bound to something else —
// and it proves a binding stale when the bound session's title no longer
// matches what the pane's TUI displays (restarted or switched TUI; nothing
// else ever clears @pane_session_id).
test("a pane title pins the session's pane when no binding exists", async () => {
  const h = await loadPlugin({
    panes:
      "%2\topencode\t/repo\tses_old\tOC | Old work\n" +
      "%4\topencode\t/repo\tses_other\tOC | Converter fixes\n",
  });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    const plugin = await h.init("/repo");
    await plugin.event({
      type: "session.created",
      data: { sessionID: "ses_new", info: { directory: "/repo", title: "Converter fixes" } },
    });
    await plugin.prompt({ sessionID: "ses_new", prompt: { text: "go" } });
    await settle(h);

    const routed = h.events().filter((line) => line.startsWith("user-prompt-submit"));
    assert.equal(routed.length, 1);
    // %4's TUI is hosting ses_new (title match); the guess would have been
    // %2 and would have arrived flagged bind=0.
    assert.match(routed[0], / env=%4 bind=unset$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    h.dispose();
  }
});

test("a truncated pane title prefix-matches the full session title", async () => {
  const h = await loadPlugin({
    panes:
      "%2\topencode\t/repo\tses_a\tOC | Fix opencode session status swap in T…\n" +
      "%4\topencode\t/repo\tses_b\tOC | Unrelated work\n",
    titles: { ses_new: "Fix opencode session status swap in Tmux sidebar" },
  });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    const plugin = await h.init("/repo", undefined, "/repo");
    await plugin.prompt({ sessionID: "ses_new", prompt: { text: "go" } });
    await settle(h);

    assert.match(h.events()[0], / env=%2 bind=unset$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    h.dispose();
  }
});

// Regression: opencode has no session-end event, so a pane whose TUI was
// restarted or switched sessions keeps the dead session's binding forever.
// With every candidate bound, the old tie-break collapsed all new sessions
// onto the first pane and starved the rest. The stale binding is now
// reclaimable because the pane's current title no longer matches the bound
// session's title — while a live binding (title still matching) is never
// stolen.
test("a stale binding is reclaimed instead of collapsing onto the first pane", async () => {
  const h = await loadPlugin({
    panes:
      "%2\topencode\t/repo\tses_dead\tOC | Fresh start\n" +
      "%4\topencode\t/repo\tses_live\tOC | Live work\n",
    titles: { ses_dead: "Dead work", ses_live: "Live work" },
  });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    const plugin = await h.init("/repo", undefined, "/repo");
    await plugin.prompt({ sessionID: "ses_new", prompt: { text: "go" } });
    await settle(h);

    // %2 is bound to the dead ses_dead ("Dead work" ≠ its TUI's "Fresh
    // start"); %4's binding is still live and must not be touched.
    assert.match(h.events()[0], / env=%2 bind=unset$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    h.dispose();
  }
});

// A binding can survive a TUI restart or session switch while naming a
// session that is still live — the pane's own title is what proves it no
// longer hosts that session. When a title-matched pane exists, it wins
// over the contradicting binding, exactly like the reclaim path does for
// dead bindings.
test("a binding contradicted by the pane title loses to the title match", async () => {
  const h = await loadPlugin({
    panes:
      "%2\topencode\t/repo\tses_convert\tOC | Expand Clocks tab columns to full width\n" +
      "%4\topencode\t/repo\tses_live\tOC | Convert tab errors and style issues\n",
    titles: {
      ses_convert: "Convert tab errors and style issues",
      ses_live: "Live work",
    },
  });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    const plugin = await h.init("/repo", undefined, "/repo");
    await plugin.prompt({ sessionID: "ses_convert", prompt: { text: "go" } });
    await settle(h);

    // %2 is bound to ses_convert, but its TUI shows the Clocks session's
    // title; %4's title names ses_convert. The binding was left behind by
    // a restart/switch (or by the pre-title-match bridge) and must lose.
    assert.match(h.events()[0], / env=%4 bind=unset$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    h.dispose();
  }
});

// When nothing better matches — every candidate bound and no title
// evidence — the first match is a guess that must not record a binding on
// a pane that may belong to another live session.
test("a last-resort guess routes the event but flags the binding as uncommitted", async () => {
  const h = await loadPlugin({
    panes:
      "%2\topencode\t/repo\tses_a\t\n" +
      "%4\topencode\t/repo\tses_b\t\n",
  });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: "ses_new", prompt: { text: "go" } });
    await settle(h);

    assert.match(h.events()[0], / env=%2 bind=0$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    h.dispose();
  }
});

// Audit #1: two auto-titled sessions in one directory share long prefixes.
// %2 is bound to ses_a and its TUI displays ses_a's title — a prefix of
// ses_b's longer title. That stamp is explained by %2's own binding, so it
// is not evidence for ses_b: the old ≥8-char prefix match routed ses_b to
// %2 and re-pointed its binding (the reported "two agents writing to each
// other's title" wig).
test("a stamp explained by the pane's own binding is not title evidence for another session", async () => {
  const h = await loadPlugin({
    panes:
      "%2\topencode\t/repo\tses_a\tOC | Refactor the auth flow\n" +
      "%4\topencode\t/repo\t\n",
    titles: { ses_a: "Refactor the auth flow", ses_b: "Refactor the auth flow in the API" },
  });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    const plugin = await h.init("/repo", undefined, "/repo");
    await plugin.prompt({ sessionID: "ses_b", prompt: { text: "go" } });
    await settle(h);

    // %2's stamp belongs to ses_a; ses_b free-claims the unbound %4.
    assert.match(h.events()[0], / env=%4 bind=unset$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    h.dispose();
  }
});

// Audit #1: ses_b is already bound to %4 and %4's own stamp confirms it,
// while %2's stamp (ses_a's title, a prefix of ses_b's) loose-matches.
// The titled-versus-bound conflict must not re-point ses_b's binding.
test("a prefix-hit title cannot re-point a binding the bound pane's own stamp confirms", async () => {
  const h = await loadPlugin({
    panes:
      "%2\topencode\t/repo\tses_a\tOC | Refactor the auth flow\n" +
      "%4\topencode\t/repo\tses_b\tOC | Refactor the auth flow in the API\n",
    titles: { ses_a: "Refactor the auth flow", ses_b: "Refactor the auth flow in the API" },
  });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    const plugin = await h.init("/repo", undefined, "/repo");
    await plugin.prompt({ sessionID: "ses_b", prompt: { text: "go" } });
    await settle(h);

    // %4's TUI displays ses_b's title: the binding is ground truth and
    // %2's prefix hit must not win it.
    assert.match(h.events()[0], / env=%4 bind=unset$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    h.dispose();
  }
});

// Audit #2: the daemon's inherited TMUX_PANE is a guess. When it does
// point at a live opencode pane the event may ride it — but never
// committed, or the CLI records a binding on a pane that may belong to
// another session (the permanent-misroute case the bind flag defuses).
test("an inherited TMUX_PANE pointing at an opencode pane is routed uncommitted", async () => {
  const h = await loadPlugin({ panes: "%3\topencode\t/elsewhere\t\n" });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    process.env.TMUX_PANE = "%3";
    const plugin = await h.init("/repo");
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await settle(h);

    // %3 is an opencode pane but in the wrong directory and unbound: the
    // inherited pane is the last resort — routed, flagged bind=0.
    assert.match(h.events()[0], / env=%3 bind=0$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    delete process.env.TMUX_PANE;
    h.dispose();
  }
});

// Audit #3: the 2s pane cache can serve a resolution captured before
// another session's binding write landed, double-claiming one pane. A
// committed resolution therefore invalidates cached resolutions pointing
// at the same pane.
test("a committed resolution invalidates cached resolutions of the same pane", async () => {
  const h = await loadPlugin({
    panes: "%2\topencode\t/repo\t\n%4\topencode\t/repo\t\n",
  });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    const plugin = await h.init("/repo");
    // B resolves first and free-claims %2 (cached).
    await plugin.prompt({ sessionID: "ses_b", prompt: { text: "b1" } });
    // A resolves next, free-claims %2 too, and its hook writes the
    // binding — landed in the pane table here.
    await plugin.prompt({ sessionID: "ses_a", prompt: { text: "a1" } });
    h.setPanes("%2\topencode\t/repo\tses_a\t\n%4\topencode\t/repo\t\n");
    // B's next event must re-resolve instead of riding the pre-write
    // cache entry into re-claiming %2.
    await plugin.prompt({ sessionID: "ses_b", prompt: { text: "b2" } });
    await settle(h);

    assert.match(h.events()[0], / env=%2 bind=unset$/);
    assert.match(h.events()[1], / env=%2 bind=unset$/);
    assert.match(h.events()[2], / env=%4 bind=unset$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    h.dispose();
  }
});

// The reclaim probe must keep accepting truncated stamps: a live binding
// whose pane title is the bound session's title truncated at terminal
// width is NOT stale. Over-tightening the match would let any new session
// steal a live pane whose stamp happens to be truncated.
test("a truncated stamp still proves a binding live (no false reclaim)", async () => {
  const h = await loadPlugin({
    panes:
      "%2\topencode\t/repo\tses_other\tOC | Something else entirely\n" +
      "%4\topencode\t/repo\tses_live\tOC | Fix opencode session status swap in T…\n",
    titles: {
      ses_other: "Something else entirely",
      ses_live: "Fix opencode session status swap in Tmux sidebar",
    },
  });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    const plugin = await h.init("/repo", undefined, "/repo");
    await plugin.prompt({ sessionID: "ses_new", prompt: { text: "go" } });
    await settle(h);

    // Neither binding is stale (%4's truncated stamp still matches its
    // bound session's full title), so the last resort is the uncommitted
    // first match — not a reclaim of %4.
    assert.match(h.events()[0], / env=%2 bind=0$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    h.dispose();
  }
});

// Turn scoping is per parent: with two mains, a child's tool result is
// kept while its own parent's turn runs — not while ANY turn runs.
test("a child's tool activity is gated on its own parent's turn", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.event({ type: "session.created", data: { sessionID: "ses_a", info: { directory: "/repo", title: "A" } } });
    await plugin.event({ type: "session.created", data: { sessionID: "ses_b", info: { directory: "/repo", title: "B" } } });
    await plugin.event({ type: "session.created", data: { sessionID: "ses_child_a", parentID: "ses_a", title: "ca" } });
    await plugin.event({ type: "session.created", data: { sessionID: "ses_child_b", parentID: "ses_b", title: "cb" } });
    await plugin.prompt({ sessionID: "ses_b", prompt: { text: "turn b" } });
    await plugin.tool({ sessionID: "ses_child_a", tool: "read", input: {}, status: "completed", result: { output: "o", metadata: null } });
    await plugin.tool({ sessionID: "ses_child_b", tool: "read", input: {}, status: "completed", result: { output: "o", metadata: null } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: "ses_a", source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: "ses_a", title: "A" }),
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: "ses_b", source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: "ses_b", title: "B" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: "ses_b", prompt: "turn b" }),
      // Kept (child_b's parent's turn is active) and routed under the
      // parent's id; child_a's result is dropped (its parent is idle).
      "activity-log " + JSON.stringify({ cwd: "/repo", session_id: "ses_b", tool_name: "read", tool_input: {}, tool_response: { output: "o", metadata: null } }),
    ]);
  } finally {
    h.dispose();
  }
});

// Regression: a project spans every worktree of the repo, so sibling-
// worktree TUIs all share one projectID and the project check alone passed
// every sibling session's events. Each instance then forwarded the foreign
// session with its own cwd, and resolution's single path match committed
// the foreign session's identity onto that instance's pane — the newest
// session took over every opencode row's title, binding, and run clock.
// Ownership is per directory: a session living in a sibling worktree must
// not be forwarded at all (its own instance owns it).
test("a sibling-worktree session sharing the project is never forwarded", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo/.worktrees/a", "proj_repo");
    // Same project, sibling directory: created, prompting, and turn
    // lifecycle must all be dropped.
    await plugin.event({
      type: "session.created",
      data: { sessionID: "ses_sibling", info: { projectID: "proj_repo", directory: "/repo/.worktrees/b", title: "Sibling" } },
    });
    await plugin.prompt({ sessionID: "ses_sibling", prompt: { text: "sibling turn" } });
    await plugin.event({ type: "session.execution.started", data: { sessionID: "ses_sibling" } });
    await plugin.event({ type: "session.execution.succeeded", data: { sessionID: "ses_sibling" } });
    // A session in THIS instance's directory still forwards normally.
    await plugin.event({
      type: "session.created",
      data: { sessionID: MAIN, info: { projectID: "proj_repo", directory: "/repo/.worktrees/a", title: "Mine" } },
    });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo/.worktrees/a", session_id: MAIN, source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo/.worktrees/a", session_id: MAIN, title: "Mine" }),
    ]);
  } finally {
    h.dispose();
  }
});

// Defense in depth for the same takeover: if a foreign session's event
// ever reaches resolution (a stale cache, an unattributable directory),
// the single path match must not COMMIT the binding when the pane's own
// title stamp proves it is still hosting its bound session — recording it
// would re-point the pane onto the foreign session and permanently
// misroute the session that actually lives here.
test("a foreign event onto a single path match is routed uncommitted when the pane's stamp proves its own binding", async () => {
  const h = await loadPlugin({
    panes: "%2\topencode\t/repo/.worktrees/a\tses_live\tOC | Live session title\n",
    titles: { ses_live: "Live session title", ses_foreign: "Foreign work" },
  });
  try {
    process.env.TMUX_RECORD_ENV = "1";
    const plugin = await h.init("/repo/.worktrees/a", undefined, "/repo/.worktrees/a");
    await plugin.prompt({ sessionID: "ses_foreign", prompt: { text: "go" } });
    await settle(h);

    // Routed (the event has to land somewhere) but flagged bind=0 so the
    // CLI never records ses_foreign's binding on ses_live's pane.
    assert.match(h.events()[0], / env=%2 bind=0$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    h.dispose();
  }
});
