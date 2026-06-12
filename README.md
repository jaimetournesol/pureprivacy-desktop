# PurePrivacy Desktop

Phase-1 **native shell** for PurePrivacy: a [Tauri 2](https://tauri.app) + Svelte app
that supervises two local sidecar processes —

- **tuwunel** — an embedded Matrix homeserver (the user's personal server)
- **C-tor** — a Tor client providing the onion transport between instances

The shell owns the sidecars' lifecycle (spawn, health-check, restart, shutdown) and
renders the onboarding + dashboard UI on top.

## Dev quickstart

```bash
source ~/.nvm/nvm.sh            # Node v24 via nvm
pnpm install
./scripts/fetch-sidecars.sh     # fetch tuwunel + tor into the runtime bin dir
pnpm tauri dev
```

`fetch-sidecars.sh` extracts `tuwunel` from the upstream OCI image (requires Docker)
and copies the system `tor` (or apt-installs it). Binaries land in
`$HOME/.local/share/ai.tournesol.pureprivacy/bin` by default — override with
`PUREPRIVACY_BIN_DIR`. Run with `--uninstall` to remove them.

## Demo mode

The app runs **without sidecars**. If the supervisor can't find (or fails to start)
`tuwunel`/`tor`, it falls back to demo mode: the full UI is browsable with stubbed
data, and an **amber banner** is shown so it's unmistakable that nothing real is
running. This keeps UI work unblocked and makes first-clone DX painless.

## Runtime layout

Everything lives under the platform app-data dir (`ai.tournesol.pureprivacy`):

```
<app_data_dir>/
├── config/        # generated tuwunel + tor configuration
├── tor/           # tor data dir (onion keys, state)
├── tuwunel-db/    # homeserver database
└── bin/           # sidecar binaries (dev: populated by fetch-sidecars.sh)
```

## Ports

| Service             | Address           | Why                                        |
| ------------------- | ----------------- | ------------------------------------------ |
| tuwunel homeserver  | `127.0.0.1:8118`  | loopback-only; never exposed off-box       |
| tor SOCKS proxy     | `127.0.0.1:9150`  | Tor Browser's port, avoids system tor 9050 |

## Docs

- UX design: [`../docs/redesign/2026-06-ux-design.md`](../docs/redesign/2026-06-ux-design.md)
- Plan: [`../docs/redesign/2026-06-portability-plan.md`](../docs/redesign/2026-06-portability-plan.md)
  (design: [`2026-06-portability-design.md`](../docs/redesign/2026-06-portability-design.md))

## Status

| Area                                    | Status     |
| --------------------------------------- | ---------- |
| Onboarding flow (D1–D5)                 | ✅ done    |
| Dashboard (D6)                          | ✅ done    |
| Sidecar supervisor + demo mode          | ✅ done    |
| People / Boxes / Agent / Settings pages | ❌ not yet |
| `externalBin` packaging + signing       | ❌ not yet |
| Auto-updater                            | ❌ not yet |
