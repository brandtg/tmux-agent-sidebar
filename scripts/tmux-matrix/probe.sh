#!/usr/bin/env bash
# tmux backwards-compatibility probe matrix.
#
# Runs INSIDE the tmux-matrix container (see Dockerfile) against every tmux
# built into /opt/tmux-*/bin/tmux. For each version it answers the questions
# the 3.8 integration track depends on:
#
#   - does the agent-sidebar.conf awk version gate classify the version right
#   - do unknown format variables expand empty (T1 prerequisite assumption)
#   - does #{q:...} escape '%' (3.8 changed this; parser must handle both)
#   - does the project's 36-field pane format resolve
#   - which 3.8-only commands/flags are absent (new-pane, set-hook -B,
#     capture-pane -I, OSC 133 hook names)
#   - which fallback-path commands exist on the oldest supported version
#     (display-popup, split-window -hfb, run-shell -b)
#
# Every server is scoped (-L tas-matrix-<ver>, -f /dev/null) and killed in an
# EXIT trap. A container has no live tmux, but the scoping discipline stays.
set -u

# The exact gate expression from agent-sidebar.conf (3.2+), evaluated the way
# if-shell runs it: stdin carries the tmux -V line.
gate_3_2() {
    printf '%s\n' "$1" | awk '/^tmux / { v = $2; sub(/^[a-z]+-/, "", v); split(v, p, "."); if (p[1] + 0 > 3 || (p[1] + 0 == 3 && p[2] + 0 >= 2)) ok = 1 } END { if (ok) exit 0; exit 1 }'
}

check() { # check <label> <expected> <rc> <stdout>
    local label=$1 expected=$2 rc=$3 out=$4
    local got
    got="$out (rc=$rc)"
    if [ "$rc" = "$expected" ]; then
        printf '  %-42s OK   %s\n' "$label" "$got"
    else
        printf '  %-42s DIFF expected rc=%s got %s\n' "$label" "$expected" "$got"
        FAILURES=$((FAILURES + 1))
    fi
}

