#!/bin/sh
set -eu

REPO_SLUG="${STARCIL_UPDATE_REPO:-daybigo/starcil}"
CHECKSUM_ASSET="SHA256SUMS"

if ! command -v curl >/dev/null 2>&1; then
    echo "starcil installer requires curl" >&2
    exit 1
fi

OS=$(uname -s)
ARCH=$(uname -m)
case "$OS:$ARCH" in
    Linux:x86_64|Linux:amd64) ASSET="starcil-x86_64-unknown-linux-gnu" ;;
    Linux:aarch64|Linux:arm64) ASSET="starcil-aarch64-unknown-linux-gnu" ;;
    Darwin:x86_64|Darwin:amd64) ASSET="starcil-x86_64-apple-darwin" ;;
    Darwin:arm64|Darwin:aarch64) ASSET="starcil-aarch64-apple-darwin" ;;
    *) echo "unsupported platform: $OS $ARCH" >&2; exit 1 ;;
esac

LATEST_URL=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO_SLUG/releases/latest")
TAG=${LATEST_URL##*/}
if [ -z "$TAG" ] || [ "$TAG" = "latest" ]; then
    echo "could not resolve the latest stable Starcil release" >&2
    exit 1
fi

TEMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/starcil-install.XXXXXX")
trap 'rm -rf "$TEMP_DIR"' EXIT INT TERM
ASSET_PATH="$TEMP_DIR/$ASSET"
CHECKSUM_PATH="$TEMP_DIR/$CHECKSUM_ASSET"
BASE_URL="https://github.com/$REPO_SLUG/releases/download/$TAG"

curl -fsSL "$BASE_URL/$ASSET" -o "$ASSET_PATH"
curl -fsSL "$BASE_URL/$CHECKSUM_ASSET" -o "$CHECKSUM_PATH"

EXPECTED=$(awk -v asset="$ASSET" '$2 == asset || $2 == "*" asset { print tolower($1); exit }' "$CHECKSUM_PATH")
if [ -z "$EXPECTED" ]; then
    echo "$CHECKSUM_ASSET has no entry for $ASSET" >&2
    exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL=$(sha256sum "$ASSET_PATH" | awk '{print tolower($1)}')
elif command -v shasum >/dev/null 2>&1; then
    ACTUAL=$(shasum -a 256 "$ASSET_PATH" | awk '{print tolower($1)}')
else
    echo "starcil installer requires sha256sum or shasum" >&2
    exit 1
fi
if [ "$ACTUAL" != "$EXPECTED" ]; then
    echo "SHA-256 mismatch for $ASSET" >&2
    exit 1
fi

INSTALL_DIR="${STARCIL_INSTALL_DIR:-$HOME/.local/bin}"
if ! mkdir -p "$INSTALL_DIR" 2>/dev/null; then
    INSTALL_DIR="/usr/local/bin"
    if command -v sudo >/dev/null 2>&1; then
        sudo mkdir -p "$INSTALL_DIR"
        sudo install -m 0755 "$ASSET_PATH" "$INSTALL_DIR/starcil"
    else
        echo "cannot create $INSTALL_DIR and sudo is unavailable" >&2
        exit 1
    fi
else
    install -m 0755 "$ASSET_PATH" "$INSTALL_DIR/starcil"
fi

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "PATH hint: add $INSTALL_DIR to your PATH" ;;
esac

VERSION=${TAG#v}
printf 'starcil %s installed — run `starcil`\n' "$VERSION"
