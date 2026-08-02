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
HANDOFF_PW=/handoff/webui-password
# The OWNER's choice wins. If they set a password from the phone it arrives here, and it
# must beat both the environment and anything we generated on an earlier boot — otherwise
# "change my password" would appear to work and quietly not.
if [ -s "$HANDOFF_PW" ] && ! cmp -s "$HANDOFF_PW" "$PW_FILE"; then
  HERMES_WEBUI_PASSWORD="$(cat "$HANDOFF_PW")"
  ( umask 077; printf '%s' "$HERMES_WEBUI_PASSWORD" > "$PW_FILE" )
  echo "[agent] using the WebUI password set by the owner"
fi
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

# A generated password nobody can read is the same as a locked door with no key: the owner
# would reach the WebUI over its onion and have nothing to type. So mirror it into the
# handoff volume, which is exactly the box↔agent channel the Matrix credentials already use
# (0600, private named volume, never published). The box hands it to the OWNER'S phone only,
# and the phone fills it in for them.
if [ -d /handoff ]; then
  ( umask 077; printf '%s' "$HERMES_WEBUI_PASSWORD" > /handoff/webui-password ) || true
fi

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

# ── WebUI, supervised so a password change can take effect ──────────────────────────────
# The password is read by the server at startup, so changing it means restarting the server.
# The owner changes it from their phone, long after this container booted, and asking them
# to recreate a container to change a password would be absurd — so watch the handoff file
# and bounce the server when it changes.
#
# A restart must stay distinguishable from a CRASH: an unconditional restart loop would hide
# a genuinely broken server behind an endless respawn, and Docker's restart policy would
# never see the failure. The watcher therefore leaves a marker before it kills, and only a
# marked exit is treated as intentional.
RESTART_FLAG=/tmp/webui-restart
webui_loop() {
  while true; do
    HERMES_WEBUI_PASSWORD="$(cat "$PW_FILE")"
    export HERMES_WEBUI_PASSWORD
    # --foreground makes bootstrap EXEC the server in place. Without it, it double-forks a
    # detached child and returns, so we'd see our only job "finish" immediately and treat a
    # perfectly healthy server as dead.
    /opt/hermes/venv/bin/python bootstrap.py --foreground &
    local child=$!
    echo "$child" > /tmp/webui.pid
    wait "$child" || true
    if [ -f "$RESTART_FLAG" ]; then
      rm -f "$RESTART_FLAG"
      echo "[agent] webui restarting on the new password"
      continue
    fi
    echo "[agent] webui exited unexpectedly" >&2
    return 1   # a real failure: fall through so the container exits and Docker restarts it
  done
}
webui_loop &
pids+=($!)

password_watch() {
  local seen
  seen="$(md5sum "$PW_FILE" 2>/dev/null | cut -d' ' -f1)"
  while true; do
    sleep 5
    [ -s "$HANDOFF_PW" ] || continue
    cmp -s "$HANDOFF_PW" "$PW_FILE" && continue
    ( umask 077; cp "$HANDOFF_PW" "$PW_FILE" )
    local now
    now="$(md5sum "$PW_FILE" 2>/dev/null | cut -d' ' -f1)"
    [ "$now" = "$seen" ] && continue
    seen="$now"
    echo "[agent] WebUI password changed by the owner — restarting the webui"
    touch "$RESTART_FLAG"
    kill "$(cat /tmp/webui.pid 2>/dev/null)" 2>/dev/null || true
  done
}
password_watch &
pids+=($!)

# ── Agents beyond the first ─────────────────────────────────────────────────────────────
# One box can run several agents. Each is a Hermes PROFILE with its own home, config, model,
# memory and skills — and its own Matrix account, so it shows up as its own contact in the
# Agents app rather than as a second personality behind one identity.
#
# The first agent maps to Hermes's *default* profile and keeps taking its credentials from
# the process environment (below). Every later agent gets `profiles/<localpart>/`, whose
# `.env` carries that agent's Matrix credentials — that file IS the profile's secret scope,
# which is how the multiplexer gives each profile its own identity. Putting them in the
# process environment instead would hand every profile the first agent's account.
#
# Matrix needs no per-profile config.yaml: the gateway enables the platform whenever
# MATRIX_ACCESS_TOKEN + MATRIX_HOMESERVER resolve in scope (gateway/config.py).
HERMES_PROFILES_DIR="$HERMES_HOME/profiles"

# Read one KEY=VALUE out of a handoff file. Deliberately NOT `source`: these files come from
# another container, and sourcing would execute whatever is in them.
handoff_get() { sed -n "s/^$2=//p" "$1" 2>/dev/null | head -1; }

