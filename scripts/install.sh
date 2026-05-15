#!/usr/bin/env sh
set -eu

REPO="Aero123421/whisperccpcli"
VERSION="${VERSION:-latest}"
INSTALL_DIR="${INSTALL_DIR:-"$HOME/.whispercli/bin"}"
ROOT_DIR="$(dirname "$INSTALL_DIR")"

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS:$ARCH" in
  Linux:x86_64) ASSET="whispercli-linux-x64.tar.gz" ;;
  Darwin:x86_64) ASSET="whispercli-macos-x64.tar.gz" ;;
  Darwin:arm64) ASSET="whispercli-macos-arm64.tar.gz" ;;
  *) echo "unsupported platform: $OS $ARCH" >&2; exit 1 ;;
esac

if [ "$VERSION" = "latest" ]; then
  URL="https://github.com/$REPO/releases/latest/download/$ASSET"
else
  URL="https://github.com/$REPO/releases/download/$VERSION/$ASSET"
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

mkdir -p "$INSTALL_DIR" "$ROOT_DIR/models" "$ROOT_DIR/transcripts" "$ROOT_DIR/logs"

echo "==> Downloading $URL"
if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$URL" -o "$TMP_DIR/$ASSET"
elif command -v wget >/dev/null 2>&1; then
  wget -q "$URL" -O "$TMP_DIR/$ASSET"
else
  echo "curl or wget is required for the shell installer" >&2
  exit 1
fi

echo "==> Installing to $INSTALL_DIR"
tar -xzf "$TMP_DIR/$ASSET" -C "$INSTALL_DIR"
chmod +x "$INSTALL_DIR/whispercli"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    echo "Add this to your shell profile:"
    echo "  export PATH=\"\$PATH:$INSTALL_DIR\""
    ;;
esac

"$INSTALL_DIR/whispercli" doctor
