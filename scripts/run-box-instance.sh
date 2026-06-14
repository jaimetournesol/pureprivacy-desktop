#!/usr/bin/env bash
# Launch one PurePrivacy desktop box GUI as an isolated instance.
# Usage: start-box.sh <name> <port-offset>
set -u
NAME="$1"; OFFSET="$2"
BIN=/home/jaime/Tournesol/pureprivacy-desktop/src-tauri/target/release/pureprivacy
SHARED_BIN=/home/jaime/.local/share/ai.tournesol.pureprivacy/bin
ROOT=/tmp/ppbox/$NAME
mkdir -p "$ROOT"
export XDG_DATA_HOME="$ROOT/share"
export XDG_CONFIG_HOME="$ROOT/config"
export XDG_CACHE_HOME="$ROOT/cache"
mkdir -p "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME"
export PUREPRIVACY_BIN_DIR="$SHARED_BIN"
export PUREPRIVACY_PORT_OFFSET="$OFFSET"
# GUI session
export DISPLAY=:0
export GDK_BACKEND=x11
export XAUTHORITY=/run/user/1000/.mutter-Xwaylandauth.H98SQ3
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
export XDG_RUNTIME_DIR=/run/user/1000
export WEBKIT_DISABLE_DMABUF_RENDERER=1   # avoid GPU/dmabuf issues over Xwayland
echo "[start-box] $NAME offset=$OFFSET data=$XDG_DATA_HOME"
exec "$BIN" >"$ROOT/box.log" 2>&1
