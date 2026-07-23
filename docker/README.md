# PurePrivacy box in Docker

Run your box as a container — on a Linux server, a NAS, a Raspberry Pi, a cloud VPS, or
Windows/macOS via Docker Desktop. One image, everywhere Docker runs.

**No ports to publish.** The box is reached only through its `.onion` (outbound Tor
rendezvous), so there's nothing to expose or forward. Its whole identity — the onion key,
the admin account, `secrets.json`, and `pairings.json` — lives in **one named volume**.
Lose that volume and the box is gone for good, so **back it up**.

> **Your volume name is unique to your install.** `pp-box init` generates one (e.g.
> `pureprivacy-data-a1b2c3d4`) and records it in `.env` as `PP_VOLUME`, so two boxes on the
> same host never collide. **It must never change** — pointing the box at a different name
> gives you a new, empty box. Keep `.env` safe alongside your backups. *(Installs made before
> this existed have no `PP_VOLUME` and keep using the original `pureprivacy-data` — nothing
> to do.)*

## Easiest: pull the published image

No build needed — pull it straight from Docker Hub and finish setup in your browser:

```bash
docker pull jaimemelon/pureprivacy-box:latest
MYVOL=pp-data-$(openssl rand -hex 4)      # your box's data volume — note it down, keep it forever
docker run -d --name pureprivacy-box --restart unless-stopped -v "$MYVOL":/data \
  -p 127.0.0.1:8470:8470 -e PUREPRIVACY_SETUP_BIND=0.0.0.0 \
  jaimemelon/pureprivacy-box:latest
# then open http://127.0.0.1:8470/ in your browser
```

(Or with compose: set `PP_IMAGE=jaimemelon/pureprivacy-box:latest` and `docker compose up -d`.)
The rest of this guide covers building the image yourself and the `pp-box` helper.

## Quick start (build it yourself)

Everything goes through the **`pp-box`** helper in this directory. Set-up is a **one-page
web form** — no need to bake a password into config:

```bash
./pp-box build     # once — build the container image from the host-built binary + sidecars
./pp-box init      # box name + a fresh secrets key → .env (leave the password blank)
./pp-box up        # start the box; it prints your setup URL
```

Then open the URL it prints — **http://127.0.0.1:8470/** — in any browser on this machine:

1. Choose a **username + password** (this is what your phone signs in with — keep it safe).
2. The box provisions and shows a **QR code**.
3. **Scan it in the PurePrivacy phone app** → you're signed in, all over Tor.

The setup page is loopback-only (host `127.0.0.1` only, never the LAN) and **shuts itself
down the moment your phone connects** — setup is one-time.

> Prefer a scripted/non-interactive setup (CI, headless)? Give `init` a password instead and
> the box provisions straight from it; then `./pp-box qr` prints the connect code to the
> terminal (the pre-web-setup behaviour, still supported).

## All commands

| Command | What it does |
|---|---|
| `./pp-box init` | Create `.env` — box name, a fresh `PP_SECRETS_KEY`, and an optional password (blank ⇒ set it in the browser). |
| `./pp-box build` | Build the `pureprivacy-box:dev` image (stages the binary + sidecars). |
| `./pp-box up` | Start the box. First run prints the **web-setup URL** (`http://127.0.0.1:8470/`); a provisioned box just resumes. |
| `./pp-box qr` | Print the phone-connect QR in the terminal (for the scripted/password-in-`.env` path). |
| `./pp-box status` | Running? Shows the onion, uptime, and the volume name. |
| `./pp-box logs` | Follow the logs (watch it mint the onion + boot the sidecars). |
| `./pp-box restart` | Restart the box. |
| `./pp-box down` | Stop the box — identity is kept in the volume. |
| `./pp-box update` | Rebuild the image + recreate the box on the same volume (same onion). |
| `./pp-box backup [dir]` | Tar the volume (onion key + secrets + pairings) → `backups/`. **Do this.** |
| `./pp-box restore <file>` | Restore a backup into the volume (stop the box first). |
| `./pp-box shell` | Open a shell inside the container. |
| `./pp-box destroy` | Remove the box **and** its volume (asks you to type the box name). |

## Windows

Runs on **Docker Desktop for Windows** — pick either front end (same commands, same box):

- **PowerShell (native):** use `pp-box.ps1`, e.g. `./pp-box.ps1 init`, `./pp-box.ps1 up`,
  `./pp-box.ps1 qr`. Same subcommands as the table above.
- **WSL2 / Git Bash:** use the bash `./pp-box` exactly as on Linux. WSL2 is Docker Desktop's
  default backend (real Linux), so this is the most battle-tested path; Git Bash works too
  (the script disables MSYS path-mangling for container mounts).

