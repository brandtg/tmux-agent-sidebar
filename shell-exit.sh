# tmux-agent-sidebar: shell integration for interactive bash and zsh.
#
# When sourced inside tmux, each foreground command is recorded into two
# pane options so the sidebar can tell you when a long-running command
# (a batch job, a test suite) has finished:
#
#   @pane_last_cmd   basename of the last command started (preexec)
#   @pane_last_exit  exit code of the last completed command (precmd)
#
# The sidebar pairs these with the pane's foreground-command transitions
# to flash a finished task (green hollow diamond) until you focus the
# pane, and marks non-zero exits as failed (red diamond).
#
# Setup — add to ~/.zshrc (or ~/.bashrc):
#
#   [ -f "$HOME/.tmux/plugins/tmux-agent-sidebar/shell-exit.sh" ] && \
#     . "$HOME/.tmux/plugins/tmux-agent-sidebar/shell-exit.sh"
#
# Outside tmux, or in fish and other shells, the script does nothing.

_interp() {
  # Writes one pane option. `$1` = option name, `$2` = value.
  tmux set-option -p -t "${TMUX_PANE:-}" -- "$1" "$2" 2>/dev/null
}

_unlink_marks() {
  # Clear leftovers from a previous shell that occupied this pane, so no
  # stale exit code can be attributed to a future command.
  tmux set-option -p -t "${TMUX_PANE:-}" -u @pane_last_cmd 2>/dev/null
  tmux set-option -p -t "${TMUX_PANE:-}" -u @pane_last_exit 2>/dev/null
}

cmd_basename() {
  local cmd="${1%% *}"
  printf '%s\n' "${cmd##*/}"
}

# bash ---------------------------------------------------------------
if [ -n "${BASH_VERSION:-}" ]; then

  _tas_precmd() {
    _interp "@pane_last_exit" "$?"
  }

  # Record the command basename. The DEBUG trap fires for every command
  # the shell runs — including the tmux write inside it — so the guard
  # flag keeps the recursion at one write per user command.
  _tas_active=""
  _tas_debug() {
    [ "$_tas_active" = "1" ] && return 0
    case "$BASH_COMMAND" in
      _tas_*|trap*|"") return 0 ;;
    esac
    _tas_active="1"
    _interp "@pane_last_cmd" "$(cmd_basename "$BASH_COMMAND")"
    _tas_active=""
  }
  trap '_tas_debug' DEBUG

  case "$PROMPT_COMMAND" in
    *_tas_precmd*) ;;
    *) PROMPT_COMMAND="_tas_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
  esac

  _unlink_marks
  return 0
fi

# zsh ----------------------------------------------------------------
if [ -n "${ZSH_VERSION:-}" ]; then

  _tas_precmd_zsh() { _interp "@pane_last_exit" "$?"; }
  _tas_preexec_zsh() { _interp "@pane_last_cmd" "$(cmd_basename "$1")"; }

  if autoload -Uz add-zsh-hook 2>/dev/null; then
    add-zsh-hook precmd _tas_precmd_zsh
    add-zsh-hook preexec _tas_preexec_zsh
  else
    precmd_functions=(${precmd_functions[@]} _tas_precmd_zsh)
    preexec_functions=(${preexec_functions[@]} _tas_preexec_zsh)
  fi

  _unlink_marks
  return 0
fi
