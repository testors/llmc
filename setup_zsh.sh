#!/usr/bin/env zsh
# llmc: Zsh integration — source this file in your .zshrc
# Usage: Press Ctrl+E with a natural language description on the command line

_ai_cmd_replace() {
  # Skip if the buffer is empty
  [[ -z "$BUFFER" ]] && return

  local result
  result="$(llmc "$BUFFER" 2>/dev/tty)"

  if [[ $? -eq 0 && -n "$result" ]]; then
    BUFFER="$result"
    CURSOR=${#BUFFER}
  fi
  zle redisplay
}

zle -N _ai_cmd_replace
bindkey '^e' _ai_cmd_replace
