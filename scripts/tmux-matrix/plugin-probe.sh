#!/usr/bin/env bash
# tmux-agent-sidebar PLUGIN compatibility matrix.
#
# Unlike probe.sh (tmux behavior only), this runs the actual plugin — the
# release binary and the real agent-sidebar.conf — against every tmux the
# plugin claims to support, exercising the four compat surfaces:
#
#   1. conf load      — the real agent-sidebar.conf parses and registers
#                       hooks on this tmux (with @agent_sidebar_bin pre-set,
#                       as the entry script would).
#   2. toggle         — the split-sidebar open path works: @pane_role lands.
#   3. TUI runtime    — the binary runs as a pane process on this tmux,
#                       renders a frame, and writes @sidebar_pid (proves
#                       pane-option writes work).
#   4. hook pipeline  — `hook claude SessionStart` with a real payload
#                       writes @pane_agent=claude (proves the read/parse/
#                       write loop against this server).
#
# Every server is scoped (-L tas-plugin-<ver>, -f /dev/null + prelude) and
# killed in an EXIT trap.
set -u

BIN=/usr/local/bin/tmux-agent-sidebar
CONF=/opt/plugin/agent-sidebar.conf
WORKDIR=/tmp/probe
mkdir -p "$WORKDIR"

PRELUDE="$WORKDIR/prelude.conf"
cat > "$PRELUDE" <<EOF
set -g @agent_sidebar_bin "$BIN"
set -g @sidebar_auto_create off
source-file "$CONF"
EOF

cat > "$WORKDIR/payload.json" <<'EOF'
{"session_id":"probe-session","transcript_path":"/tmp/probe/none.jsonl","cwd":"/tmp/probe","hook_event_name":"SessionStart","source":"startup"}
EOF

FAILURES=0
check() { # check <label> <ok:0|1> <detail>
    if [ "$2" = 0 ]; then
        printf '  %-44s OK   %s\n' "$1" "$3"
    else
        printf '  %-44s FAIL %s\n' "$1" "$3"
        FAILURES=$((FAILURES + 1))
    fi
}

printf 'plugin binary: %s\n' "$("$BIN" --version 2>/dev/null | head -1)"

for TMUXBIN in $(ls -d /opt/tmux-*/bin/tmux 2>/dev/null | sort -V); do
    VER=$("$TMUXBIN" -V | awk '{print $2}')
    SOCK="tas-plugin-$VER"
    T() { "$TMUXBIN" -L "$SOCK" "$@"; }
    cleanup() { T kill-server 2>/dev/null || true; }
    trap cleanup EXIT

    # The plugin shells out to bare `tmux` (src/tmux/commands.rs) and finds
    # the server via $TMUX. Replicate a real install: the version's binary
    # first on PATH, and $TMUX pointing at the scoped server for out-of-pane
    # invocations (toggle/hook run from this shell, not from inside a pane).
    export PATH="/opt/tmux-$VER/bin:$PATH"
    export TMUX="/tmp/tmux-$(id -u)/$SOCK"

    printf '=== plugin on tmux %s ===\n' "$VER"

    # 1. Conf load — the whole real conf, stderr must be empty.
    ERRFILE="$WORKDIR/conf-$VER.err"
    T -f "$PRELUDE" new-session -d -s p -x 120 -y 40 2>"$ERRFILE"
    RC=$?
    # tmux <= 3.2a ignores -x/-y for detached sessions (window snaps to the
    # 80x24 default until a client attaches); resize-window pins it on every
    # version. Without this the toggle legitimately takes popup mode on a
    # window that measures 80 wide (< @sidebar_popup_max_width).
    T resize-window -x 120 -y 40 -t p:0 >/dev/null 2>&1
    DETAIL="rc=$RC"
    [ -s "$ERRFILE" ] && DETAIL="$DETAIL stderr: $(head -c 200 "$ERRFILE")"
    [ "$RC" = 0 ] && [ ! -s "$ERRFILE" ]; check "conf parses + loads clean" $? "$DETAIL"
    HOOKS=$(T show-hooks -g 2>/dev/null | grep -c "after-select-window\|window-layout-changed")
    [ "${HOOKS:-0}" -ge 1 ]; check "conf-registered hooks present" $? "$HOOKS hook line(s)"

    # 2. Toggle — split sidebar opens, role lands.
    T send-keys -t p:0.0 'cd /tmp/probe' Enter 2>/dev/null
    sleep 0.3
    "$BIN" toggle p:0 /tmp/probe 320 >/dev/null 2>&1
    RC=$?
    ROLES=$(T list-panes -t p:0 -F '#{q:@pane_role}' 2>/dev/null | grep -c sidebar)
    [ "$RC" = 0 ] && [ "${ROLES:-0}" -ge 1 ]
    check "toggle creates @pane_role=sidebar split" $? "toggle rc=$RC, $ROLES sidebar pane(s)"
    SID=$(T list-panes -t p:0 -F '#{pane_id} #{q:@pane_role}' 2>/dev/null | grep sidebar | head -1 | awk '{print $1}')

    # 3. TUI runtime — real binary as a pane process on this server.
    #    new-pane only exists on tmux 3.7+; split-window covers every
    #    version (and is what the plugin's own open path uses).
    TUIPANE=$(T split-window -d -t p:0 -P -F '#{pane_id}' -c /tmp/probe "$BIN" 2>/dev/null)
    [ -z "$TUIPANE" ] && TUIPANE=$(T new-pane -d -t p:0 -P -F '#{pane_id}' -c /tmp/probe "$BIN" 2>/dev/null)
    sleep 2
    ALIVE=$(T list-panes -t p:0 -F '#{pane_id}' 2>/dev/null | grep -c "^$TUIPANE$")
    PIDOPT=$(T display-message -p -t "$TUIPANE" '#{q:@sidebar_pid}' 2>/dev/null)
    FRAME=$(T capture-pane -p -t "$TUIPANE" 2>/dev/null)
    FRAMELINES=$(printf '%s' "$FRAME" | grep -cv '^$')
    HEADER=$(printf '%s' "$FRAME" | grep -c '☰\|ⓘ')
    [ "$ALIVE" = 1 ] && [ -n "$PIDOPT" ] && [ "${FRAMELINES:-0}" -ge 1 ] && [ "${HEADER:-0}" -ge 1 ]
    check "TUI runs, renders header, writes @sidebar_pid" $? "alive=$ALIVE pid=$PIDOPT lines=$FRAMELINES header-glyphs=$HEADER"

    # 4. Hook pipeline — real payload through the real adapter.
    #    hooks.json maps the upstream trigger to the internal kebab-case
    #    name (e.g. SessionStart -> session-start); the CLI takes the
    #    internal name, exactly as the real wiring invokes it.
    TMUX_PANE="$TUIPANE" "$BIN" hook claude session-start < "$WORKDIR/payload.json" >/dev/null 2>&1
    RC=$?
    AGENT=$(T display-message -p -t "$TUIPANE" '#{q:@pane_agent}' 2>/dev/null)
    STATUS=$(T display-message -p -t "$TUIPANE" '#{q:@pane_status}' 2>/dev/null)
    [ "$RC" = 0 ] && [ "$AGENT" = "claude" ]
    check "hook claude session-start writes @pane_agent" $? "rc=$RC agent=[$AGENT] status=[$STATUS]"

    trap - EXIT
    cleanup
    unset TMUX
done

printf '\n%s\n' "plugin matrix complete; failures: $FAILURES"
exit "$FAILURES"
