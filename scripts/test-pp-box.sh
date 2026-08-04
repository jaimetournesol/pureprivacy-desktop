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
