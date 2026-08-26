#!/bin/sh
# Install the latest gitpane release binary.
#   curl -fsSL https://raw.githubusercontent.com/affromero/gitpane/main/install.sh | sh
# Override the destination with GITPANE_INSTALL_DIR (default: /usr/local/bin).
set -eu

REPO="affromero/gitpane"
INSTALL_DIR="${GITPANE_INSTALL_DIR:-/usr/local/bin}"

OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
  Darwin)
    case "$ARCH" in
      arm64)  TARGET="aarch64-apple-darwin" ;;
      x86_64) TARGET="x86_64-apple-darwin" ;;
      *) echo "error: unsupported macOS architecture: $ARCH" >&2; exit 1 ;;
    esac ;;
  Linux)
    case "$ARCH" in
      x86_64)          TARGET="x86_64-unknown-linux-musl" ;;
      aarch64 | arm64) TARGET="aarch64-unknown-linux-gnu" ;;
      *) echo "error: unsupported Linux architecture: $ARCH" >&2; exit 1 ;;
    esac ;;
  *)
    echo "error: unsupported OS: $OS (on Windows, download the zip from https://github.com/$REPO/releases/latest)" >&2
    exit 1 ;;
esac

URL="https://github.com/$REPO/releases/latest/download/gitpane-$TARGET.tar.gz"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Downloading gitpane ($TARGET)..."
curl -fsSL "$URL" | tar xz -C "$TMP_DIR"

if [ -w "$INSTALL_DIR" ]; then
  mv "$TMP_DIR/gitpane" "$INSTALL_DIR/gitpane"
else
  echo "Installing to $INSTALL_DIR (requires sudo)..."
  sudo mv "$TMP_DIR/gitpane" "$INSTALL_DIR/gitpane"
fi

echo "Installed: $("$INSTALL_DIR/gitpane" --version)"
