#!/usr/bin/env bash
# PurePrivacy — one script to install a box, with or without agents.
#
#     ./install.sh
#
# Asks two questions, then runs the docker commands for you. Everything it does can also
# be done by hand with ./pp-box (init / build / up / agents) — this is just the front door.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"

command -v docker >/dev/null 2>&1 || { echo "Docker isn't installed. Install Docker first."; exit 1; }
docker info >/dev/null 2>&1 || { echo "Docker isn't running (or you lack permission). Start it and retry."; exit 1; }

echo
echo "  PurePrivacy — your box"
echo "  ─────────────────────"
echo
echo "  1) Basic          messaging + calls, over Tor.            (~1.2 GB)"
echo "  2) With agents    adds AI agents running on your box.     (~3 GB total)"
echo
echo "  Agents live in their own container and appear in a separate 'Agents' app on"
echo "  your phone — never mixed in with messages from people."
echo
read -rp "  Which would you like? [1] " choice
case "${choice:-1}" in
  2) WANT_AGENTS=1 ;;
  *) WANT_AGENTS=0 ;;
esac

# ── The box image ───────────────────────────────────────────────────────────────────────
# Prefer the published image (no build toolchain needed); fall back to building locally.
BOX_IMAGE="${PP_IMAGE:-jaimemelon/pureprivacy-box:latest}"
echo
echo "→ getting the box image ($BOX_IMAGE)"
if ! docker pull "$BOX_IMAGE"; then
  echo "  couldn't pull it — building from source instead"
  ./build.sh
  BOX_IMAGE="pureprivacy-box:dev"
fi

# ── The agent image (only if asked for) ─────────────────────────────────────────────────
if [ "$WANT_AGENTS" = "1" ]; then
  AGENT_IMAGE="${PP_AGENT_IMAGE:-pureprivacy-agent:dev}"
  if docker image inspect "$AGENT_IMAGE" >/dev/null 2>&1; then
    echo "→ agent image already present ($AGENT_IMAGE)"
  else
    echo "→ building the agent image — this takes a few minutes the first time"
    docker build -t "$AGENT_IMAGE" "$HERE/agent"
  fi
fi

# ── Config ──────────────────────────────────────────────────────────────────────────────
# init writes .env (box name, secrets key, and THIS box's data volume name). Never rerun it
# on a box that already exists: a fresh volume name means a new, empty box.
if [ -f .env ]; then
  echo
  echo "→ .env already exists — keeping your existing box config untouched."
else
  ./pp-box init
fi

# Record the choice so every later pp-box command agrees with it.
if grep -q '^PP_AGENTS=' .env 2>/dev/null; then
  sed -i "s/^PP_AGENTS=.*/PP_AGENTS=$WANT_AGENTS/" .env
else
  printf 'PP_AGENTS=%s\n' "$WANT_AGENTS" >> .env
fi
if ! grep -q '^PP_IMAGE=' .env 2>/dev/null; then
  printf 'PP_IMAGE=%s\n' "$BOX_IMAGE" >> .env
fi

./pp-box up

echo
echo "  Done."
echo "  Open http://127.0.0.1:8470/ to finish setup, then scan the QR in the phone app."
if [ "$WANT_AGENTS" = "1" ]; then
  echo "  Then open the Agents app on your phone and tap 'Set up agents'."
else
  echo "  Want agents later?  ./pp-box agents on"
fi
echo
