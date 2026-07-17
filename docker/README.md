# PurePrivacy box in Docker

Run your box as a container — on a Linux server, a NAS, a Raspberry Pi, a cloud VPS, or
Windows/macOS via Docker Desktop. One image, everywhere Docker runs.

**No ports to publish.** The box is reached only through its `.onion` (outbound Tor
rendezvous), so there's nothing to expose or forward. Its whole identity — the onion key,
the admin account, `secrets.json`, and `pairings.json` — lives in **one named volume**
(`pureprivacy-data`). Lose that volume and the box is gone for good, so **back it up**.

## Quick start

Everything goes through the **`pp-box`** helper in this directory:

```bash
./pp-box build     # once — build the container image from the host-built binary + sidecars
./pp-box init      # asks for a login user/box name + password, generates a secrets key → .env
./pp-box up        # start the box (detached); it mints its onion on first run
./pp-box qr        # once it's up, print the QR — scan it in the PurePrivacy phone app
```

(`build` and `init` are independent — build makes the image once, init writes your
config; do them in either order, then `up`.)

That's it — scan the QR and your phone is connected, all over Tor.

## All commands

| Command | What it does |
|---|---|
| `./pp-box init` | Create `.env` — admin user/box name, password, and a fresh `PP_SECRETS_KEY`. |
| `./pp-box build` | Build the `pureprivacy-box:dev` image (stages the binary + sidecars). |
| `./pp-box up` | Start the box. First run mints the onion + creates the admin account. |
| `./pp-box qr` | Print the phone-connect QR (and the `@user:onion` payload). |
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
.\pp-box.ps1 init ; .\pp-box.ps1 up ; .\pp-box.ps1 qr
```

(Or build it directly inside WSL2 and run from there.) A self-contained image you can
`docker build` / `docker pull` on any OS is the Stage-2 follow-up.

## Back up your box — it's the whole identity

An `.onion` address is derived from a secret key that exists **only** in the
`pureprivacy-data` volume. If that volume is deleted, the address can never come back and
your phone is orphaned on a dead box. So keep a backup:

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

The CLI just wraps these. `docker compose` reads the `.env` that `init` wrote:

```bash
docker compose up -d           # start        (needs PP_PASS + PP_SECRETS_KEY in .env)
docker compose logs -f box     # watch it mint its onion + print the QR
docker compose down            # stop
```

or one plain `docker run` (identity in the `pureprivacy-data` volume):

```bash
docker volume create pureprivacy-data
docker run -d --name pureprivacy-box --restart unless-stopped -v pureprivacy-data:/data \
  -e PP_USER=jaime -e PP_PASS='a-strong-password' -e PP_BOX=mybox \
  -e PP_SECRETS_KEY="$(openssl rand -base64 32)" \
  pureprivacy-box:dev
docker logs -f pureprivacy-box
```

## Notes & known limits (Stage 1)

- Build is **Stage 1**: it reuses the prebuilt Tauri binary + sidecars from your machine
  (`build.sh`) — fast to iterate. A shipping image would compile both in a multi-stage
  build.
- **Image is ~1.2 GB / amd64 only.** The binary is the Tauri GUI (links webkit2gtk), run
  headless under Xvfb, so the image ships webkit/gtk/xvfb just to satisfy it. **Stage 2** —
  a headless box runner with no Tauri — drops all of that and unlocks multi-arch (arm64 for
  a Pi / Apple Silicon), a leaner image, and a native setup CLI.
