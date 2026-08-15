#!/usr/bin/env bash
# test-pp-box.sh — smoke tests for the pp-box CLI's backup/restore/destroy paths.
#
# Why this exists: pp-box is ~600 lines of bash guarding the box's identity, and it has
# already shipped one silent data-loss class bug (backup covering only the box volume) and
# one silent misclassification bug (`tar tzf | grep -q` under pipefail reading every large
# bundle as "no match" via SIGPIPE). Both would have been caught by exactly the round-trips
# below. Runs against THROWAWAY volumes and a scratch install dir — it never touches a real
# box's volumes, and it refuses to start if a pureprivacy-box container is running.
#
#   ./scripts/test-pp-box.sh          # run all tests (needs docker; ~30s)
#
set -uo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
PPBOX="$HERE/docker/pp-box"
PREFIX="ppboxtest-$$"
WORK="$(mktemp -d)"
PASS=0
FAIL=0

say()  { printf '\033[34m==>\033[0m %s\n' "$*"; }
good() { printf '\033[32m ok \033[0m %s\n' "$*"; PASS=$((PASS+1)); }
bad()  { printf '\033[31mFAIL\033[0m %s\n' "$*"; FAIL=$((FAIL+1)); }

cleanup() {
  # update-test leftovers: the stand-in compose project, the throwaway registry, its images
  docker rm -f "$PREFIX-upd-box" "$PREFIX-upd-agent" "$PREFIX-registry" >/dev/null 2>&1
  docker network rm "$PREFIX-upd_default" >/dev/null 2>&1
  docker images --format '{{.Repository}}:{{.Tag}}' | grep "^127.0.0.1:${RPORT:-none}/" | xargs -r docker rmi -f >/dev/null 2>&1
  docker volume ls --format '{{.Name}}' | grep "^$PREFIX" | xargs -r docker volume rm -f >/dev/null 2>&1
  rm -rf "$WORK"
}
trap cleanup EXIT

command -v docker >/dev/null || { echo "docker required"; exit 2; }
# Never run while a real box is up: restore paths refuse when the container exists, which
# would make every restore test fail confusingly — and pointing at a live install by
# accident must be impossible, so everything below uses $PREFIX-* volumes only.
if docker ps --format '{{.Names}}' | grep -qx pureprivacy-box; then
  echo "a pureprivacy-box container is running — stop it first (./pp-box down)"; exit 2
fi

# ---- a fake install: scratch dir with .env + pp-box, volumes seeded with marker files ----
BOXV="$PREFIX-box"; AGENTV="$PREFIX-agent"; HANDV="$PREFIX-handoff"
INSTALL="$WORK/install"; mkdir -p "$INSTALL"
cp "$PPBOX" "$INSTALL/pp-box"; chmod +x "$INSTALL/pp-box"
cat > "$INSTALL/.env" <<EOF
PP_USER=tester
PP_BOX=testbox
PP_PASS=irrelevant
PP_SECRETS_KEY=THE-REAL-KEY
PP_VOLUME=$BOXV
PP_AGENT_VOLUME=$AGENTV
PP_AGENT_HANDOFF_VOLUME=$HANDV
EOF

seed() { # $1=volume $2=marker-relpath $3=content
  docker volume create "$1" >/dev/null
  docker run --rm -v "$1":/v -e P="$2" -e C="$3" alpine \
    sh -c 'mkdir -p "/v/$(dirname "$P")"; printf "%s" "$C" > "/v/$P"'
}
say "seeding throwaway volumes"
seed "$BOXV" "box.json" '{"box_name":"testbox","username":"tester","onion":"aaaabbbbccccddddeeeeffffgggghhhhiiiijjjjkkkkllllmmmmnnnn.onion"}'
seed "$BOXV" "data/tor/hs/hs_ed25519_secret_key" "FAKE-ONION-KEY"
seed "$AGENTV" "hermes/auth.json" '{"providers":[{"provider":"fake"}]}'
seed "$HANDV" "webui-password" "hunter2"

vol_file() { docker run --rm -v "$1":/v:ro alpine cat "/v/$2" 2>/dev/null; }

