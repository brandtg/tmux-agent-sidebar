// Regression tests for the OpenCode bridge's session gating.
//
// OpenCode runs every `task` subagent in a child session whose bus events
// and chat.message hook reach this plugin. The bridge must forward only
// the main session's lifecycle to the pane hooks, or a child's idle stops
// the elapsed clock mid-turn, a child's prompt restarts it, and a child's
// tool results re-arm a cleared clock after the turn's stop (a timer that
// never stops).
//
// Run: node --test .opencode/plugins/tmux-agent-sidebar.test.mjs
//
// The plugin resolves hook.sh by walking up from its own directory, so the
// harness copies it into a temp tree with a fake hook.sh that records every
// forwarded event instead of invoking the real binary. Each test gets a
// fresh temp tree, and therefore a fresh module instance with clean
// session-gating state.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

const PLUGIN_SRC = fileURLToPath(new URL("./tmux-agent-sidebar.js", import.meta.url));

// Load the plugin from a scratch tree and return a driver that feeds hook
// invocations and collects forwarded events from the fake hook.sh. Event
// lines are "<event-name> <payload-json>".
async function loadPlugin() {
  const root = mkdtempSync(join(tmpdir(), "oc-bridge-test-"));
  const deep = join(root, "a", "b", "plugins");
  mkdirSync(deep, { recursive: true });
  writeFileSync(deep ? join(deep, "tmux-agent-sidebar.js") : "", readFileSync(PLUGIN_SRC, "utf8"), { mode: 0o755 });
  writeFileSync(
    join(root, "hook.sh"),
    `#!/usr/bin/env bash\npayload=$(cat)\nprintf '%s %s\\n' "$2" "$payload" >> ${JSON.stringify(join(root, "events.log"))}\n`,
    { mode: 0o755 },
  );

  const mod = import(join(deep, "tmux-agent-sidebar.js"));
  return {
    async init(directory) {
      const { TmuxAgentSidebar } = await mod;
      const plugin = await TmuxAgentSidebar({ directory });
      // The hook spawns are fire-and-forget and their bash processes
      // append to the log in completion order. Serialize the driver with
      // a pause per invocation so the log reflects program order.
      return new Proxy(plugin, {
        get(target, prop) {
          const fn = target[prop];
          if (typeof fn !== "function") return fn;
          return async (...args) => {
            const result = await fn.apply(target, args);
            await new Promise((resolve) => setTimeout(resolve, 120));
            return result;
          };
        },
      });
    },
    events() {
      try {
        return readFileSync(join(root, "events.log"), "utf8").trim().split("\n").filter(Boolean);
      } catch {
        return [];
      }
    },
    dispose() {
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
    await plugin.event({ event: { type: "session.created", properties: { sessionID: MAIN, info: { title: "Main" } } } });
    await plugin["chat.message"]({ sessionID: MAIN }, { parts: [{ type: "text", text: "fix the bug" }] });
    // Subagent spins up: its own session + prompt + status + tools.
    await plugin.event({ event: { type: "session.created", properties: { sessionID: CHILD, info: { title: "explore (@explore subagent)", parentID: MAIN } } } });
    await plugin["chat.message"]({ sessionID: CHILD }, { parts: [{ type: "text", text: "subagent prompt" }] });
    await plugin.event({ event: { type: "session.status", properties: { sessionID: CHILD, status: { type: "busy" } } } });
    await plugin["tool.execute.after"]({ sessionID: CHILD, tool: "read" }, { title: "t", output: "o", metadata: null });
    await plugin.event({ event: { type: "session.idle", properties: { sessionID: CHILD } } });
    // Parent loop continues and finishes the turn.
    await plugin.event({ event: { type: "session.status", properties: { sessionID: MAIN, status: { type: "busy" } } } });
    await plugin.event({ event: { type: "session.idle", properties: { sessionID: MAIN } } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: MAIN, source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: MAIN, title: "Main" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "fix the bug" }),
      // The child's prompt, busy, and idle are all dropped; its mid-turn
      // tool result is kept (that is what the subagent is doing).
      "activity-log " + JSON.stringify({ cwd: "/repo", session_id: CHILD, tool_name: "read", tool_input: {}, tool_response: { title: "t", output: "o", metadata: null } }),
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
    await plugin["chat.message"]({ sessionID: MAIN }, { parts: [{ type: "text", text: "go" }] });
    await plugin.event({ event: { type: "session.status", properties: { sessionID: MAIN, status: { type: "busy" } } } });
    await plugin.event({ event: { type: "session.status", properties: { sessionID: MAIN, status: { type: "retry" }, next: 5 } } });
    await plugin.event({ event: { type: "session.status", properties: { sessionID: MAIN, status: { type: "busy" } } } });
    await plugin.event({ event: { type: "session.idle", properties: { sessionID: MAIN } } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: MAIN, last_message: "" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("busy without a chat.message still stamps the clock (retry recovery)", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.event({ event: { type: "session.status", properties: { sessionID: MAIN, status: { type: "busy" } } } });
    await plugin.event({ event: { type: "session.idle", properties: { sessionID: MAIN } } });
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
    await plugin["chat.message"]({ sessionID: MAIN }, { parts: [{ type: "text", text: "go" }] });
    await plugin.event({ event: { type: "session.created", properties: { sessionID: CHILD, info: { title: "bg", parentID: MAIN } } } });
    await plugin.event({ event: { type: "session.idle", properties: { sessionID: MAIN } } });
    // Everything below happens after the main turn's stop.
    await plugin.event({ event: { type: "session.status", properties: { sessionID: CHILD, status: { type: "busy" } } } });
    await plugin["chat.message"]({ sessionID: CHILD }, { parts: [{ type: "text", text: "bg prompt" }] });
    await plugin["tool.execute.after"]({ sessionID: CHILD, tool: "bash" }, { title: "t", output: "o", metadata: null });
    await plugin.event({ event: { type: "session.idle", properties: { sessionID: CHILD } } });
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
    await plugin["chat.message"]({ sessionID: MAIN }, { parts: [{ type: "text", text: "first" }] });
    await plugin.event({ event: { type: "session.idle", properties: { sessionID: MAIN } } });
    // User switches to another session (no session.created observed).
    await plugin["chat.message"]({ sessionID: "ses_other" }, { parts: [{ type: "text", text: "second" }] });
    await plugin.event({ event: { type: "session.idle", properties: { sessionID: "ses_other" } } });
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
    await plugin.event({ event: { type: "session.created", properties: { sessionID: MAIN, info: { title: "Main" } } } });
    await plugin["chat.message"]({ sessionID: MAIN }, { parts: [{ type: "text", text: "go" }] });
    await plugin.event({ event: { type: "permission.asked", properties: { sessionID: CHILD, id: "per_1" } } });
    await plugin.event({ event: { type: "session.idle", properties: { sessionID: MAIN } } });
    await plugin.event({ event: { type: "permission.asked", properties: { sessionID: CHILD, id: "per_2" } } });
    await plugin.event({ event: { type: "permission.asked", properties: { sessionID: MAIN, id: "per_3" } } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: MAIN, source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: MAIN, title: "Main" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }),
      "notification " + JSON.stringify({ cwd: "/repo", session_id: CHILD, wait_reason: "permission" }),
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
    await plugin.event({ event: { type: "session.created", properties: { sessionID: MAIN, info: { title: "Main" } } } });
    await plugin["chat.message"]({ sessionID: MAIN }, { parts: [{ type: "text", text: "go" }] });
    // Mid-turn child question passes (the whole turn is blocked).
    await plugin.event({ event: { type: "question.asked", properties: { sessionID: CHILD, id: "que_1" } } });
    await plugin.event({ event: { type: "session.idle", properties: { sessionID: MAIN } } });
    // Between turns a child question is dropped, a main question passes.
    await plugin.event({ event: { type: "question.asked", properties: { sessionID: CHILD, id: "que_2" } } });
    await plugin.event({ event: { type: "question.asked", properties: { sessionID: MAIN, id: "que_3" } } });
    await settle(h);

    assert.deepEqual(h.events(), [
      "session-start " + JSON.stringify({ cwd: "/repo", session_id: MAIN, source: "startup" }),
      "session-title " + JSON.stringify({ cwd: "/repo", session_id: MAIN, title: "Main" }),
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }),
      "notification " + JSON.stringify({ cwd: "/repo", session_id: CHILD, wait_reason: "question" }),
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
    await plugin["chat.message"]({ sessionID: MAIN }, { parts: [{ type: "text", text: "go" }] });
    await plugin.event({ event: { type: "permission.replied", properties: { sessionID: MAIN, requestID: "per_1", reply: "once" } } });
    await plugin.event({ event: { type: "question.replied", properties: { sessionID: MAIN, requestID: "que_1" } } });
    await plugin.event({ event: { type: "question.rejected", properties: { sessionID: MAIN, requestID: "que_2" } } });
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

test("session.error ends the turn and re-arms gating for the next one", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin["chat.message"]({ sessionID: MAIN }, { parts: [{ type: "text", text: "go" }] });
    await plugin.event({ event: { type: "session.error", properties: { sessionID: MAIN, error: { message: "boom" } } } });
    await plugin["chat.message"]({ sessionID: MAIN }, { parts: [{ type: "text", text: "again" }] });
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

test("child titles never take over the row", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.event({ event: { type: "session.created", properties: { sessionID: MAIN, info: { title: "Main" } } } });
    await plugin.event({ event: { type: "session.updated", properties: { sessionID: CHILD, info: { title: "child", parentID: MAIN } } } });
    await plugin.event({ event: { type: "session.updated", properties: { sessionID: MAIN, info: { title: "Renamed" } } } });
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
