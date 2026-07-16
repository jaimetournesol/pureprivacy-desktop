# PurePrivacy box — containerized.
#
# STAGE-1 image: reuses the prebuilt Tauri binary, which links webkit2gtk, so we run it
# under a virtual display (Xvfb) — the GUI renders to nothing; only the supervisor matters.
# It ships webkit/gtk purely to satisfy that link. STAGE 2 (a headless box runner with no
# Tauri) will drop all of this and shrink the image dramatically.
#
# The box is reached only via its .onion (outbound Tor rendezvous), so NO published ports
# are needed — just a persistent volume for the data dir (onion keys, db, secrets, pairings).
FROM ubuntu:26.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      libwebkit2gtk-4.1-0 libgtk-3-0 \
      libayatana-appindicator3-1 \
      libevent-2.1-7 \
      xvfb dbus-x11 \
      ca-certificates fontconfig tzdata \
      qrencode \
    && rm -rf /var/lib/apt/lists/*
# NOTE: voice/coturn (turnserver) is left out of Stage 1 — its binary pulls a pile of DB
# client libs (incl. a version-pinned libmysqlclient) it never uses here. The box runs
# fully (chat + federation) without it; calls are the only thing off until a lean coturn.

# Sidecars (tor, tuwunel, caddy, coturn, livekit-server, lk-jwt-service) + the box binary
# are staged next to this Dockerfile by build.sh (kept out of git — large, host-built).
COPY bin/ /opt/pureprivacy/bin/
COPY pureprivacy /opt/pureprivacy/pureprivacy
COPY entrypoint.sh /opt/pureprivacy/entrypoint.sh
RUN chmod +x /opt/pureprivacy/bin/* /opt/pureprivacy/pureprivacy /opt/pureprivacy/entrypoint.sh

ENV PUREPRIVACY_BIN_DIR=/opt/pureprivacy/bin \
    PUREPRIVACY_DATA_DIR=/data \
    DISPLAY=:99 \
    WEBKIT_DISABLE_COMPOSITING_MODE=1 \
    LIBGL_ALWAYS_SOFTWARE=1

# The data dir MUST persist — it holds the box's identity (onion key, admin account,
# secrets, pairings). Mount a named volume here; losing it = losing the box.
VOLUME /data

ENTRYPOINT ["/opt/pureprivacy/entrypoint.sh"]
