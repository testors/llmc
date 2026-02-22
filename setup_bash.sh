#!/usr/bin/env bash
# llmc: Bash integration — source this file in your .bashrc
# Usage: Press Ctrl+E with a natural language description on the command line

_ai_cmd_replace() {
  # Skip if the line is empty
  [[ -z "$READLINE_LINE" ]] && return

  local result
  result="$(llmc "$READLINE_LINE" 2>/dev/tty)"

  if [[ $? -eq 0 && -n "$result" ]]; then
    READLINE_LINE="$result"
    READLINE_POINT=${#READLINE_LINE}
  fi
}

bind -x '"\C-e": _ai_cmd_replace'