# ------------------------------------------------------ update: a Docker-Hub install ----
# `pp-box update <ver>` on an install that runs a REGISTRY image must pull that tag, pin it
# in .env (box — and agent only while the add-on is on), and recreate; it must never call
# build.sh, and a failed pull must leave .env and the containers exactly as they were.
# Before this it always ran build.sh (which needs a Tauri build no user machine has), and
# the `docker pull` the box told owners to run first never reached compose anyway — it kept
# the tag .env named. A throwaway local registry stands in for Docker Hub, alpine for the
# images, and a stand-in compose file carries the same env contract as the real one.
say "update on a Docker-Hub install pulls + pins the version, never builds, fails closed"
RPORT=""
docker run -d --name "$PREFIX-registry" -p 127.0.0.1:0:5000 registry:2 >/dev/null 2>&1
RPORT="$(docker port "$PREFIX-registry" 5000/tcp 2>/dev/null | head -1 | sed 's/.*://')"
if [ -n "$RPORT" ]; then
  for i in $(seq 1 30); do
    if command -v curl >/dev/null 2>&1; then curl -sf "http://127.0.0.1:$RPORT/v2/" >/dev/null 2>&1 && break
    else sleep 3; break; fi
    sleep 1
  done
  BOXREPO="127.0.0.1:$RPORT/pp-box"; AGREPO="127.0.0.1:$RPORT/pp-agent"
  for t in 0.1.10 0.1.11; do
    docker tag alpine "$BOXREPO:$t" && docker push -q "$BOXREPO:$t" >/dev/null 2>&1
    docker tag alpine "$AGREPO:$t"  && docker push -q "$AGREPO:$t"  >/dev/null 2>&1
  done
  # Drop the local copies of the NEW tag: an update has to genuinely pull it.
  docker rmi "$BOXREPO:0.1.11" "$AGREPO:0.1.11" >/dev/null 2>&1
  INSTALLU="$WORK/installu"; mkdir -p "$INSTALLU"
  cp "$PPBOX" "$INSTALLU/pp-box"; chmod +x "$INSTALLU/pp-box"
  # A build.sh that only leaves a fingerprint — the Hub path must never reach it.
  printf '#!/bin/sh\ntouch "$(dirname "$0")/BUILD-SH-WAS-CALLED"\n' > "$INSTALLU/build.sh"
  chmod +x "$INSTALLU/build.sh"
  cat > "$INSTALLU/docker-compose.yml" <<EOF
name: $PREFIX-upd
services:
  box:
    image: \${PP_IMAGE:-pureprivacy-box:dev}
    container_name: $PREFIX-upd-box
    command: ["sleep", "300"]
  agent:
    image: \${PP_AGENT_IMAGE:-jaimemelon/pureprivacy-agent:latest}
    container_name: $PREFIX-upd-agent
    profiles: ["agents"]
    command: ["sleep", "300"]
EOF
  cat > "$INSTALLU/.env" <<EOF
PP_USER=tester
PP_BOX=testbox
PP_PASS=irrelevant
PP_SECRETS_KEY=k
PP_VOLUME=$PREFIX-upd-vol
PP_AGENTS=1
PP_IMAGE=$BOXREPO:0.1.10
PP_AGENT_IMAGE=$AGREPO:0.1.10
EOF
  ( cd "$INSTALLU" && ./pp-box update 0.1.11 ) >/dev/null 2>&1; rc=$?
  [ "$rc" = 0 ] && good "update 0.1.11 exits 0 on a Hub install" || bad "update 0.1.11 failed (rc=$rc)"
  [ ! -e "$INSTALLU/BUILD-SH-WAS-CALLED" ] \
    && good "build.sh never invoked on a Hub install" || bad "build.sh was invoked on a Hub install"
  grep -q "^PP_IMAGE=$BOXREPO:0.1.11\$" "$INSTALLU/.env" \
    && good ".env pins PP_IMAGE to 0.1.11" || bad ".env has $(grep ^PP_IMAGE= "$INSTALLU/.env")"
  grep -q "^PP_AGENT_IMAGE=$AGREPO:0.1.11\$" "$INSTALLU/.env" \
    && good ".env pins PP_AGENT_IMAGE to 0.1.11 (add-on on)" || bad ".env has $(grep ^PP_AGENT_IMAGE= "$INSTALLU/.env")"
  bimg="$(docker inspect "$PREFIX-upd-box" --format '{{.Config.Image}}' 2>/dev/null)"
  aimg="$(docker inspect "$PREFIX-upd-agent" --format '{{.Config.Image}}' 2>/dev/null)"
  [ "$bimg" = "$BOXREPO:0.1.11" ] && good "box container recreated on 0.1.11" || bad "box container runs '$bimg'"
  [ "$aimg" = "$AGREPO:0.1.11" ] && good "agent container recreated on 0.1.11" || bad "agent container runs '$aimg'"

  # A tag that doesn't exist: fail, and change nothing.
  ( cd "$INSTALLU" && ./pp-box update 9.9.9 ) >/dev/null 2>&1; rc=$?
  [ "$rc" != 0 ] && good "update to a missing tag fails" || bad "update 9.9.9 exited 0"
  grep -q "^PP_IMAGE=$BOXREPO:0.1.11\$" "$INSTALLU/.env" \
    && good ".env untouched after the failed pull" || bad ".env changed after a failed pull"
  [ "$(docker inspect "$PREFIX-upd-box" --format '{{.Config.Image}}' 2>/dev/null)" = "$BOXREPO:0.1.11" ] \
    && good "container untouched after the failed pull" || bad "container changed after a failed pull"

  # A plain box (add-on off) must not touch the agent image at all.
  sed -i 's/^PP_AGENTS=.*/PP_AGENTS=0/' "$INSTALLU/.env"
  ( cd "$INSTALLU" && ./pp-box update 0.1.10 ) >/dev/null 2>&1
  grep -q "^PP_IMAGE=$BOXREPO:0.1.10\$" "$INSTALLU/.env" && grep -q "^PP_AGENT_IMAGE=$AGREPO:0.1.11\$" "$INSTALLU/.env" \
    && good "add-on off: box repinned, agent image left alone" || bad "add-on off: $(grep -E '^PP_(AGENT_)?IMAGE=' "$INSTALLU/.env" | tr '\n' ' ')"

  # A source install (local image name, no registry namespace) still goes through build.sh.
  sed -i 's|^PP_IMAGE=.*|PP_IMAGE=pp-box:dev|' "$INSTALLU/.env"
  ( cd "$INSTALLU" && ./pp-box update ) >/dev/null 2>&1
  [ -e "$INSTALLU/BUILD-SH-WAS-CALLED" ] \
    && good "source install still rebuilds via build.sh" || bad "source install skipped build.sh"
  ( cd "$INSTALLU" && docker compose --profile agents down --remove-orphans ) >/dev/null 2>&1
