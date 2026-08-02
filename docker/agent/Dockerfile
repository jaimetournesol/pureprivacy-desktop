# PurePrivacy agent — Hermes Agent + Hermes WebUI, built from source, pinned together.
#
# WHY WE BUILD THIS OURSELVES rather than pulling the upstream images:
#
#   1. The WebUI imports hermes-agent internals directly (its own README warns that version
#      skew causes import/behaviour drift, and its compose file shares an agent-source volume
#      between the two containers). Two independently-tagged `:latest` images therefore
#      GUARANTEE drift. One image built from two pinned commits makes that impossible.
#   2. Size: `nousresearch/hermes-agent:latest` is ~3.9 GB. The dependency set we actually
#      need resolves to ~380 MB, which matters when a box runs on a Pi or a NAS.
#   3. Supply chain: the WebUI can execute shell commands. Vendoring a third-party `:latest`
#      with that capability into a privacy appliance is not something to do implicitly — we
#      pin the exact commits we reviewed.
#
# The container shares the BOX's network namespace (`--network container:pureprivacy-box`),
# so it reaches tuwunel on 127.0.0.1:8118 and tor's SOCKS on 127.0.0.1:9150 with no new
# port exposure, and the WebUI's 127.0.0.1:8787 lands on the box's loopback where tor can
# map a (client-authorised) onion port onto it. Nothing here publishes a port itself.
FROM python:3.11-slim-trixie AS build

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      git build-essential pkg-config libolm-dev ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Pinned revisions — bump BOTH together, never one alone (see reason 1 above).
# Full 40-char SHAs are required: `git fetch --depth 1 <sha>` rejects an abbreviation.
ARG HERMES_AGENT_REPO=https://github.com/NousResearch/hermes-agent.git
ARG HERMES_AGENT_REF=45c15d33e5ed7f42c6b4778ab8d34b40ad27cc86
ARG HERMES_WEBUI_REPO=https://github.com/nesquena/hermes-webui.git
ARG HERMES_WEBUI_REF=320789ae596a3963d726d90f6c7f3bc86f7f2d6d

WORKDIR /opt/hermes
RUN git init agent && cd agent \
    && git remote add origin "$HERMES_AGENT_REPO" \
    && git fetch --depth 1 origin "$HERMES_AGENT_REF" \
    && git checkout FETCH_HEAD
RUN git init webui && cd webui \
    && git remote add origin "$HERMES_WEBUI_REPO" \
    && git fetch --depth 1 origin "$HERMES_WEBUI_REF" \
    && git checkout FETCH_HEAD

# ── Patch: don't probe the endpoint on every keystroke ───────────────────────────────────
# Upstream's base-URL field runs `oninput="...;_scheduleOnboardingProbe()"`, which fires a
# probe 400ms after each keystroke. The probe calls _setOnboardingProbeState(), which calls
# _renderOnboardingBody() — and that REBUILDS the <input> you are typing into. The element
# is destroyed and replaced, so focus is lost; on Android the keyboard closes with it, which
# makes the field effectively untypable on a phone (and fires connection attempts the user
# never asked for).
#
# We keep both deliberate triggers: onblur (you've finished typing) and the explicit "Test
# connection" button. Only the per-keystroke probe goes.
#
# `grep -q` first so a future HERMES_WEBUI_REF bump FAILS THE BUILD rather than silently
# shipping without the patch — a sed that matches nothing exits 0 and would look fine.
RUN cd webui/static \
    && grep -q 'this.value;_scheduleOnboardingProbe()' onboarding.js \
    && sed -i 's/this\.value;_scheduleOnboardingProbe()/this.value/' onboarding.js \
    && ! grep -q 'this.value;_scheduleOnboardingProbe()' onboarding.js \
    && echo "patched: base-URL field no longer probes on every keystroke"

COPY --from=ghcr.io/astral-sh/uv:latest /uv /usr/local/bin/uv

