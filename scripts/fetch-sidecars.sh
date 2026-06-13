#!/usr/bin/env bash
#
# fetch-sidecars.sh — fetch the two PurePrivacy sidecar binaries for local dev.
#
#   tuwunel    — Matrix homeserver, extracted from the upstream OCI image
#   tor        — C-tor daemon, copied from the system install (or apt-installed)
#   turnserver — coturn for 1:1 voice (OPTIONAL — box runs fine without it)
#
# Binaries land in $PUREPRIVACY_BIN_DIR, defaulting to the app's runtime bin dir:
#   $HOME/.local/share/ai.tournesol.pureprivacy/bin
#
# Usage:
#   ./scripts/fetch-sidecars.sh              # fetch both (idempotent)
#   ./scripts/fetch-sidecars.sh --force      # re-fetch even if present
#   ./scripts/fetch-sidecars.sh --uninstall  # remove both binaries
#
set -euo pipefail

BIN_DIR="${PUREPRIVACY_BIN_DIR:-$HOME/.local/share/ai.tournesol.pureprivacy/bin}"
TUWUNEL_IMAGE="ghcr.io/matrix-construct/tuwunel:latest"

# ---------------------------------------------------------------- colors ----
if [[ -t 1 ]]; then
  C_RESET=$'\033[0m' C_GREEN=$'\033[32m' C_YELLOW=$'\033[33m' C_RED=$'\033[31m' C_BLUE=$'\033[34m' C_BOLD=$'\033[1m'
else
  C_RESET='' C_GREEN='' C_YELLOW='' C_RED='' C_BLUE='' C_BOLD=''
fi
info() { printf '%s==>%s %s\n' "$C_BLUE"   "$C_RESET" "$*"; }
ok()   { printf '%s ok %s %s\n' "$C_GREEN"  "$C_RESET" "$*"; }
warn() { printf '%swarn%s %s\n' "$C_YELLOW" "$C_RESET" "$*"; }
err()  { printf '%s err%s %s\n' "$C_RED"    "$C_RESET" "$*" >&2; }

# ----------------------------------------------------------------- flags ----
FORCE=0
UNINSTALL=0
for arg in "$@"; do
  case "$arg" in
    --force)     FORCE=1 ;;
    --uninstall) UNINSTALL=1 ;;
    -h|--help)
      sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) err "unknown flag: $arg (try --help)"; exit 2 ;;
  esac
done

# ------------------------------------------------------------- uninstall ----
if [[ "$UNINSTALL" == 1 ]]; then
  info "Removing sidecar binaries from $BIN_DIR"
  for bin in tuwunel tor turnserver; do
    if [[ -e "$BIN_DIR/$bin" ]]; then
      rm -f "$BIN_DIR/$bin"
      ok "removed $BIN_DIR/$bin"
    else
      warn "$BIN_DIR/$bin not present — nothing to do"
    fi
  done
  rmdir "$BIN_DIR" 2>/dev/null && ok "removed empty dir $BIN_DIR" || true
  exit 0
fi

mkdir -p "$BIN_DIR"
info "Sidecar bin dir: ${C_BOLD}$BIN_DIR${C_RESET}"
if [[ -z "${PUREPRIVACY_BIN_DIR:-}" ]]; then
  info "Override with:   export PUREPRIVACY_BIN_DIR=\"$BIN_DIR\""
fi

# verify <path> — true if the binary runs and reports a version
verify() { "$1" --version >/dev/null 2>&1; }

FAILURES=0

# --------------------------------------------------------------- tuwunel ----
fetch_tuwunel() {
  local dest="$BIN_DIR/tuwunel"

  if [[ "$FORCE" == 0 && -x "$dest" ]] && verify "$dest"; then
    ok "tuwunel already present ($("$dest" --version 2>/dev/null | head -n1)) — skipping"
    return 0
  fi

  if ! command -v docker >/dev/null 2>&1; then
    err "docker is required to extract tuwunel from $TUWUNEL_IMAGE"
    return 1
  fi

  info "Pulling $TUWUNEL_IMAGE"
  docker pull --quiet "$TUWUNEL_IMAGE" >/dev/null || { err "docker pull failed"; return 1; }

  info "Extracting /usr/bin/tuwunel from image"
  local cid
  cid="$(docker create "$TUWUNEL_IMAGE")" || { err "docker create failed"; return 1; }
  if ! docker cp "$cid:/usr/bin/tuwunel" "$dest"; then
    docker rm -f "$cid" >/dev/null 2>&1 || true
    err "failed to copy tuwunel out of the image"
    return 1
  fi
  docker rm -f "$cid" >/dev/null

  chmod 0755 "$dest"
  if verify "$dest"; then
    ok "tuwunel installed: $("$dest" --version 2>/dev/null | head -n1)"
  else
    err "tuwunel was copied but '$dest --version' failed (wrong arch or missing libs?)"
    return 1
  fi
}