else
  bad "could not start a throwaway registry (registry:2) — update path untested"
fi

# --------------------------------------------------------------------- backup: bundle ----
say "backup produces a format-2 bundle covering all three volumes"
( cd "$INSTALL" && ./pp-box backup "$WORK/backups" ) >/dev/null 2>&1
BUNDLE="$(ls "$WORK/backups"/pp-box-*.tgz 2>/dev/null | head -1)"
if [ -n "$BUNDLE" ]; then good "backup wrote $(basename "$BUNDLE")"; else bad "no bundle written"; fi

MEMBERS="$(tar tzf "$BUNDLE" 2>/dev/null)"
missing=""
for m in MANIFEST box.tgz agent-data.tgz agent-handoff.tgz; do
  echo "$MEMBERS" | grep -qx "./$m" || missing="$missing $m"
done
if [ -z "$missing" ]; then
  good "bundle holds MANIFEST + box + agent-data + agent-handoff"
else
  bad "bundle is missing:$missing (has: $(echo "$MEMBERS" | tr '\n' ' '))"
fi

MAN="$(tar xzf "$BUNDLE" -O ./MANIFEST 2>/dev/null)"
echo "$MAN" | grep -q '^secrets_key=THE-REAL-KEY$' \
  && good "MANIFEST carries the secrets key" \
  || bad "MANIFEST is missing the secrets key (an undecryptable backup)"
echo "$MAN" | grep -q '^onion=aaaabbbbccccddddeeeeffffgggghhhhiiiijjjjkkkkllllmmmmnnnn.onion$' \
  && good "MANIFEST records the onion (box was down — read from the volume, not the container)" \
  || bad "MANIFEST onion wrong/missing: $(echo "$MAN" | grep ^onion= || echo none)"

perm="$(stat -c %a "$BUNDLE" 2>/dev/null || stat -f %Lp "$BUNDLE")"
[ "$perm" = "600" ] && good "bundle is 0600" || bad "bundle perms are $perm, wanted 600"

# ------------------------------------------------- restore: bundle into fresh volumes ----
say "restore round-trips the bundle into a second set of volumes"
INSTALL2="$WORK/install2"; mkdir -p "$INSTALL2"
cp "$PPBOX" "$INSTALL2/pp-box"; chmod +x "$INSTALL2/pp-box"
# Same volume names would collide with the seeded set; a restore must land in ITS install's
# volumes. Wrong secrets key on purpose: the repair prompt is part of the contract.
sed -e "s/$BOXV/$PREFIX-box2/" -e "s/$AGENTV/$PREFIX-agent2/" -e "s/$HANDV/$PREFIX-handoff2/" \
    -e 's/THE-REAL-KEY/WRONG-KEY/' "$INSTALL/.env" > "$INSTALL2/.env"
