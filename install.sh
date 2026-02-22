#!/bin/sh
set -e

REPO="testors/llmc"
INSTALL_DIR="${HOME}/.local/bin"
DATA_DIR="${HOME}/.local/share/llmc"

# Detect platform
OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}" in
  Linux)  os="linux" ;;
  Darwin) os="macos" ;;
  *) echo "Unsupported OS: ${OS}"; exit 1 ;;
esac

case "${ARCH}" in
  x86_64|amd64)  arch="x86_64" ;;
  aarch64|arm64) arch="aarch64" ;;
  *) echo "Unsupported architecture: ${ARCH}"; exit 1 ;;
esac

ASSET="llmc-${os}-${arch}.tar.gz"
DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"

echo "Downloading llmc for ${os}-${arch}..."
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

curl -fsSL "${DOWNLOAD_URL}" -o "${TMP}/${ASSET}"
tar xzf "${TMP}/${ASSET}" -C "${TMP}"

# Install binary
mkdir -p "${INSTALL_DIR}"
mv "${TMP}/llmc" "${INSTALL_DIR}/llmc"
chmod +x "${INSTALL_DIR}/llmc"
echo "Installed: ${INSTALL_DIR}/llmc"

# Detect shell
SHELL_NAME="$(basename "${SHELL:-/bin/sh}")"
case "${SHELL_NAME}" in
  zsh)  RC_FILE="${HOME}/.zshrc" ;;
  bash) RC_FILE="${HOME}/.bashrc" ;;
  *)    RC_FILE="${HOME}/.profile" ;;
esac

# Ensure PATH
if ! echo "${PATH}" | tr ':' '\n' | grep -qx "${INSTALL_DIR}"; then
  if ! grep -q "${INSTALL_DIR}" "${RC_FILE}" 2>/dev/null; then
    echo "export PATH=\"${INSTALL_DIR}:\$PATH\"" >> "${RC_FILE}"
    echo "Added ${INSTALL_DIR} to PATH in ${RC_FILE}"
  fi
fi

# Write shell integration scripts
mkdir -p "${DATA_DIR}"

cat > "${DATA_DIR}/setup_bash.sh" << 'BASH_EOF'
# llmc: Bash integration — source this file in your .bashrc
# Usage: Press Ctrl+E with a natural language description on the command line

_ai_cmd_replace() {
  [[ -z "$READLINE_LINE" ]] && return

  local result
  result="$(llmc "$READLINE_LINE" 2>/dev/tty)"

  if [[ $? -eq 0 && -n "$result" ]]; then
    READLINE_LINE="$result"
    READLINE_POINT=${#READLINE_LINE}
  fi
}

bind -x '"\C-e": _ai_cmd_replace'
BASH_EOF

cat > "${DATA_DIR}/setup_zsh.sh" << 'ZSH_EOF'
# llmc: Zsh integration — source this file in your .zshrc
# Usage: Press Ctrl+E with a natural language description on the command line

_ai_cmd_replace() {
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
ZSH_EOF

echo "Installed: ${DATA_DIR}/"

# Add shell integration source line
case "${SHELL_NAME}" in
  zsh)  SETUP_FILE="${DATA_DIR}/setup_zsh.sh" ;;
  bash) SETUP_FILE="${DATA_DIR}/setup_bash.sh" ;;
  *)
    echo ""
    echo "Done! Shell integration is available for bash and zsh only."
    exit 0
    ;;
esac

if ! grep -q "${SETUP_FILE}" "${RC_FILE}" 2>/dev/null; then
  echo "source \"${SETUP_FILE}\"" >> "${RC_FILE}"
  echo "Added Ctrl+E integration to ${RC_FILE}"
fi

echo ""
echo "Done! Run this to activate now:"
echo "  source ${RC_FILE}"
