#!/bin/sh
set -e

# lane installer. Mirrors the artifact naming produced by .github/workflows/release.yml:
#   lane_<version>_<os>_<arch>.tar.gz  (+ checksums.txt)
# os ∈ {linux, darwin}, arch ∈ {amd64, arm64}.

REPO="FlexNetOS/lane"
INSTALL_DIR="/usr/local/bin"

log() {
  printf "%s\n" "$1"
}

log "Step 1/7: Detecting platform..."

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
case "$OS" in
  darwin) OS="darwin" ;;
  linux)  OS="linux" ;;
  *)
    echo "Unsupported OS: $OS"
    exit 1
    ;;
esac

ARCH=$(uname -m)
case "$ARCH" in
  x86_64|amd64)  ARCH="amd64" ;;
  arm64|aarch64) ARCH="arm64" ;;
  *)
    echo "Unsupported architecture: $ARCH"
    exit 1
    ;;
esac

log "Step 2/7: Resolving latest release..."

TAG=$(curl -fsLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" | sed -n 's|.*/tag/||p')
if [ -z "$TAG" ]; then
  echo "Failed to fetch latest version"
  exit 1
fi

VERSION="${TAG#v}"
FILENAME="lane_${VERSION}_${OS}_${ARCH}.tar.gz"
URL="https://github.com/$REPO/releases/download/${TAG}/${FILENAME}"
CHECKSUM_URL="https://github.com/$REPO/releases/download/${TAG}/checksums.txt"

log "Installing lane ${VERSION} (${OS}/${ARCH})..."

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

log "Step 3/7: Downloading archive..."
curl -fL --retry 3 --connect-timeout 15 --progress-bar "$URL" -o "$TMP/$FILENAME"

log "Step 4/7: Downloading checksums..."
curl -fsSL --retry 3 --connect-timeout 15 "$CHECKSUM_URL" -o "$TMP/checksums.txt"

log "Step 5/7: Verifying checksum..."
if [ "$OS" = "darwin" ]; then
  (cd "$TMP" && grep "$FILENAME" checksums.txt | shasum -a 256 -c --quiet)
elif command -v sha256sum >/dev/null 2>&1; then
  (cd "$TMP" && grep "$FILENAME" checksums.txt | sha256sum -c --quiet)
else
  echo "Warning: cannot verify checksum (sha256sum/shasum not found)"
fi

log "Step 6/7: Extracting archive..."
tar -xzf "$TMP/$FILENAME" -C "$TMP"

if [ -w "$INSTALL_DIR" ]; then
  log "Step 7/7: Installing binary to $INSTALL_DIR..."
  install -m 0755 "$TMP/lane" "$INSTALL_DIR/lane"
else
  log "Step 7/7: Installing binary to $INSTALL_DIR (sudo password may be required)..."
  sudo -v
  sudo install -m 0755 "$TMP/lane" "$INSTALL_DIR/lane"
fi

log "Installed lane to $INSTALL_DIR/lane"
"$INSTALL_DIR/lane" version