# Give agent $1 (localpart) a profile carrying the credentials in $2 (its handoff file).
provision_profile() {
  local lp="$1" env_file="$2"
  local home="$HERMES_PROFILES_DIR/$lp"

  if [ ! -d "$home" ]; then
    echo "[agent] new agent '$lp' — creating its Hermes profile"
    # --clone copies the default profile's config.yaml/.env, so a new agent starts on a
    # model that already works instead of an onboarding wizard the owner can't reach from
    # the Agents app. They can point it at a different provider afterwards in Agent settings.
    if ! /opt/hermes/venv/bin/hermes profile create "$lp" --clone --no-alias \
         --description "PurePrivacy agent $lp" >/dev/null 2>&1; then
      echo "[agent] could not create the Hermes profile for '$lp'" >&2
      return 1
    fi
  fi

  # The provider the owner picked in the wizard. Absent = "same as my other agents", which
  # the --clone above already gave them, so we leave config.yaml alone.
  #
  # For an OAuth provider (openai-codex, xai-oauth, qwen-oauth) there is no key to write:
  # Hermes holds its own OAuth session in auth.json and the owner finishes signing in from
  # Agent settings. We still record the provider so the profile opens on the right one.
  local prov; prov="$(handoff_get "$env_file" PP_AGENT_PROVIDER)"
  if [ -n "$prov" ]; then
    local mdl base key
    mdl="$(handoff_get "$env_file" PP_AGENT_MODEL)"
    base="$(handoff_get "$env_file" PP_AGENT_BASE_URL)"
    key="$(handoff_get "$env_file" PP_AGENT_API_KEY)"
    echo "[agent] '$lp' configured for provider '$prov'"
    ( umask 077
      {
        echo "model:"
        echo "  provider: $prov"
        if [ -n "$mdl" ];  then echo "  default: $mdl"; fi
        if [ -n "$base" ]; then echo "  base_url: $base"; fi
        # model.api_key, NOT OPENAI_API_KEY in .env: Hermes host-gates OPENAI_API_KEY to
        # openai.com, so a key put there is silently dropped for any other endpoint and the
        # runtime falls through to "no-key-required" → 401. See docs/HANDOFF.
        if [ -n "$key" ];  then echo "  api_key: $key"; fi
      } > "$home/config.yaml"
    )
    chmod 600 "$home/config.yaml"
  fi

  # Cross-signing, per agent — same two-step as the default profile, because each agent is a
  # separate Matrix identity and cannot borrow another's recovery key.
  local rec_file="$home/matrix-recovery.key" rec_line
  if [ -s "$rec_file" ]; then
    rec_line="MATRIX_RECOVERY_KEY=$(cat "$rec_file")"
  else
    rec_line="MATRIX_RECOVERY_KEY_OUTPUT_FILE=$rec_file"
  fi

  # Rewrite only the keys we own, so anything the owner set in Agent settings survives.
  local tmp="$home/.env.pp-new"
  ( umask 077
    if [ -f "$home/.env" ]; then
      grep -vE '^(MATRIX_HOMESERVER|MATRIX_USER_ID|MATRIX_ACCESS_TOKEN|MATRIX_DEVICE_ID|MATRIX_ALLOWED_USERS|MATRIX_E2EE_MODE|MATRIX_RECOVERY_KEY|MATRIX_RECOVERY_KEY_OUTPUT_FILE)=' \
        "$home/.env" > "$tmp" 2>/dev/null || true
    else
      : > "$tmp"
    fi
    {
      echo "MATRIX_HOMESERVER=$(handoff_get "$env_file" MATRIX_HOMESERVER)"
      echo "MATRIX_USER_ID=$(handoff_get "$env_file" MATRIX_USER_ID)"
      echo "MATRIX_ACCESS_TOKEN=$(handoff_get "$env_file" MATRIX_ACCESS_TOKEN)"
      echo "MATRIX_DEVICE_ID=$(handoff_get "$env_file" MATRIX_DEVICE_ID)"
      echo "MATRIX_ALLOWED_USERS=$(handoff_get "$env_file" PP_OWNER)"
      echo "MATRIX_E2EE_MODE=required"
      echo "$rec_line"
    } >> "$tmp"
  )
  mv "$tmp" "$home/.env"
  chmod 600 "$home/.env"
  return 0
}

# One gateway serves every profile (gateway.multiplex_profiles). Starting a second gateway
# per profile would double-bind the same platforms — Hermes hard-errors on exactly that.
enable_multiplexing() {
  local cfg="$HERMES_HOME/config.yaml"
  if grep -qE '^\s*multiplex_profiles:\s*true' "$cfg" 2>/dev/null; then return 0; fi
  echo "[agent] enabling gateway.multiplex_profiles so one gateway serves every agent"
  /opt/hermes/venv/bin/hermes config set gateway.multiplex_profiles true >/dev/null 2>&1 \
    || echo "[agent] could not enable multiplex_profiles — extra agents may stay offline" >&2
}

