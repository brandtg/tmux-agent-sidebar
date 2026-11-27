#!/usr/bin/env bash
# Single-frame snapshot of the quick switcher — the `switch` subcommand's
# in-pane TUI, the exact render a `--popup` invocation shows inside its
# tmux popup (the popup itself is not capturable; the TUI draws the same
# frame into a plain pane).
#
# The window is sized to the switcher's default popup geometry (80%×60%
# of the 140×46 canvas ≈ 112×28) so the capture matches what users see.
#
# Usage:  scenario.sh <output-dir> [extra capture args...]

set -euo pipefail

OUT="${1:?usage: scenario.sh <output-dir> [extra capture args...]}"
shift
EXTRA_ARGS=("$@")

source "$(cd "$(dirname "$0")/../common" && pwd)/_lib.sh"

setup "switcher-src"
trap cleanup EXIT

mkdir -p "$OUT"

# Five agents, like the hero — the switcher reads server-wide, so the
# list shows the full spread of statuses (running/waiting/running/
# background/error).
export HERMES=1

# The agent panes the switcher lists come from the shared layout; the
# sidebar pane itself is not needed here but costs nothing.
build_layout
paint_stream "$MAIN_PANE" \
    "$ROOT/fixtures/scenarios/hero/main-pane.stream"

# Alt-tab story: the MRU says the last jumps were to the waiting codex
# (then hermes), so the switcher's default selection — first entry that
# is not the pane you are on — lands on the codex row and its attention
# flash rides the selection.
tmux set-option -g @sidebar_switch_mru "$PANE_WAITING $PANE_HERMES"

# Dedicated small session on the same isolated server (the switcher
# lists agent panes server-wide), sized to the switcher's default popup
# geometry: 80% × 60% of the 140×46 hero canvas ≈ 112×28. Detached
# sessions honor -x/-y, so the TUI fills exactly the popup-sized frame.
tmux new-session -d -s switcher -x 112 -y 28 -c "${SCENARIO_CWD:-$ROOT}"
UI_WINDOW=$(tmux display-message -p -t switcher '#{window_id}')
UI_PANE=$(tmux list-panes -t "$UI_WINDOW" -F '#{pane_id}' | head -n 1)

# Run the bare `switch` TUI (no --popup: a plain pane is what the
# capture pipeline can see). One refresh tick (~1 s) fills the list.
tmux send-keys -t "$UI_PANE" "$BIN switch" Enter
sleep 1.5

# Capture the switcher window.
SESSION=switcher
WINDOW=$UI_WINDOW
capture_single
