#!/usr/bin/env bash
# Stage the (host-built) box binary + sidecars next to the Dockerfile, then build the image.
# Stage-1 approach: reuse prebuilt artifacts rather than compiling inside Docker (fast to
# iterate). A shipping image would build both in a multi-stage Dockerfile instead.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN_SRC="${PUREPRIVACY_BIN_DIR:-$HOME/.local/share/ai.tournesol.pureprivacy/bin}"
APP_BIN="${APP_BIN:-$HOME/Tournesol/pureprivacy-desktop/src-tauri/target/release/pureprivacy}"
IMG="${IMAGE:-pureprivacy-box:dev}"

[ -x "$APP_BIN" ] || { echo "no box binary at $APP_BIN — build it first (pnpm tauri build)"; exit 1; }

rm -rf "$HERE/bin" "$HERE/pureprivacy"
mkdir -p "$HERE/bin"
# turnserver is NOT staged — the image installs coturn via apt (correct libs) and symlinks
# it into the bin dir, so calls work without shipping the host's DB-linked turnserver.
for b in tor tuwunel caddy livekit-server lk-jwt-service; do
  if [ -f "$BIN_SRC/$b" ]; then cp "$BIN_SRC/$b" "$HERE/bin/$b"
  else echo "warn: sidecar '$b' missing at $BIN_SRC (box will run without it)"; fi
done
cp "$APP_BIN" "$HERE/pureprivacy"

echo "staged $(du -sh "$HERE/bin" | cut -f1) sidecars + $(du -h "$HERE/pureprivacy" | cut -f1) binary"
docker build -t "$IMG" "$HERE"
echo "✓ built $IMG"
echo
echo "Run a box (reached only via its .onion — no ports to publish):"
echo "  docker volume create pp-data"
echo "  docker run -d --name mybox -v pp-data:/data \\"
echo "    -e PP_USER=jaime -e PP_PASS=your-strong-pass -e PP_BOX=mybox \\"
echo "    -e PP_SECRETS_KEY=\$(openssl rand -base64 32) $IMG"
echo "  docker logs -f mybox   # watch it mint its onion + print the connect QR"