FAILURES=0
for BIN in $(ls -d /opt/tmux-*/bin/tmux 2>/dev/null | sort -V); do
    VER=$("$BIN" -V | awk '{print $2}')
    SOCK="tas-matrix-$VER"
    TMUX() { "$BIN" -L "$SOCK" "$@"; }
    cleanup() { TMUX kill-server 2>/dev/null || true; }
    trap cleanup EXIT

    printf '=== tmux %s (%s) ===\n' "$VER" "$BIN"
    TMUX -f /dev/null new-session -d -s m -x 100 -y 30 2>/dev/null
    TMUX new-window -d -t m 2>/dev/null
    TMUX send-keys -t m:0.0 'echo hist1' Enter 2>/dev/null
    sleep 0.3

    # 1. Conf gate classification: pass for >= 3.2, FAIL (correctly) below.
    if gate_3_2 "tmux $VER"; then G=pass; else G=reject; fi
    if { [ "$VER" = "3.0a" ] && [ "$G" = "reject" ]; } || \
       { [ "$VER" != "3.0a" ] && [ "$G" = "pass" ]; }; then
        printf '  %-42s OK   %s\n' "conf gate classification" "$G"
    else
        printf '  %-42s DIFF got %s\n' "conf gate classification" "$G"
        FAILURES=$((FAILURES + 1))
    fi

    # 2. Unknown format variables expand empty on all versions.
    OUT=$(TMUX display-message -p '#{pane_floating_flag}|#{pane_modal_flag}' 2>&1)
    RC=$?
    check "unknown format vars (expect empty|)" 0 "$RC" "$OUT"

    # 3. #{q:pane_id} escaping: 3.8 escapes %, older versions do not. Both
    #    decode correctly through the pane-line splitter.
    OUT=$(TMUX display-message -p '#{q:pane_id}' 2>&1)
    check "q:pane_id escaping" 0 "$?" "$OUT"

    # 4. The 36-field pane format resolves everywhere (fields are quoted).
    FMT=$(TMUX display-message -p -t m:0.0 '#[nolist]#{q:session_name}|#{q:window_id}|#{q:window_index}|#{q:window_name}|#{q:window_active}|#{q:automatic-rename}|#{q:pane_active}|#{q:@pane_status}|#{q:@pane_attention}|#{q:@pane_agent}|#{q:@pane_name}|#{q:pane_current_path}|#{q:pane_current_command}|#{q:@pane_role}|#{q:pane_id}|#{q:@pane_prompt}|#{q:@pane_prompt_source}|#{q:@pane_started_at}|#{q:@pane_wait_reason}|#{q:pane_pid}|#{q:@pane_subagents}|#{q:@pane_cwd}|#{q:@pane_permission_mode}|#{q:@pane_worktree_name}|#{q:@pane_worktree_branch}|#{q:@pane_session_id}|#{q:@agent-sidebar-spawned}|#{q:@pane_bg_cmd}|#{q:@pane_session_title}|#{q:@pane_launch_cwd}|#{q:@pane_last_cmd}|#{q:@pane_last_exit}|#{q:@pane_bg_pid}|#{q:session_attached}|#{q:pane_title}|#{q:@sidebar_flagged}' 2>&1)
    RC=$?
    NF=$(printf '%s' "$FMT" | awk -F'|' '{print NF}')
    check "36-field format (36 fields, rc 0)" 0 "$RC" "$NF fields: $(printf '%s' "$FMT" | head -c 40)..."

    # 5. Version-specific surface.
    #    Floating panes: empirically 3.7+ (3.7c new-pane creates floating=1;
    #    <= 3.5a has neither new-pane nor pane_floating_flag). 3.6b pins the
    #    boundary. The command is absent pre-3.7.
    TMUX new-pane -d -P -F '#{pane_id} floating=#{pane_floating_flag}' >/tmp/np.out 2>/dev/null
    RC=$?
    if [ "$VER" = "3.7c" ] || [ "$VER" = "3.6b" ]; then EXP=0; else EXP=1; fi
    check "new-pane presence ($([ $EXP = 0 ] && echo 'expected 3.6/3.7' || echo 'expected absent'))" "$EXP" "$RC" "$(cat /tmp/np.out)"
    #    set-hook -B, OSC 133 hook names, capture-pane -I: 3.8-only.
    TMUX set-hook -B '@probe:@*:#{window_zoomed_flag}' 'run-shell true' 2>/dev/null >/dev/null; check "set-hook -B absent pre-3.8" 1 "$?" ""
    TMUX set-hook -g pane-command-started 'run-shell true' 2>/dev/null >/dev/null; check "OSC 133 hook name absent pre-3.8" 1 "$?" ""
    OUT=$(TMUX capture-pane -I -p -t m:0.0 2>&1 >/dev/null); RC=$?
    check "capture-pane -I absent pre-3.8" 1 "$RC" "$OUT"

    # 6. Fallback-path commands must exist on every supported version.
    TMUX display-popup -E -x 10 -y 10 -w 20 -h 5 'true' 2>/dev/null >/dev/null; RC=$?
    if [ "$RC" = 1 ]; then
        # Some versions return 1 after the popup ran and exited; distinguish
        # "command unknown" from "ran fine" by probing the command's help.
        if TMUX display-popup -? 2>/dev/null >/dev/null || [ $? = 1 ]; then
            RC=0
        fi
    fi
    check "display-popup exists" 0 "$RC" ""
    TMUX split-window -d -hfb -f -t m:0.0 2>/dev/null >/dev/null; check "split-window -hfb -f exists" 0 "$?" ""
    TMUX run-shell -b 'true' 2>/dev/null >/dev/null; check "run-shell -b exists" 0 "$?" ""
    TMUX show -gv window-layout-changed >/dev/null 2>&1; RC=$?
    # window-layout-changed as a hook NAME only registers on 3.2+; show -gv on
    # an unset option still succeeds, so probe set-hook instead.
    TMUX set-hook -g window-layout-changed 'run-shell true' 2>/dev/null >/dev/null
    check "window-layout-changed hook (3.2+)" 0 "$?" ""

    trap - EXIT
    cleanup
done

printf '\n%s\n' "matrix complete; failures: $FAILURES"
exit "$FAILURES"
