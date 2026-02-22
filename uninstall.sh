#!/usr/bin/env bash
set -e

BIN="${HOME}/.local/bin/llmc"
DATA_DIR="${HOME}/.local/share/llmc"
CONFIG_DIR="${HOME}/.config/llmc"

echo "Uninstalling llmc..."

# ── Remove binary ──────────────────────────────────────────────────────────────
if [[ -f "$BIN" ]]; then
  rm "$BIN"
  echo "Removed: ${BIN}"
fi

# ── Remove shell scripts ───────────────────────────────────────────────────────
if [[ -d "$DATA_DIR" ]]; then
  rm -rf "$DATA_DIR"
  echo "Removed: ${DATA_DIR}"
fi

# ── Remove config ──────────────────────────────────────────────────────────────
if [[ -d "$CONFIG_DIR" ]]; then
  read -rp "Remove config (API key)? [y/N] " answer
  if [[ "$answer" =~ ^[Yy]$ ]]; then
    rm -rf "$CONFIG_DIR"
    echo "Removed: ${CONFIG_DIR}"
  else
    echo "Kept: ${CONFIG_DIR}"
  fi
fi

# ── Clean up shell rc files ────────────────────────────────────────────────────
for RC_FILE in "${HOME}/.zshrc" "${HOME}/.bashrc" "${HOME}/.profile"; do
  if [[ -f "$RC_FILE" ]] && grep -q "llmc" "$RC_FILE"; then
    sed -i'' -e '/setup_zsh\.sh/d' -e '/setup_bash\.sh/d' "$RC_FILE"
    echo "Cleaned: ${RC_FILE}"
  fi
done

echo ""
echo "Done! Restart your shell to apply changes."