# ------------------------------------------------------------------- tor ----
fetch_tor() {
  local dest="$BIN_DIR/tor"

  if [[ "$FORCE" == 0 && -x "$dest" ]] && verify "$dest"; then
    ok "tor already present ($("$dest" --version 2>/dev/null | head -n1)) — skipping"
    return 0
  fi

  # find_system_tor: PATH first, then sbin dirs (Debian installs to /usr/sbin)
  find_system_tor() {
    command -v tor 2>/dev/null && return 0
    local p
    for p in /usr/sbin/tor /usr/local/sbin/tor /usr/bin/tor; do
      [[ -x "$p" ]] && { echo "$p"; return 0; }
    done
    return 1
  }

  local sys_tor=""
  if sys_tor="$(find_system_tor)"; then
    info "Copying system tor from $sys_tor"
  elif command -v apt-get >/dev/null 2>&1; then
    warn "No system tor found — installing via apt (this will prompt for sudo)"
    sudo apt-get install -y tor || { err "apt-get install tor failed"; return 1; }
    sys_tor="$(find_system_tor)" || { err "tor installed but binary not found"; return 1; }
  else
    err "No system tor and no apt-get available."
    err "Install the Tor Expert Bundle manually:"
    err "  1. Download for your platform: https://www.torproject.org/download/tor/"
    err "  2. Extract and copy the 'tor' binary to: $dest"
    err "  3. chmod 0755 $dest"
    return 1
  fi

  cp "$sys_tor" "$dest" || { err "failed to copy $sys_tor"; return 1; }
  chmod 0755 "$dest"
  if verify "$dest"; then
    ok "tor installed: $("$dest" --version 2>/dev/null | head -n1)"
  else
    err "tor was copied but '$dest --version' failed"
    return 1
  fi
}

# ------------------------------------------------------------- turnserver ----
# Optional: 1:1 voice. A missing turnserver leaves the box fully functional
# (chat + federation), just without calls — so this never fails the script.
fetch_turn() {
  local dest="$BIN_DIR/turnserver"

  if [[ "$FORCE" == 0 && -x "$dest" ]] && verify "$dest"; then
    ok "turnserver already present ($("$dest" --version 2>/dev/null | head -n1)) — skipping"
    return 0
  fi

  local sys_turn=""
  for p in "$(command -v turnserver 2>/dev/null || true)" /usr/bin/turnserver /usr/sbin/turnserver /usr/local/bin/turnserver; do
    [[ -n "$p" && -x "$p" ]] && { sys_turn="$p"; break; }
  done

  if [[ -z "$sys_turn" ]] && command -v apt-get >/dev/null 2>&1; then
    warn "No system coturn found — installing via apt (prompts for sudo)"
    sudo apt-get install -y coturn || { warn "apt-get install coturn failed — voice will be unavailable"; return 1; }
    for p in /usr/bin/turnserver /usr/sbin/turnserver; do [[ -x "$p" ]] && { sys_turn="$p"; break; }; done
  fi

  if [[ -z "$sys_turn" ]]; then
    warn "No coturn available — the box will run WITHOUT 1:1 voice."
    warn "Install it later with: sudo apt-get install coturn   (then re-run this script)"
    return 1
  fi

  cp "$sys_turn" "$dest" || { warn "failed to copy $sys_turn — voice unavailable"; return 1; }
  chmod 0755 "$dest"
  if verify "$dest"; then
    ok "turnserver installed: $("$dest" --version 2>/dev/null | head -n1)"
  else
    warn "turnserver copied but '$dest --version' failed — voice may not work"
    return 1
  fi
}

fetch_tuwunel || FAILURES=$((FAILURES + 1))
fetch_tor     || FAILURES=$((FAILURES + 1))
fetch_turn    || warn "voice (coturn) not installed — this is OPTIONAL, box still works"

echo
if [[ "$FAILURES" -gt 0 ]]; then
  err "$FAILURES sidecar(s) failed — see messages above"
  exit 1
fi
ok "${C_BOLD}All sidecars ready in $BIN_DIR${C_RESET}"
info "If you use a custom dir, make sure the app sees it:"
printf '    export PUREPRIVACY_BIN_DIR="%s"\n' "$BIN_DIR"
