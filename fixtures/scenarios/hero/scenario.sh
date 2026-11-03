#!/usr/bin/env bash
# Hero: one-shot single-frame capture of the v1.3 release-prep scene —
# five agents on one repo (claude/codex/hermes/opencode/error), the
# manual TODO panel open, and the focused Claude transcript filling the
# right pane to the bottom row.
#
# Usage:  scenario.sh <output-dir> [extra capture args...]

set -euo pipefail

OUT="${1:?usage: scenario.sh <output-dir> [extra capture args...]}"
shift
EXTRA_ARGS=("$@")

source "$(cd "$(dirname "$0")/../common" && pwd)/_lib.sh"

# Focus the Claude pane so the right side is a full transcript (the
# star row: task progress + subagent tree, mirrored by the stream).
export FOCUS=MAIN_PANE
# Fifth pane: hermes — the fork's headline addition.
export HERMES=1

setup "hero"
trap cleanup EXIT

mkdir -p "$OUT"

build_layout
paint_stream "$MAIN_PANE" \
    "$ROOT/fixtures/scenarios/hero/main-pane.stream"

# Claude's activity log — the row's task progress renders from the
# TaskCreate/TaskUpdate lines: 2 of 4 done, the third underway.
cat >> "$MAIN_LOG" <<'ACTIVITY'
10:12|TaskCreate|#1 reproduce the flake locally
10:12|TaskCreate|#2 derive the seed from the run ID
10:12|TaskCreate|#3 add a regression test for thread ordering
10:12|TaskCreate|#4 backport the seed fix to v1.2
10:31|TaskUpdate|completed #1
10:47|TaskUpdate|completed #2
11:03|TaskUpdate|in_progress #3
ACTIVITY

# Hermes' activity log — the post_tool_call entry behind the row's
# `Edit (src/api/routes.ts, +40 −12)` activity line.
cat >> "$HERMES_LOG" <<'ACTIVITY'
11:21|Edit|src/api/routes.ts (+40 −12)
ACTIVITY

# The manual TODO list — "your checklist tracking their work" is the
# frame's narrative trick. Two of five are done; the default
# hide-completed keeps them out of the panel, so the header reads
# `todos · 3/5 · hide done` and tells that story without strikethrough
# rows.
mkdir -p "$XDG_STATE_HOME/tmux-agent-sidebar"
cat > "$XDG_STATE_HOME/tmux-agent-sidebar/todos.txt" <<'TODOS'
[✓] reproduce the nightly flake locally
[✓] derive the seed from the run ID
[ ] verify the seed fix on v1.2
[ ] review hermes' typed-client migration
[ ] cut the v1.3 release notes
TODOS

# Open the TODO panel (what pressing `t` in the sidebar does). The
# sidebar reads shared bool options as "1"/"0", not tmux's on/off.
tmux set-option -g @sidebar_todo_visible 1

start_sidebar

capture_single
