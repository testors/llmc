#!/usr/bin/env bash
set -e

INSTALL_DIR="${HOME}/.local/bin"
DATA_DIR="${HOME}/.local/share/llmc"
REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
SHELL_NAME="$(basename "$SHELL")"

# ── Build ───────────────────────────────────────────────────────────────────────
echo "Building llmc..."
cargo build --release --manifest-path "${REPO_DIR}/Cargo.toml"

# ── Install binary ──────────────────────────────────────────────────────────────
mkdir -p "${INSTALL_DIR}"
cp "${REPO_DIR}/target/release/llmc" "${INSTALL_DIR}/llmc"
echo "Installed: ${INSTALL_DIR}/llmc"

# ── Install shell scripts ──────────────────────────────────────────────────────
mkdir -p "${DATA_DIR}"
cp "${REPO_DIR}/setup_bash.sh" "${DATA_DIR}/setup_bash.sh"
cp "${REPO_DIR}/setup_zsh.sh"  "${DATA_DIR}/setup_zsh.sh"
cp "${REPO_DIR}/uninstall.sh"  "${DATA_DIR}/uninstall.sh"
chmod +x "${DATA_DIR}/uninstall.sh"
echo "Installed: ${DATA_DIR}/"

# ── Ensure PATH ─────────────────────────────────────────────────────────────────
if ! echo "$PATH" | tr ':' '\n' | grep -qx "${INSTALL_DIR}"; then
  case "${SHELL_NAME}" in
    zsh)  RC_FILE="${HOME}/.zshrc" ;;
    bash) RC_FILE="${HOME}/.bashrc" ;;
    *)    RC_FILE="${HOME}/.profile" ;;
  esac

  if ! grep -q "${INSTALL_DIR}" "${RC_FILE}" 2>/dev/null; then
    echo "export PATH=\"${INSTALL_DIR}:\$PATH\"" >> "${RC_FILE}"
    echo "Added ${INSTALL_DIR} to PATH in ${RC_FILE}"
  fi
  export PATH="${INSTALL_DIR}:${PATH}"
fi

# ── Shell integration ───────────────────────────────────────────────────────────
case "${SHELL_NAME}" in
  zsh)
    RC_FILE="${HOME}/.zshrc"
    SETUP_FILE="${DATA_DIR}/setup_zsh.sh"
    ;;
  bash)
    RC_FILE="${HOME}/.bashrc"
    SETUP_FILE="${DATA_DIR}/setup_bash.sh"
    ;;
  *)
    echo "Done! Shell integration is available for bash and zsh only."
    exit 0
    ;;
esac

if ! grep -q "setup_${SHELL_NAME}.sh" "${RC_FILE}" 2>/dev/null; then
  echo "source \"${SETUP_FILE}\"" >> "${RC_FILE}"
  echo "Added Ctrl+E integration to ${RC_FILE}"
fi

echo ""
echo "Done! You can delete the source directory after installation."
echo "To uninstall: ~/.local/share/llmc/uninstall.sh"
echo ""
echo "Run this to activate now:"
echo "  source ${RC_FILE}"