# ── Gateway, gated on the box handing us credentials ────────────────────────────────────
# The gateway is what makes an agent reachable as a Matrix user. It stays off until the box
# has provisioned an account and written its handoff file — starting it before that just
# crash-loops on a fresh install. Setup happens from the phone, possibly long after this
# container started, so watch for the files rather than checking once.
gateway_watch() {
  local seen=""
  while true; do
    if [ -f /handoff/matrix.env ]; then
      local now
      # Hash EVERY agent's file: adding a second agent must restart the gateway, and the
      # first agent's file is untouched by that.
      #
      # Build the list explicitly rather than globbing straight into `cat`. /handoff/agents
      # does not exist until a second agent is provisioned, so the glob would stay literal,
      # `cat` would fail on it, and — with `set -o pipefail` — the failure propagates out of
      # the pipeline and `set -e` kills this watcher. Which kills the container, on the far
      # more common path where there is only ever one agent.
      local files=(/handoff/matrix.env) f
      for f in /handoff/agents/*.env; do
        if [ -e "$f" ]; then files+=("$f"); fi
      done
      now="$(cat "${files[@]}" | md5sum | cut -d' ' -f1)"
      if [ "$now" != "$seen" ]; then
        seen="$now"
        # shellcheck disable=SC1091
        set -a; . /handoff/matrix.env; set +a
        # Only the owner may talk to the agent. Without this the adapter's default gating
        # applies, and on a federated box that is not a boundary we want to leave to chance.
        export MATRIX_ALLOWED_USERS="${PP_OWNER:-}"
        # E2EE: required, not optional. Everything else on the box is end-to-end encrypted,
        # and an agent conversation carries exactly the kind of content that shouldn't be
        # the one plaintext exception. "optional" would silently fall back to cleartext when
        # a room isn't encrypted, which is the failure mode you'd never notice.
        export MATRIX_E2EE_MODE="${MATRIX_E2EE_MODE:-required}"
        # ── Cross-signing identity ──────────────────────────────────────────────────────
        # Without this the agent's device is never signed by its own identity, so every
        # client shows it as unverified ("not verified by its owner" in Element) and has no
        # way to tell a genuine agent device from one an attacker registered on the account.
        #
        # Hermes will bootstrap cross-signing by itself, but ONLY if it has somewhere to put
        # the recovery key it generates: with MATRIX_RECOVERY_KEY_OUTPUT_FILE unset it
        # refuses (deliberately — it will not print a recovery key to a log). So the first
        # boot points it at a file, and every boot after that feeds the same key back in.
        #
        # BOTH halves are required, and the second is the one that's easy to miss. The
        # bootstrap branch only runs when the account has NO cross-signing keys at all;
        # once they exist, a NEW device (a fresh access token, a rebuilt container) takes
        # the other branch, and without MATRIX_RECOVERY_KEY there is nothing to sign it
        # with — the identity survives but this device sits outside it, silently.
        #
        # The key lives in the agent's own volume, 0600, written once by Hermes with
        # O_EXCL. Losing it is not fatal: delete the file AND the account's cross-signing
        # keys on the homeserver, and the next boot bootstraps a fresh identity.
        MATRIX_XSIGN_KEY_FILE="$HERMES_HOME/matrix-recovery.key"
        if [ -s "$MATRIX_XSIGN_KEY_FILE" ]; then
          MATRIX_RECOVERY_KEY="$(cat "$MATRIX_XSIGN_KEY_FILE")"
          export MATRIX_RECOVERY_KEY
          unset MATRIX_RECOVERY_KEY_OUTPUT_FILE
        else
          # Hermes refuses to overwrite this path, so only offer it when it's absent.
          export MATRIX_RECOVERY_KEY_OUTPUT_FILE="$MATRIX_XSIGN_KEY_FILE"
          unset MATRIX_RECOVERY_KEY
        fi
        # No proxy: the homeserver is on OUR loopback (shared netns), so a Tor circuit here
        # would be a pointless round trip out to the network and back to the same host.
        unset MATRIX_PROXY
        # Every agent after the first, before the gateway starts — the multiplexer reads the
        # profile set once at startup, so a profile written afterwards would not be served.
        local extra=0 lp
        for f in /handoff/agents/*.env; do
          [ -e "$f" ] || continue
          lp="$(basename "$f" .env)"
          # The first agent IS the default profile, so it takes the env path below.
          if [ "$lp" = "hermes-ai" ]; then continue; fi
          if provision_profile "$lp" "$f"; then
            extra=$((extra + 1))
          fi
        done
        if [ "$extra" -gt 0 ]; then enable_multiplexing; fi

        echo "[agent] credentials received for ${MATRIX_USER_ID:-?} (+${extra} more) — starting gateway"
        pkill -f "hermes gateway" 2>/dev/null || true
        # `run`, NOT `start`. `start` drives an installed systemd/launchd service, which
        # doesn't exist in a container — it printed "The gateway runs as the container's main
        # process. Or run the gateway directly: hermes gateway run" and exited, so the agent
        # sat there with a Matrix account and no gateway, silently ignoring every message.
        # `hermes gateway --help` calls `run` the one "recommended for WSL, Docker, Termux".
        /opt/hermes/venv/bin/hermes gateway run &
      fi
    fi
    sleep 5
  done
}
gateway_watch &
pids+=($!)

# Exit as soon as EITHER dies, so Docker's restart policy sees the failure instead of the
# container lingering half-alive with one process gone.
wait -n
term
wait
