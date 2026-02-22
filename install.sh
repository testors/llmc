#!/bin/sh
set -e

REPO="testors/llmc"
INSTALL_DIR="${HOME}/.local/bin"

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

# Get latest release download URL
DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"

echo "Downloading llmc for ${os}-${arch}..."
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

curl -fsSL "${DOWNLOAD_URL}" -o "${TMP}/${ASSET}"
tar xzf "${TMP}/${ASSET}" -C "${TMP}"

mkdir -p "${INSTALL_DIR}"
mv "${TMP}/llmc" "${INSTALL_DIR}/llmc"
chmod +x "${INSTALL_DIR}/llmc"

echo "Installed: ${INSTALL_DIR}/llmc"

# Ensure PATH
SHELL_NAME="$(basename "${SHELL:-/bin/sh}")"
case "${SHELL_NAME}" in
  zsh)  RC_FILE="${HOME}/.zshrc" ;;
  bash) RC_FILE="${HOME}/.bashrc" ;;
  *)    RC_FILE="${HOME}/.profile" ;;
esac

if ! echo "${PATH}" | tr ':' '\n' | grep -qx "${INSTALL_DIR}"; then
  if ! grep -q "${INSTALL_DIR}" "${RC_FILE}" 2>/dev/null; then
    echo "export PATH=\"${INSTALL_DIR}:\$PATH\"" >> "${RC_FILE}"
    echo "Added ${INSTALL_DIR} to PATH in ${RC_FILE}"
  fi
fi

# Shell integration (Ctrl+E)
"${INSTALL_DIR}/llmc" --install 2>/dev/null || true

echo ""
echo "Done! Run this to activate now:"
echo "  source ${RC_FILE}"
