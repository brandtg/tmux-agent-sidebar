#!/usr/bin/env bash
# Single-frame snapshot of the fullscreen agent overview grid — the
# `overview` subcommand's in-pane TUI, the exact render the fullscreen
# tmux popup shows (the popup itself is not capturable; the TUI draws
# the same grid into a plain pane).
#
# Five agent panes from the shared layout; every pane gets a painted
# stream so each grid cell shows real transcript content, not a blank
# capture.
#
# Usage:  scenario.sh <output-dir> [extra capture args...]

set -euo pipefail

OUT="${1:?usage: scenario.sh <output-dir> [extra capture args...]}"
shift
EXTRA_ARGS=("$@")

source "$(cd "$(dirname "$0")/../common" && pwd)/_lib.sh"

setup "overview-src"
trap cleanup EXIT

mkdir -p "$OUT"

# Five agents, like the hero — the grid shows the full spread of
# statuses (running/waiting/running/background/error).
export HERMES=1

build_layout
paint_stream "$MAIN_PANE" \
    "$ROOT/fixtures/scenarios/hero/main-pane.stream"
paint_stream "$PANE_WAITING" \
    "$ROOT/fixtures/scenarios/overview/codex-permission.stream"
paint_stream "$PANE_HERMES" \
    "$ROOT/fixtures/scenarios/overview/hermes-edit.stream"
paint_stream "$PANE_BACKGROUND" \
    "$ROOT/fixtures/scenarios/overview/opencode-dev.stream"
paint_stream "$PANE_ERROR" \
    "$ROOT/fixtures/scenarios/overview/error-ratelimit.stream"

# Dedicated fullscreen session — the overview popup is 100%×100% of the
# client, so the in-pane TUI gets the full 140×46 canvas.
tmux new-session -d -s overview -x 140 -y 46 -c "${SCENARIO_CWD:-$ROOT}"
UI_WINDOW=$(tmux display-message -p -t overview '#{window_id}')
UI_PANE=$(tmux list-panes -t "$UI_WINDOW" -F '#{pane_id}' | head -n 1)

# Run the bare `overview` TUI (no --popup: a plain pane is what the
# capture pipeline can see). The first grid refresh — listing, process
# snapshot, and one capture-pane per cell — needs a beat over the 500 ms
# default interval.
tmux send-keys -t "$UI_PANE" "$BIN overview" Enter
sleep 2.5

# Capture the overview window, not the agent layout.
SESSION=overview
WINDOW=$UI_WINDOW
capture_single
