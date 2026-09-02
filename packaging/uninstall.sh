#!/bin/sh
set -eu

if [ -n "${STARCIL_INSTALL_DIR:-}" ]; then
    INSTALL_DIR=$STARCIL_INSTALL_DIR
elif [ -f "$HOME/.local/bin/starcil" ] || [ -L "$HOME/.local/bin/starcil" ]; then
    INSTALL_DIR="$HOME/.local/bin"
elif [ -f "/usr/local/bin/starcil" ] || [ -L "/usr/local/bin/starcil" ]; then
    INSTALL_DIR="/usr/local/bin"
else
    INSTALL_DIR="$HOME/.local/bin"
fi

BINARY_PATH="$INSTALL_DIR/starcil"
if [ -f "$BINARY_PATH" ] || [ -L "$BINARY_PATH" ]; then
    if [ -w "$BINARY_PATH" ] || [ -w "$INSTALL_DIR" ]; then
        rm -f "$BINARY_PATH"
    elif command -v sudo >/dev/null 2>&1; then
        sudo rm -f "$BINARY_PATH"
    else
        echo "cannot remove $BINARY_PATH and sudo is unavailable" >&2
        exit 1
    fi
    echo "Removed Starcil binary or symlink: $BINARY_PATH"
elif [ -e "$BINARY_PATH" ]; then
    echo "refusing to remove non-file install target: $BINARY_PATH" >&2
    exit 1
else
    echo "Starcil binary was not present: $BINARY_PATH"
fi

CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/starcil"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/starcil"
echo "Kept user configuration: $CONFIG_DIR"
echo "Kept user data: $DATA_DIR"
