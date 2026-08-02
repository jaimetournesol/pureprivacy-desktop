#!/usr/bin/env bash
# PurePrivacy agent container entrypoint: Hermes WebUI (+ optionally the messaging gateway).
#
# Runs in the BOX's network namespace, so everything here binds loopback and nothing is
# published. The WebUI is reached from the phone over a SECOND hidden service with tor v3
# client authorisation in front of it — see docker/README.md.
set -euo pipefail

mkdir -p "$HERMES_HOME" "$HERMES_WORKSPACE" "$HERMES_WEBUI_STATE_DIR"

# ── WebUI password ──────────────────────────────────────────────────────────────────────
# The WebUI can run shell commands, so it must NEVER come up without auth. Upstream only
# *warns* about this (its own .env.example: "REQUIRED if you bind to anything other than
# 127.0.0.1 — without it, anyone who can reach the port can run commands"), and we verified
# that an unset password really does serve /api/profiles to an unauthenticated caller. We
# bind loopback, but loopback here is the BOX's loopback, which tor maps onto an onion —
# so treat it as reachable and always have a password.
PW_FILE="$HERMES_HOME/webui-password"
if [ -z "${HERMES_WEBUI_PASSWORD:-}" ]; then
  if [ -s "$PW_FILE" ]; then
    HERMES_WEBUI_PASSWORD="$(cat "$PW_FILE")"
  else
    HERMES_WEBUI_PASSWORD="$(python3 -c 'import secrets;print(secrets.token_urlsafe(32))')"
    ( umask 077; printf '%s' "$HERMES_WEBUI_PASSWORD" > "$PW_FILE" )
    echo "[agent] generated a WebUI password → $PW_FILE (surface it to the owner, don't log it)"
  fi
fi
export HERMES_WEBUI_PASSWORD

# ── Reachability of the box ─────────────────────────────────────────────────────────────
# Not fatal: the agent is still useful (and configurable) before the homeserver answers,
# and the box may still be minting its onion on a first run. Say so rather than crash-loop.
if ! curl -sf --max-time 5 -o /dev/null "http://127.0.0.1:${PP_HOMESERVER_PORT:-8118}/_matrix/client/versions"; then
  echo "[agent] note: tuwunel not answering on 127.0.0.1:${PP_HOMESERVER_PORT:-8118} yet." >&2
  echo "[agent]       Expected if the box is still starting, or if this container was not" >&2
  echo "[agent]       started with --network container:pureprivacy-box." >&2
fi

pids=()
term() { for p in "${pids[@]:-}"; do kill "$p" 2>/dev/null || true; done; }
trap term TERM INT

echo "[agent] hermes $(/opt/hermes/venv/bin/hermes --version 2>/dev/null | head -1)"
echo "[agent] webui → http://${HERMES_WEBUI_HOST}:${HERMES_WEBUI_PORT}/  (box loopback)"

cd /opt/hermes/webui
# --foreground makes bootstrap EXEC the server in place. Without it, it double-forks a
# detached child and returns, so PID 1 would see its only job "finish" immediately and the
# container would restart-loop while the server was in fact running fine.
/opt/hermes/venv/bin/python bootstrap.py --foreground &
pids+=($!)

# The messaging gateway is what makes agents reachable as Matrix users. It stays OFF until
# at least one profile has Matrix credentials, otherwise it just fails in a loop on a fresh
# box. The box turns it on once it has provisioned an agent account.
if [ "${PP_AGENT_GATEWAY:-0}" = "1" ]; then
  echo "[agent] starting messaging gateway"
  /opt/hermes/venv/bin/hermes gateway start &
  pids+=($!)
fi

# Exit as soon as EITHER dies, so Docker's restart policy sees the failure instead of the
# container lingering half-alive with one process gone.
wait -n
term
wait
