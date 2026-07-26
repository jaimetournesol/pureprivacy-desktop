#!/usr/bin/env bash
# stage-sidecars.sh — put the box's sidecar binaries where `tauri build` will BUNDLE them,
# so an installer (.deb/.rpm/.AppImage) is a working box with no manual fetch step.
#
# They land in src-tauri/sidecars/, which tauri.conf.json ships as a resource; at runtime
# supervisor::bin_dir() prefers <resources>/sidecars when it contains tuwunel.
#
#   ./scripts/stage-sidecars.sh
#
# PLATFORM REALITY (checked 2026-07-26): the homeserver, tuwunel, publishes **linux-gnu
# builds only** — no Windows, no macOS. lk-jwt-service is likewise Linux-only. So only Linux
# installers can carry a complete box today. On other platforms this script stages whatever
# exists and exits 0; the build still succeeds, and the box falls back to its app-data bin dir
# (the manual `fetch-sidecars.sh` route) — it simply won't be one-click.
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$HERE/src-tauri/sidecars"
mkdir -p "$OUT"

case "$(uname -s)" in
  Linux) ;;
  *)
    echo "note: $(uname -s) — tuwunel/lk-jwt have no upstream build for this platform;"
    echo "      shipping the app without bundled sidecars (see script header)."
    exit 0
    ;;
esac

# Reuse the proven fetcher, pointed at our staging dir instead of the user's runtime bin dir.
PUREPRIVACY_BIN_DIR="$OUT" "$HERE/scripts/fetch-sidecars.sh" "$@"

echo
echo "staged into $OUT:"
missing=0
for b in tor tuwunel caddy livekit-server lk-jwt-service; do
  if [ -f "$OUT/$b" ]; then
    chmod +x "$OUT/$b"
    printf '  %-16s %s\n' "$b" "$(du -h "$OUT/$b" | cut -f1)"
  else
    printf '  %-16s MISSING\n' "$b"
    # tuwunel is the homeserver: without it the bundle is not a box at all.
    [ "$b" = "tuwunel" ] && missing=1
  fi
done
if [ "$missing" = 1 ]; then
  echo "error: tuwunel missing — the bundle would install a non-working box" >&2
  exit 1
fi
