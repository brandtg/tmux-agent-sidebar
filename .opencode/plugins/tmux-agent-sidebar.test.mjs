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

async function loadPlugin({ panes = "" } = {}) {
  const root = mkdtempSync(join(tmpdir(), "oc-bridge-test-"));
  const deep = join(root, "a", "b", "plugins");
  mkdirSync(deep, { recursive: true });
  writeFileSync(join(deep, "tmux-agent-sidebar.js"), readFileSync(PLUGIN_SRC, "utf8"), { mode: 0o755 });
  writeFileSync(
    join(root, "hook.sh"),
    `#!/usr/bin/env bash
payload=$(cat)
if [ "\${TMUX_RECORD_ENV-}" = "1" ]; then
  printf '%s %s env=%s\\n' "$2" "$payload" "\${TMUX_PANE-unset}" >> ${JSON.stringify(join(root, "events.log"))}
else
  printf '%s %s\\n' "$2" "$payload" >> ${JSON.stringify(join(root, "events.log"))}
fi
`,
    { mode: 0o755 },
  );
  // Shadow tmux with a script printing the canned pane snapshot, so the
  // bridge's pane resolution is exercised without a real tmux server.
  const bindir = join(root, "bin");
  mkdirSync(bindir, { recursive: true });
  writeFileSync(join(bindir, "tmux"), `#!/usr/bin/env sh\ncat <<'PANES'\n${panes}PANES\n`, { mode: 0o755 });
  const savedPath = process.env.PATH;
  process.env.PATH = `${bindir}:${savedPath ?? ""}`;

  const mod = import(pathToFileURL(join(deep, "tmux-agent-sidebar.js")));
  // Setup's cleanup (set by init) aborts the bus subscription; without it
  // the fake subscribe's poll timer would keep the event loop alive.
  let cleanup = null;
  return {
    async init(directory) {
      const { default: definition } = await mod;

      // Fake plugin context: capture registered hooks, queue bus events.
      const promptHooks = [];
      const toolHooks = [];
      const queue = [];
      const ctx = {
        location: { directory },
        session: {
          hook: async (name, cb) => {
            if (name === "prompt") promptHooks.push(cb);
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
      // tool result is kept (that is what the subagent is doing).
      "activity-log " + JSON.stringify({ cwd: "/repo", session_id: CHILD, tool_name: "read", tool_input: {}, tool_response: { output: "o", metadata: null } }),
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
      "notification " + JSON.stringify({ cwd: "/repo", session_id: CHILD, wait_reason: "permission" }),
      "stop " + JSON.stringify({ cwd: "/repo", session_id: MAIN, last_message: "" }),
      "notification " + JSON.stringify({ cwd: "/repo", session_id: MAIN, wait_reason: "permission" }),
    ]);
  } finally {
    h.dispose();
  }
});

test("question form asks map to a waiting notification like permissions", async () => {
  const h = await loadPlugin();
  try {
    const plugin = await h.init("/repo");
    await plugin.event({ type: "session.created", data: { sessionID: MAIN, title: "Main" } });
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    // Mid-turn child question passes (the whole turn is blocked).
    await plugin.event({ type: "form.created", data: { form: { sessionID: CHILD, id: "que_1", title: "q", fields: [] } } });
    await plugin.event({ type: "session.idle", data: { sessionID: MAIN } });
    // Between turns a child question is dropped, a main question passes.
    await plugin.event({ type: "form.created", data: { form: { sessionID: CHILD, id: "que_2", title: "q", fields: [] } } });
    await plugin.event({ type: "form.created", data: { form: { sessionID: MAIN, id: "que_3", title: "q", fields: [] } } });
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
    await plugin.prompt({ sessionID: MAIN, prompt: { text: "go" } });
    await plugin.event({ type: "permission.replied", data: { sessionID: MAIN, requestID: "per_1", reply: "once" } });
    await plugin.event({ type: "form.replied", data: { sessionID: MAIN, id: "que_1", answer: {} } });
    await plugin.event({ type: "form.cancelled", data: { sessionID: MAIN, id: "que_2" } });
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

test("child renames never take over the row", async () => {
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

    // %2 is an opencode pane but in the wrong directory; the inherited %1
    // still exists in the pane table, so it is kept — the bridge cannot
    // know better (this is what makes plain in-pane setups keep working).
    assert.deepEqual(h.events(), [
      "user-prompt-submit " + JSON.stringify({ cwd: "/repo", session_id: MAIN, prompt: "go" }) + " env=%1",
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

    assert.match(h.events()[0], / env=%2$/);
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

    assert.match(h.events()[0], / env=%7$/);
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
    assert.match(h.events()[0], / env=%4$/);
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
    assert.match(h.events()[0], / env=%5$/);
  } finally {
    delete process.env.TMUX_RECORD_ENV;
    delete process.env.TMUX_PANE;
    h.dispose();
  }
});