**One caveat — the image.** `build` bundles a **Linux** box binary + sidecars that are staged
on a Linux host, so it **can't build on native Windows**. Get the image once, then `up`/`qr`
work natively from PowerShell:

```powershell
# on a Linux box (or in WSL2):  cd docker && ./pp-box build && docker save pureprivacy-box:dev -o pp-box.tar
docker load -i pp-box.tar      # ← on Windows
.\pp-box.ps1 init ; .\pp-box.ps1 up      # then open http://127.0.0.1:8470/ in your browser
```

(Or build it directly inside WSL2 and run from there.) A self-contained image you can
`docker build` / `docker pull` on any OS is the Stage-2 follow-up.

## Back up your box — it's the whole identity

An `.onion` address is derived from a secret key that exists **only** in your box's data
volume (the `PP_VOLUME` name in `.env`). If that volume is deleted — or you point the box at a
different name — the address can never come back and your phone is orphaned on a dead box. So
keep a backup (of the volume **and** `.env`):

```bash
./pp-box backup                     # → docker/backups/pp-box-<onion>-N.tgz
```

Recovering onto a new machine (or after an accidental wipe) is the reverse — and it brings
back the **same onion**, so your phone reconnects with no re-pairing:

```bash
./pp-box restore backups/pp-box-….tgz
./pp-box up
```

`PP_SECRETS_KEY` (in `.env`) must also stay the same across restarts — it decrypts
`secrets.json`. `init` generates it once; keep `.env` private (it's `chmod 600` and
git-ignored) and store a copy alongside your backup.

## Verified

- Boots, provisions, mints its onion + admin account inside the container.
- Identity persists in the `pureprivacy-data` volume; a fresh container **resumes with the
  same onion**, and survives `docker restart` / a host reboot.
- **Reachable over Tor via its `.onion` with no published ports** (proven box-to-box).
- **Full feature parity — voice + video calls included.** All six sidecars run (tor,
  tuwunel, caddy, coturn, livekit-server, lk-jwt-service); coturn comes from apt so it gets
  its correct libs.
- **`backup` → wipe → `restore` round-trips the identity** (same onion returns).

## Without the CLI (plain docker / compose)

The CLI just wraps these. `docker compose` reads the `.env` that `init` wrote (all vars are
optional — leave `PP_PASS` unset for the web-setup flow):

```bash
docker compose up -d           # start; the setup page is published to host 127.0.0.1:8470 only
docker compose logs -f box     # watch it come up (and, in the scripted path, print the QR)
docker compose down            # stop
```

Then open **http://127.0.0.1:8470/** and finish setup in your browser (unless you set
`PP_PASS`, in which case it provisions from that and prints the QR to the logs).

Or one plain `docker run` (identity in the `pureprivacy-data` volume; publish the setup port
to host loopback only):

```bash
MYVOL=pp-data-$(openssl rand -hex 4)     # pick a name and KEEP it — it holds your box identity
docker volume create "$MYVOL"
docker run -d --name pureprivacy-box --restart unless-stopped -v "$MYVOL":/data \
  -p 127.0.0.1:8470:8470 \
  -e PUREPRIVACY_SETUP_BIND=0.0.0.0 \
  pureprivacy-box:dev
docker logs -f pureprivacy-box     # then open http://127.0.0.1:8470/ in your browser
```

⚠️ Write that volume name down. Every later `docker run`, backup, or restore must use the
**same** one — a different name is a different (empty) box, and the onion key is unrecoverable.

*(Prefer the non-interactive path? Drop the two setup lines and add
`-e PP_USER=yourname -e PP_PASS='a-strong-password' -e PP_SECRETS_KEY="$(openssl rand -base64 32)"`.
`PP_USER` has no default — it's required whenever `PP_PASS` is set.)*

## Notes & known limits (Stage 1)

- Build is **Stage 1**: it reuses the prebuilt Tauri binary + sidecars from your machine
  (`build.sh`) — fast to iterate. A shipping image would compile both in a multi-stage
  build.
- **Image is ~1.2 GB / amd64.** The binary is the Tauri GUI (links webkit2gtk), run headless
  under Xvfb, so the image ships webkit/gtk/xvfb just to satisfy it. All the *sidecars* have
  arm64 builds, so the only blocker to a multi-arch (arm64 for a Pi / Apple Silicon) image is
  cross-compiling this webkit-linked binary — cleanest to do alongside **Stage 2** (a headless
  box runner with no Tauri), which also shrinks the image.
- **Setup page:** the one-page web setup is the only local surface, bound to host `127.0.0.1`
  only and live **only until your phone signs in**. Everything else is Tor-only over the `.onion`.
