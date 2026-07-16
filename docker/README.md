# PurePrivacy box in Docker

Run your box as a container — on a Linux server, a NAS, a Raspberry Pi, a cloud VPS, or
Windows/macOS via Docker Desktop. One image, everywhere Docker runs.

**No ports to publish.** The box is reached only through its `.onion` (outbound Tor
rendezvous), so there's nothing to expose or forward — just a **named volume** that holds
the box's identity (onion key, admin account, `secrets.json`, `pairings.json`). Lose that
volume and you lose the box.

## Build

```bash
./build.sh          # stages the host-built binary + sidecars, then `docker build`
```

Stage 1 reuses the **prebuilt** Tauri binary + sidecars from your machine (fast to iterate).
A shipping image will build both inside a multi-stage Dockerfile.

## Run

```bash
docker compose up -d           # (edit PP_* in your env / an .env file first)
docker compose logs -f box     # watch it mint its onion + print the connect QR
```

or plain Docker:

```bash
docker volume create pp-data
docker run -d --name pureprivacy-box --restart unless-stopped -v pp-data:/data \
  -e PP_USER=jaime -e PP_PASS='a-strong-password' -e PP_BOX=mybox \
  -e PP_SECRETS_KEY="$(openssl rand -base64 32)" \
  pureprivacy-box:dev
docker logs -f pureprivacy-box
```

On first run it provisions (mints the onion, creates the admin account) and prints a
scannable QR in the logs — open the PurePrivacy phone app and scan it to connect. On every
later run it just resumes on the persisted volume (same onion, no re-provision).

`PP_SECRETS_KEY` must stay the **same** across restarts (it decrypts `secrets.json`) —
generate it once and keep it.

## Verified (Stage 1)

- Boots, provisions, mints its onion + admin account inside the container.
- Identity persists in `/data`; a fresh container **resumes with the same onion**.
- Survives `docker restart`.
- **Reachable over Tor via its `.onion` with no published ports** (proven box-to-box).
- Connect QR printed to the logs.

## Known limits (Stage 1 → follow-ups)

- **Image is ~1.2 GB.** The current binary is the Tauri GUI (links webkit2gtk), so we run
  it under Xvfb and ship webkit/gtk/xvfb (~556 MB) just to satisfy that. **Stage 2** — a
  headless box runner with no Tauri — drops all of it (down to ~sidecars only).
- **Voice/calls (coturn) off.** The `turnserver` binary pulls a pile of version-pinned DB
  client libs it never uses here; chat + federation work fully without it. A lean coturn
  build (or a maintained coturn image) re-enables calls.
- **amd64 only** so far — multi-arch (arm64 for Pi / Apple Silicon) is a buildx pass once
  the sidecars are sourced as multi-arch.

The Stage-2 headless runner is the same "box without the GUI" piece that also unlocks a
setup CLI, WSL, and headless Raspberry-Pi installs.
