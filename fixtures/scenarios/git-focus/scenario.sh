#!/usr/bin/env bash
# Single-frame snapshot cropped to the Repos tab at the bottom.
#
# To demonstrate the Repos row (agent, branch, ahead/behind, ~dirty),
# every seeded agent pane is pointed at a purpose-built throwaway repo:
#   - feat/login-redirect is 3 commits ahead of origin
#   - origin/feat/login-redirect is 1 commit ahead of the merge base
#   - three staged (A/M/D), three unstaged (M/M/D), three untracked
#     -> ~9 dirty
#
# The repo lives under $TMUX_DIR and is wiped by cleanup.

set -euo pipefail

OUT="${1:?usage: scenario.sh <output-dir> [extra capture args...]}"
shift
EXTRA_ARGS=("$@")

source "$(cd "$(dirname "$0")/../common" && pwd)/_lib.sh"

export FOCUS=PANE_WAITING
# Crop to the sidebar's bottom panel (same as activity-focus).
export CROP_ROWS=26:46
export CROP_COLS=0:46

setup "git-focus"
trap cleanup EXIT

mkdir -p "$OUT"

# ─── fake repo ────────────────────────────────────────────────────
FAKE_REPO="$TMUX_DIR/repo"
mkdir -p "$FAKE_REPO"

# -c flags inline so we don't touch the user's git config, and keep
# commits fast / signatureless regardless of the host's gpg setup.
GC=(-c commit.gpgsign=false -c tag.gpgsign=false -c user.name=tas -c user.email=tas@local)

(
    cd "$FAKE_REPO"
    git "${GC[@]}" init -q -b main
    git "${GC[@]}" commit --allow-empty -q -m "M0: init"
    echo "# login redirect fix" > README.md
    git "${GC[@]}" add README.md
    git "${GC[@]}" commit -q -m "add README"

    # Feature branch: 2 commits ahead of origin.
    git "${GC[@]}" checkout -q -b feat/login-redirect
    mkdir -p src/auth src/middleware
    printf 'export function handleRedirect() {\n  return null\n}\n' > src/auth/redirect.ts
    printf 'export function validateState() {\n  return true\n}\n'  > src/auth/state.ts
    printf 'export function withSession() {}\n'                     > src/auth/session.ts
    printf 'export function logout() {}\n'                          > src/middleware/logout.ts
    git "${GC[@]}" add src/auth src/middleware
    git "${GC[@]}" commit -q -m "scaffold redirect + session"
    printf 'export const NONCE_TTL_MS = 5 * 60 * 1000\n' > src/auth/nonce.ts
    git "${GC[@]}" add src/auth/nonce.ts
    git "${GC[@]}" commit -q -m "nonce TTL constant"

    # Ignore local-only artefacts so they don't show up as untracked
    # and push the showcase files off the visible rows.
    printf 'node_modules/\ndist/\n' > .gitignore
    git "${GC[@]}" add .gitignore
    git "${GC[@]}" commit -q -m "add .gitignore"

    # A divergent commit on main becomes origin/feat/login-redirect so
    # the branch is 1 behind as well. No real remote is needed; the
    # update-ref creates the remote-tracking ref directly.
    git "${GC[@]}" checkout -q main
    echo "docs stub" > CHANGES.md
    git "${GC[@]}" add CHANGES.md
    git "${GC[@]}" commit -q -m "O1: docs"
    ORIGIN_SHA=$(git rev-parse HEAD)
    git "${GC[@]}" checkout -q feat/login-redirect

    # Git refuses to resolve @{upstream} unless the branch's configured
    # remote actually exists in the repo config — even if the remote
    # ref itself is already in place. A placeholder URL (/dev/null)
    # plus the standard fetch refspec is enough to satisfy that.
    git "${GC[@]}" remote add origin /dev/null
    git "${GC[@]}" config remote.origin.fetch '+refs/heads/*:refs/remotes/origin/*'
    git "${GC[@]}" update-ref refs/remotes/origin/feat/login-redirect "$ORIGIN_SHA"
    git "${GC[@]}" config branch.feat/login-redirect.remote origin
    git "${GC[@]}" config branch.feat/login-redirect.merge refs/heads/feat/login-redirect

    # Staged: A (added), M (modified), D (deleted).
    printf '# login redirect fix\n\nPreserve query string on OAuth redirect.\n' > README.md
    git "${GC[@]}" add README.md
    printf 'export const CSP_DIRECTIVES = {}\n' > src/auth/csp.ts
    git "${GC[@]}" add src/auth/csp.ts
    git "${GC[@]}" rm -q src/auth/nonce.ts

    # Unstaged: M × 2 + D × 1 (tracked file removed without staging).
    printf '\n// TODO: preserve query string across the redirect\n' >> src/auth/redirect.ts
    printf '\n// TODO: reject expired nonces\n'                      >> src/auth/state.ts
    rm src/middleware/logout.ts

    # Untracked: a handful of new files, one nested.
    mkdir -p tests docs
    printf "describe('redirect', () => {})\n" > tests/redirect.test.ts
    printf "describe('session', () => {})\n"  > tests/session.test.ts
    echo "# Known issues"                     > NOTES.md
    echo "## Migration notes"                 > docs/MIGRATION.md
)

build_layout

tmux set-environment -g PATH "$PATH"
paint_stream "$MAIN_PANE" \
    "$ROOT/fixtures/scenarios/hero/main-pane.stream"

# Point the focused pane's repo lookup at the fake repo. The resolver
# keys off the pane's working directory, so respawn the pane in the
# fake repo's dir to update pane_current_path in place. Its seeded
# @pane_worktree_branch is cleared so the resolved branch shows.
tmux respawn-pane -k -t "$PANE_WAITING" -c "$FAKE_REPO" \
    "$FAKE_BIN_DIR/codex 999999"
tmux set-option -t "$PANE_WAITING" -p -u @pane_worktree_branch

# Point the remaining seeded panes at the fake repo too, and clear
# their seeded @pane_worktree_branch overrides — otherwise the Repos
# rows would mix those labels with this checkout's real branch and
# dirty state, which drifts with whatever is uncommitted when the
# capture runs. With this every row reads fix/login-redirect ↑3 ↓1 ~9.
tmux respawn-pane -k -t "$MAIN_PANE" -c "$FAKE_REPO" \
    "$FAKE_BIN_DIR/claude 999999"
tmux respawn-pane -k -t "$PANE_BACKGROUND" -c "$FAKE_REPO" \
    "$FAKE_BIN_DIR/opencode 999999"
tmux respawn-pane -k -t "$PANE_ERROR" -c "$FAKE_REPO" \
    "$FAKE_BIN_DIR/claude 999999"
for pane in "$MAIN_PANE" "$PANE_BACKGROUND" "$PANE_ERROR"; do
    tmux set-option -t "$pane" -p -u @pane_worktree_branch
done

# Launch the sidebar via respawn-pane so it starts with a clean env
# (bypassing the user's fish login shell).
tmux respawn-pane -k -t "$SIDEBAR_PANE" "$BIN"
sleep 2.0

# Switch the bottom panel to the Repos tab and give the git-info
# resolver a beat to fill in branch, movement, and dirty count.
tmux send-keys -t "$SIDEBAR_PANE" BTab
sleep 1.5

capture_single