# The venv lives OUTSIDE the agent's own checkout on purpose: hermes-agent's README warns
# that a venv inside the tree the agent operates on can be wiped by a relative-path command
# the agent runs against its own source, killing the runtime mid-session.
ENV UV_NO_CONFIG=1
RUN uv venv /opt/hermes/venv --python 3.11
ENV VIRTUAL_ENV=/opt/hermes/venv
# `.[all]` (no dev extras in a shipping image) plus the Matrix stack, which `[all]` does NOT
# pull: the box IS a Matrix homeserver, so this is the transport the agents actually use.
# mautrix's SOCKS support (aiohttp-socks, rdns) is what lets it resolve a .onion at all.
RUN cd /opt/hermes/agent \
    && uv pip install -e ".[all]" \
    && uv pip install 'mautrix[encryption]' aiohttp-socks asyncpg aiosqlite Markdown \
    && uv pip install -r /opt/hermes/webui/requirements.txt


FROM python:3.11-slim-trixie

ENV DEBIAN_FRONTEND=noninteractive
# Runtime libs + the tools that make the agent able to actually DO things. libolm3 is
# mautrix's E2EE backend; git/ripgrep/ffmpeg are what its tools reach for constantly.
RUN apt-get update && apt-get install -y --no-install-recommends \
      libolm3 git ripgrep ffmpeg curl ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*

# ── sudo ────────────────────────────────────────────────────────────────────────────────
# The agent already runs as uid 0 INSIDE this container, so it has full authority over its
# own environment — apt, pip, /usr/local/bin, all of it. What it lacks is the *command*:
# Hermes assumes sudo exists (84 of its files mention it; it even auto-rewrites bare sudo
# calls and has a SUDO_PASSWORD setting), so `sudo apt install x` fails with "sudo: not
# found" even though the privilege was never in question. That's a confusing failure to
# debug from a chat window.
#
# A shim, NOT the real thing. Installing sudo would add a setuid-root binary and a password
# path to an image whose only user is already root — pure attack surface for zero gain.
# This drops sudo's own flags and runs the command, which is exactly what real sudo would
# do here.
#
# To be explicit about the boundary: this grants nothing on the HOST. The container has no
# docker.sock, no host mounts, no added capabilities and is not privileged, and it must stay
# that way — any of those would make root-in-container mean root-on-your-machine.
RUN printf '%s\n' \
    '#!/bin/sh' \
    '# sudo shim — we are already uid 0. See docker/agent/Dockerfile for why.' \
    'while [ $# -gt 0 ]; do' \
    '  case "$1" in' \
    '    -u|--user|-g|--group|-p|--prompt|-C|--close-from|-r|--role|-t|--type) shift 2 ;;' \
    '    --) shift; break ;;' \
    '    -[!-]*|--*) shift ;;' \
    '    *) break ;;' \
    '  esac' \
    'done' \
    '[ $# -eq 0 ] && exec /bin/sh -l' \
    'exec "$@"' \
    > /usr/local/bin/sudo \
    && chmod 0755 /usr/local/bin/sudo \
    && /usr/local/bin/sudo -n true \
    && [ "$(/usr/local/bin/sudo -u root id -u)" = "0" ] \
    && echo "sudo shim installed"

COPY --from=build /opt/hermes /opt/hermes
COPY entrypoint.sh /opt/hermes/entrypoint.sh
RUN chmod +x /opt/hermes/entrypoint.sh

# Everything stateful lives under /data so ONE volume carries it: config, the agent's
# profiles (= the agents themselves), sessions, memories, skills, and ~/.hermes/.env which
# holds the model API keys. Treat this volume as secret material — it is not covered by
# `pp-box backup` today, which tars only the box's own /data.
ENV HERMES_HOME=/data/hermes \
    HERMES_WEBUI_AGENT_DIR=/opt/hermes/agent \
    HERMES_WEBUI_PYTHON=/opt/hermes/venv/bin/python \
    HERMES_WEBUI_STATE_DIR=/data/hermes/webui \
    HERMES_WEBUI_HOST=127.0.0.1 \
    HERMES_WEBUI_PORT=8787 \
    HERMES_WORKSPACE=/data/workspace \
    PATH=/opt/hermes/venv/bin:$PATH

VOLUME /data

ENTRYPOINT ["/opt/hermes/entrypoint.sh"]
