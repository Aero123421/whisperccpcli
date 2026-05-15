#!/usr/bin/env sh
set -eu

REPO="Aero123421/whisperccpcli"
VERSION="${WHISPERCLI_VERSION:-${VERSION:-latest}}"
INSTALL_DIR="${WHISPERCLI_INSTALL_DIR:-$HOME/.whispercli/bin}"
ROOT_DIR="$(dirname "$INSTALL_DIR")"
TMP_DIR="$(mktemp -d)"
SKIP_DOWNLOAD=0
ASSET=""

if [ -n "${WHISPERCLI_SKIP_DOWNLOAD:-}" ]; then
  case "${WHISPERCLI_SKIP_DOWNLOAD}" in
    1|[Tt][Rr][Uu][Ee]|[Yy]|[Yy][Ee][Ss])
      SKIP_DOWNLOAD=1
      ;;
  esac
fi

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS:$ARCH" in
  Linux:x86_64) ASSET="whispercli-linux-x64.tar.gz" ;;
  Darwin:x86_64) ASSET="whispercli-macos-x64.tar.gz" ;;
  Darwin:arm64) ASSET="whispercli-macos-arm64.tar.gz" ;;
  *) echo "unsupported platform: $OS $ARCH" >&2; exit 1 ;;
esac

URL_BASE="https://github.com/$REPO/releases"
if [ "$VERSION" = "latest" ]; then
  DOWNLOAD_URL="$URL_BASE/latest/download/$ASSET"
  CHECKSUMS_URL="$URL_BASE/latest/download/checksums.txt"
else
  DOWNLOAD_URL="$URL_BASE/download/$VERSION/$ASSET"
  CHECKSUMS_URL="$URL_BASE/download/$VERSION/checksums.txt"
fi

cleanup() {
  rm -rf "$TMP_DIR"
}

trap cleanup EXIT

mkdir -p "$INSTALL_DIR" "$ROOT_DIR/models" "$ROOT_DIR/transcripts" "$ROOT_DIR/logs"

download_file() {
  local url="$1"
  local out_file="$2"

  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$out_file"
    return
  fi

  if command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O "$out_file"
    return
  fi

  echo "curl or wget is required for the shell installer" >&2
  return 1
}

sha256_of_file() {
  local file="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
    return
  fi

  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
    return
  fi

  echo ""
}

extract_checksums_entry() {
  local checksums_file="$1"
  local target_file="$2"
  awk -v target="$target_file" '$2 == target || $2 == "*" target { print $1; exit }' "$checksums_file"
}

verify_checksum() {
  local archive="$1"
  local file_name="$2"
  local checksums_file="$TMP_DIR/checksums.txt"

  if ! download_file "$CHECKSUMS_URL" "$checksums_file"; then
    echo "Warning: failed to download checksums.txt; skipping SHA256 verification."
    return 0
  fi

  local expected
  expected="$(extract_checksums_entry "$checksums_file" "$file_name")"
  if [ -z "$expected" ]; then
    echo "Warning: checksums.txt does not include $file_name; skipping SHA256 verification."
    return 0
  fi

  local actual
  actual="$(sha256_of_file "$archive")"
  if [ -z "$actual" ]; then
    echo "Warning: no sha256 utility found; skipping SHA256 verification."
    return 0
  fi

  if [ "$actual" != "$expected" ]; then
    echo "SHA256 mismatch for $file_name" >&2
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    return 1
  fi

  echo "SHA256 OK: $file_name"
}

if [ "$SKIP_DOWNLOAD" -eq 1 ]; then
  echo "WHISPERCLI_SKIP_DOWNLOAD is enabled, skipping download."
  if [ ! -x "$INSTALL_DIR/whispercli" ]; then
    echo "whispercli executable is not available at $INSTALL_DIR/whispercli" >&2
    exit 1
  fi

  "$INSTALL_DIR/whispercli" doctor
  exit 0
fi

echo "==> Downloading $DOWNLOAD_URL"
download_file "$DOWNLOAD_URL" "$TMP_DIR/$ASSET"
verify_checksum "$TMP_DIR/$ASSET" "$ASSET"

echo "==> Installing to $INSTALL_DIR"
tar -xzf "$TMP_DIR/$ASSET" -C "$INSTALL_DIR"
chmod +x "$INSTALL_DIR/whispercli"

echo "==> Verifying install"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    echo "Add this to your shell profile:"
    echo "  export PATH=\"\$PATH:$INSTALL_DIR\""
    ;;
esac

"$INSTALL_DIR/whispercli" doctor
