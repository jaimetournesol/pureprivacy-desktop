#!/usr/bin/env bash
# Boot a PurePrivacy box inside the container: a virtual display + session bus for the
# webkit-linked binary, then the app's existing headless AUTOSTART path (provision on
# first run from env, just start on subsequent runs). Prints the phone-connect info once
# the box has minted its onion.
set -euo pipefail

: "${PP_PASS:?Set PP_PASS (the box admin password the phone logs in with)}"
PP_USER="${PP_USER:-jaime}"
PP_BOX="${PP_BOX:-mybox}"
# Stable secrets key so secrets.json (AES-GCM) decrypts across restarts. Provide your own
# in production; the fallback keeps a single container reproducible but is NOT secret.
PP_SECRETS_KEY="${PP_SECRETS_KEY:-pureprivacy-docker-default-key-change-me=}"

# Virtual X display (the GUI renders here, unseen) + a session D-Bus webkit needs.
# `docker restart` reuses the container FS, so clear any stale X lock first — otherwise
# the second boot can't claim :99 and GTK init fails.
rm -f /tmp/.X99-lock /tmp/.X11-unix/X99 2>/dev/null || true
Xvfb :99 -screen 0 1024x768x16 -nolisten tcp >/tmp/xvfb.log 2>&1 &
for i in $(seq 1 50); do [ -e /tmp/.X11-unix/X99 ] && break; sleep 0.2; done
[ -e /tmp/.X11-unix/X99 ] || { echo "[entrypoint] Xvfb failed to start:"; cat /tmp/xvfb.log; exit 1; }
eval "$(dbus-launch --sh-syntax)"
export DBUS_SESSION_BUS_ADDRESS DBUS_SESSION_BUS_PID

# Print the phone-connect QR once the box has an onion (watch box.json in the volume).
(
  onion=""
  for i in $(seq 1 300); do
    if [ -f /data/box.json ]; then
      onion="$(sed -n 's/.*"onion"[^"]*"\([a-z2-7]*\.onion\)".*/\1/p' /data/box.json 2>/dev/null | head -1)"
      [ -n "$onion" ] && break
    fi
    sleep 1
  done
  if [ -n "$onion" ]; then
    echo "======================================================================"
    echo "  PurePrivacy box is up.  User: @${PP_USER}:${onion}"
    echo "  Scan this with the PurePrivacy phone app to connect:"
    echo "======================================================================"
    qrencode -t ANSIUTF8 "pureprivacy:@${PP_USER}:${onion}" 2>/dev/null || echo "  pureprivacy:@${PP_USER}:${onion}"
    echo "======================================================================"
  else
    echo "[entrypoint] box.json/onion not seen yet — check logs above for setup progress."
  fi
) &

# Hand off to the box. AUTOSTART=1: provision on first run (needs PROVISION_USER/PASS),
# just resume on later runs (onion already present). This is the box's own headless path.
exec env \
  PUREPRIVACY_AUTOSTART=1 \
  PUREPRIVACY_PROVISION_USER="$PP_USER" \
  PUREPRIVACY_PROVISION_PASS="$PP_PASS" \
  PUREPRIVACY_PROVISION_BOX="$PP_BOX" \
  PUREPRIVACY_SECRETS_KEY="$PP_SECRETS_KEY" \
  /opt/pureprivacy/pureprivacy