( cd "$INSTALL2" && printf 'y\ny\n' | ./pp-box restore "$BUNDLE" ) >/dev/null 2>&1

[ "$(vol_file "$PREFIX-box2" data/tor/hs/hs_ed25519_secret_key)" = "FAKE-ONION-KEY" ] \
  && good "box volume restored (onion key byte-identical)" || bad "box volume content wrong after restore"
[ "$(vol_file "$PREFIX-agent2" hermes/auth.json)" = '{"providers":[{"provider":"fake"}]}' ] \
  && good "agent volume restored" || bad "agent volume content wrong after restore"
[ "$(vol_file "$PREFIX-handoff2" webui-password)" = "hunter2" ] \
  && good "handoff volume restored" || bad "handoff volume content wrong after restore"
grep -q '^PP_SECRETS_KEY=THE-REAL-KEY$' "$INSTALL2/.env" \
  && good "mismatched PP_SECRETS_KEY repaired in .env (with consent)" \
  || bad ".env key not repaired: $(grep ^PP_SECRETS_KEY= "$INSTALL2/.env")"
ls "$INSTALL2"/.env.bak-* >/dev/null 2>&1 \
  && good "previous .env kept alongside" || bad "no .env backup was kept"

# ---------------------------------------------- restore: legacy single-volume backups ----
say "legacy (pre-bundle) backups still restore"
docker run --rm -v "$BOXV":/v:ro -v "$WORK":/out alpine tar czf /out/legacy.tgz -C /v . >/dev/null
INSTALL3="$WORK/install3"; mkdir -p "$INSTALL3"
cp "$PPBOX" "$INSTALL3/pp-box"; chmod +x "$INSTALL3/pp-box"
sed "s/$BOXV/$PREFIX-box3/" "$INSTALL/.env" > "$INSTALL3/.env"
( cd "$INSTALL3" && printf 'y\n' | ./pp-box restore "$WORK/legacy.tgz" ) >/dev/null 2>&1
[ "$(vol_file "$PREFIX-box3" data/tor/hs/hs_ed25519_secret_key)" = "FAKE-ONION-KEY" ] \
  && good "legacy tar restored as a plain volume (not treated as a bundle)" \
  || bad "legacy restore broken"

# The reverse misclassification is the SIGPIPE bug: a large bundle read as legacy fills
# the volume with tarballs. The bundle restore above already proves bundles classify as
# bundles — this asserts the extracted volume holds real content, not member tarballs.
if docker run --rm -v "$PREFIX-box2":/v:ro alpine sh -c 'ls /v/box.tgz' >/dev/null 2>&1; then
  bad "bundle was extracted AS a legacy tar (member tarballs in the volume)"
else
  good "no member tarballs in restored volume (bundle/legacy sniff is sound)"
fi

# ------------------------------------------------------------------------- destroy ----
say "destroy removes ALL three volumes, not just the box"
( cd "$INSTALL" && printf 'testbox\n' | ./pp-box destroy ) >/dev/null 2>&1
left="$(docker volume ls --format '{{.Name}}' | grep -cE "^($BOXV|$AGENTV|$HANDV)$")"
[ "$left" = "0" ] && good "box, agent and handoff volumes all gone" \
  || bad "$left volume(s) survived destroy"

# destroy must be gated on the exact box name — a wrong name removes nothing.
seed "$PREFIX-box4" "box.json" '{}'
INSTALL4="$WORK/install4"; mkdir -p "$INSTALL4"
cp "$PPBOX" "$INSTALL4/pp-box"; chmod +x "$INSTALL4/pp-box"
sed "s/$BOXV/$PREFIX-box4/" "$INSTALL/.env" > "$INSTALL4/.env"
( cd "$INSTALL4" && printf 'WRONG-NAME\n' | ./pp-box destroy ) >/dev/null 2>&1
docker volume inspect "$PREFIX-box4" >/dev/null 2>&1 \
  && good "wrong confirmation name removes nothing" \
  || bad "destroy ran despite a wrong confirmation name"

# ------------------------------------------------------------------------- summary ----
echo
if [ "$FAIL" = 0 ]; then
  printf '\033[32m%d passed\033[0m, 0 failed\n' "$PASS"
else
  printf '%d passed, \033[31m%d FAILED\033[0m\n' "$PASS" "$FAIL"
  exit 1
fi
