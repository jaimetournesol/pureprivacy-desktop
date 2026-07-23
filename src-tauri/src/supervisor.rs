//! Sidecar supervision via `tokio::process::Command` (deliberately NOT
//! Tauri's externalBin: binaries are resolved at runtime from
//! `<app_data_dir>/bin/{tuwunel,tor}` or a `$PUREPRIVACY_BIN_DIR` override).
//!
//! Cancellation model — the "generation" trick:
//! Every start/stop bumps an atomic generation counter. Each supervision
//! loop captures the generation it was born under and checks it on every
//! tick; the moment it goes stale (a stop or a newer start happened) the
//! loop kills its child and exits. This means stop/start never has to track
//! down task handles — old loops simply notice they are obsolete and die.
//!
//! Kill paths, in order of preference:
//! 1. `shutdown()` sends a best-effort SIGTERM by pid (graceful: tor flushes
//!    its state, tuwunel closes RocksDB cleanly).
//! 2. Each loop calls `start_kill()` on its own child when it sees a stale
//!    generation (covers the non-unix / pid-reuse edge).
//! 3. `kill_on_drop(true)` is the backstop: if the tokio runtime is torn
//!    down with children still alive (app exit), they get SIGKILLed.
//!
//! DEMO MODE: if the binaries are missing we simulate the exact same
//! lifecycle with timers so the UI is fully drivable without binaries.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use rand::Rng; // [QW-rust b] gen_range for jittered respawn backoff
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time::{sleep, Instant};

use crate::config::{
    self, off, FEDAUTH_PORT, FEDPROXY_PORT, HOMESERVER_PORT, LIVEKIT_WS_PORT,
    LIVEKIT_WSS_ONION_PORT, LKJWT_PORT, SOCKS_PORT, TURN_PORT,
};
use crate::state::{self, Phase, ServiceState, SetupStage};

const DEMO_ONION: &str = "demo7gk2x4adqlmnop.onion";
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_millis(500);

#[derive(Default)]
pub struct Supervisor {
    /// Bumped on every start/stop; stale loops self-terminate.
    generation: AtomicU64,
    /// Live child pids, for the graceful SIGTERM path on shutdown.
    pids: Mutex<HashMap<&'static str, u32>>,
}

impl Supervisor {
    pub fn current_gen(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn bump(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn record_pid(&self, name: &'static str, pid: u32) {
        self.pids.lock().unwrap().insert(name, pid);
    }

    fn clear_pid(&self, name: &'static str) {
        self.pids.lock().unwrap().remove(name);
    }

    /// Invalidate all supervision loops and SIGTERM live children.
    /// Safe to call multiple times; also called on RunEvent::ExitRequested.
    pub fn shutdown(&self) {
        self.bump();
        let pids: Vec<u32> = self.pids.lock().unwrap().drain().map(|(_, pid)| pid).collect();
        #[cfg(unix)]
        for pid in pids {
            // Graceful first; the per-loop start_kill + kill_on_drop are the
            // harder backstops if the process ignores SIGTERM.
            let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
        }
        #[cfg(not(unix))]
        let _ = pids;
    }
}

fn is_stale(app: &AppHandle, gen: u64) -> bool {
    app.state::<Supervisor>().current_gen() != gen
}

/// [QW-rust b] Jittered backoff sleep: wait `backoff/2 + rand(0..backoff/2)`.
/// Decorrelating the respawn delay stops every sidecar that crashed at the same
/// instant (e.g. a Tor outage took them all down) from retrying in lockstep —
/// the thundering-herd that would re-hammer a still-flaky network simultaneously.
/// On average it's the same as `backoff` but spread across a window.
async fn jittered_sleep(backoff: Duration) {
    let half = backoff / 2;
    let extra = if half.is_zero() {
        Duration::ZERO
    } else {
        Duration::from_nanos(rand::thread_rng().gen_range(0..=half.as_nanos() as u64))
    };
    sleep(half + extra).await;
}

// ---------------------------------------------------------------------------
// Binary resolution
// ---------------------------------------------------------------------------

pub fn bin_dir(app: &AppHandle) -> Result<PathBuf, String> {
    if let Ok(dir) = std::env::var("PUREPRIVACY_BIN_DIR") {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    Ok(state::app_data_dir(app)?.join("bin"))
}

fn binaries_present(app: &AppHandle) -> bool {
    match bin_dir(app) {
        Ok(dir) => dir.join("tor").is_file() && dir.join("tuwunel").is_file(),
        Err(_) => false,
    }
}

/// Voice (coturn) is an OPTIONAL sidecar: a box without `turnserver` still
/// runs, just without 1:1 calls. Probed separately from the required binaries.
fn turn_present(app: &AppHandle) -> bool {
    bin_dir(app).map(|d| d.join("turnserver").is_file()).unwrap_or(false)
}

/// The Caddy fed-proxy is what enforces the paired-peer federation allowlist
/// (Option B). Optional like voice: without it, chat works but inbound
/// federation is off (no TLS terminator / allowlist).
fn caddy_present(app: &AppHandle) -> bool {
    bin_dir(app).map(|d| d.join("caddy").is_file()).unwrap_or(false)
}

/// The LiveKit SFU (group calls / Element Call). Optional sidecar.
fn livekit_present(app: &AppHandle) -> bool {
    bin_dir(app).map(|d| d.join("livekit-server").is_file()).unwrap_or(false)
}

/// lk-jwt-service: validates a Matrix OpenID token and mints a LiveKit JWT.
/// Optional sidecar — paired with livekit-server.
fn lkjwt_present(app: &AppHandle) -> bool {
    bin_dir(app).map(|d| d.join("lk-jwt-service").is_file()).unwrap_or(false)
}

/// Group-voice (Element Call) is enabled only when BOTH the SFU and the token
/// service are present. This is the `voice` flag threaded into torrc / Caddyfile
/// / tuwunel.toml: it gates the wss SFU site, the group-call onion port map, and
/// the well_known livekit_url advertisement so a box without the binaries never
/// promises a service it can't run.
fn group_voice_present(app: &AppHandle) -> bool {
    livekit_present(app) && lkjwt_present(app)
}

// ---------------------------------------------------------------------------
// Lifecycle entry points (called from commands + tray)
// ---------------------------------------------------------------------------

/// Start (or restart) the box. Picks real or demo mode by binary presence.
/// `admin_password` is `Some` only on first-run setup (it's used once to create
/// the admin account, then dropped — never persisted); plain start/restart
/// passes `None`.
pub fn start_lifecycle(app: &AppHandle, admin_password: Option<String>) {
    let gen = app.state::<Supervisor>().bump();
    let demo = !binaries_present(app);
    state::update(app, |inner| {
        inner.phase = Phase::SettingUp;
        inner.setup_stage = Some(SetupStage::StartingServices);
        inner.demo_mode = demo;
        inner.homeserver = ServiceState::Starting;
        inner.tor = ServiceState::Starting;
    });
    let handle = app.clone();
    if demo {
        tauri::async_runtime::spawn(async move { run_demo(handle, gen).await });
    } else {
        tauri::async_runtime::spawn(async move {
            if let Err(err) = run_real(handle.clone(), gen, admin_password).await {
                if !is_stale(&handle, gen) {
                    eprintln!("[pureprivacy] setup failed: {err}");
                    state::update(&handle, |inner| {
                        inner.phase = Phase::Error;
                        inner.setup_stage = None;
                    });
                }
            }
        });
    }
}

/// Stop the box: invalidate loops, kill children, mark everything stopped.
pub fn stop_lifecycle(app: &AppHandle) {
    app.state::<Supervisor>().shutdown();
    state::update(app, |inner| {
        inner.phase = Phase::Stopped;
        inner.setup_stage = None;
        inner.homeserver = ServiceState::Stopped;
        inner.tor = ServiceState::Stopped;
        inner.voice = ServiceState::Stopped;
    });
}

/// Re-render the fed-proxy allowlist from the current pairings and hot-reload
/// Caddy — no stack restart (so tor circuits + the homeserver stay up). If the
/// box isn't running, `caddy reload` simply fails harmlessly and the new
/// Caddyfile applies on the next start. Called after every pairing change.
pub fn reload_fedproxy(app: &AppHandle) {
    let Ok(paths) = config::paths(app) else { return };
    if let Some(onion) = state::read(app, |i| i.onion.clone()) {
        let _ = config::ensure_fed_cert(app, &onion);
    }
    let peers = crate::pairing::onions(&paths.data_root);
    // Pass the current group_voice_present so the :7444 wss SFU site survives a
    // pair-change reload (the Caddyfile is re-rendered wholesale here).
    let voice = group_voice_present(app);
    if config::render_caddyfile(app, &peers, voice).is_err() || !caddy_present(app) {
        return;
    }
    if let Ok(bins) = bin_dir(app) {
        // [QW-rust a] `caddy reload` talks to the running instance's admin API
        // and swaps config atomically. A FAILED reload silently keeps the stale
        // allowlist (a just-paired peer stays blocked, or a just-revoked peer
        // stays allowed) — so check the exit status and retry ONCE before giving
        // up. If caddy isn't running (box stopped), both attempts fail harmlessly
        // and the new Caddyfile applies on the next start (it's re-rendered from
        // pairings on boot). Worth noting: a reload while caddy is down is the
        // expected no-op, not an error to act on.
        let run_reload = || {
            std::process::Command::new(bins.join("caddy"))
                .args([
                    "reload",
                    "--config",
                    &paths.caddyfile.to_string_lossy(),
                    "--adapter",
                    "caddyfile",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
        };
        let ok = matches!(run_reload(), Ok(s) if s.success());
        if !ok {
            // Retry once — a transient admin-API hiccup shouldn't strand a stale
            // allowlist. Second failure is logged (likely caddy-down, benign).
            let ok2 = matches!(run_reload(), Ok(s) if s.success());
            if !ok2 {
                eprintln!(
                    "[pureprivacy] caddy reload failed (twice) — allowlist change will apply on next box start"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Demo mode — same observable lifecycle, no processes
// ---------------------------------------------------------------------------

async fn run_demo(app: AppHandle, gen: u64) {
    sleep(Duration::from_secs(2)).await;
    if is_stale(&app, gen) {
        return;
    }
    state::update(&app, |inner| {
        inner.setup_stage = Some(SetupStage::MintingAddress);
        inner.tor = ServiceState::Healthy;
    });

    sleep(Duration::from_secs(6)).await;
    if is_stale(&app, gen) {
        return;
    }
    state::update(&app, |inner| {
        inner.onion = Some(DEMO_ONION.to_string());
        inner.setup_stage = Some(SetupStage::Ready);
        inner.phase = Phase::Running;
        inner.homeserver = ServiceState::Healthy;
        inner.tor = ServiceState::Healthy;
    });
    let _ = state::persist(&app);
}

// ---------------------------------------------------------------------------
// Real mode — tor first, mint onion, then tuwunel
// ---------------------------------------------------------------------------

/// [H5] Tear down a half-brought-up box on a genuine (non-stale) run_real
/// failure and surface the Error phase HERE. We must set phase=Error before
/// calling shutdown(): shutdown() bumps the generation, after which the caller's
/// `is_stale` guard would skip its own phase=Error write — so the UI would be
/// stuck "setting up" forever. Setting it here keeps the error visible AND stops
/// the already-spawned tor/homeserver/voice/fedproxy/livekit/lkjwt loops from
/// orphaning (each loop notices the bumped generation and kills its child).
/// No-op state write if we're already stale (a concurrent stop/restart owns it).
fn fail_real(app: &AppHandle, gen: u64, err: String) -> String {
    if !is_stale(app, gen) {
        state::update(app, |inner| {
            inner.phase = Phase::Error;
            inner.setup_stage = None;
        });
        app.state::<Supervisor>().shutdown();
    }
    err
}

/// Reap a stale `lk-jwt-service` still bound to `port` before spawning a fresh one.
/// lk-jwt is configured entirely by env (no data-dir in its cmdline), so a SIGKILL / OOM /
/// crash of a previous box generation orphans it holding LIVEKIT_JWT_PORT with the OLD
/// box's LiveKit keys — the new lk-jwt then can't bind, crash-loops, and Element Call
/// fails with "livekit failed" (a JWT signed by mismatched keys). We match on BOTH the
/// binary name AND the exact `LIVEKIT_JWT_PORT={port}` in the process env, so only our own
/// orphan for THIS box's port is ever killed — a co-hosted box's lk-jwt (different port) or
/// any unrelated process on the port is never touched. Linux-only (/proc); no-op elsewhere.
#[cfg(target_os = "linux")]
fn reap_stale_lkjwt(port: u16) {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    let needle = format!("LIVEKIT_JWT_PORT={port}");
    for e in entries.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let cmd = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if !cmd.contains("lk-jwt-service") {
            continue;
        }
        // /proc/<pid>/environ is NUL-separated KEY=VALUE; an exact-entry match is precise.
        let env = std::fs::read_to_string(format!("/proc/{pid}/environ")).unwrap_or_default();
        if env.split('\0').any(|kv| kv == needle) {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
            eprintln!("[pp] reaped stale lk-jwt orphan pid {pid} on :{port}");
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn reap_stale_lkjwt(_port: u16) {}

/// Best-effort: open/refresh the Tor circuit to a peer's box so the FIRST federated
/// invite/event lands quickly instead of paying full cold-circuit cost — which made
/// first-time pairing take minutes or stall in testing. We GET the peer's onion
/// `/_matrix/federation/v1/version` (an `@open` path that bypasses the allowlist) over
/// the box's Tor SOCKS proxy. Fire-and-forget: a cold/offline peer just fails here and
/// the invite retry keeps trying — this only makes the happy path fast.
async fn warm_peer_circuit(peer_onion: &str, log: bool) {
    let socks = format!("socks5h://127.0.0.1:{}", SOCKS_PORT + off());
    let Ok(proxy) = reqwest::Proxy::all(&socks) else {
        return;
    };
    let Ok(client) = reqwest::Client::builder()
        .proxy(proxy)
        .danger_accept_invalid_certs(true) // the onion's federation cert is self-signed
        .timeout(Duration::from_secs(60)) // Tor onion circuits can be slow to build
        .build()
    else {
        return;
    };
    // Onion virtual port 8448 = the peer's Caddy fed-proxy (torrc HiddenServicePort 8448),
    // regardless of the peer's local PORT_OFFSET.
    let url = format!("https://{peer_onion}:8448/_matrix/federation/v1/version");
    if client.get(&url).send().await.is_ok() && log {
        eprintln!("[pp] warmed Tor circuit to {peer_onion}");
    }
}

async fn run_real(app: AppHandle, gen: u64, admin_password: Option<String>) -> Result<(), String> {
    let paths = config::ensure_dirs(&app)?;
    // Group calls (Element Call / LiveKit) are enabled only when both the SFU
    // and the lk-jwt binaries are present. This flag gates the wss SFU site, the
    // group-call onion port map, and the well_known livekit_url advertisement.
    let voice = group_voice_present(&app);
    config::render_torrc(&app, voice)?;
    // Placeholder so the file always exists; re-rendered with the real onion
    // below, before tuwunel ever starts. No turn / registration block until we
    // have the onion + secrets.
    let known_onion = state::read(&app, |inner| inner.onion.clone());
    let (turn_secret, join_token, username, livekit_api_key, livekit_api_secret) =
        state::read(&app, |inner| {
            (
                inner.turn_secret.clone(),
                inner.join_token.clone(),
                inner.username.clone(),
                inner.livekit_api_key.clone(),
                inner.livekit_api_secret.clone(),
            )
        });
    config::render_tuwunel(&app, known_onion.as_deref().unwrap_or("placeholder.onion"), "", "", voice)?;

    let bins = bin_dir(&app)?;
    spawn_supervised(
        app.clone(),
        gen,
        "tor",
        bins.join("tor"),
        vec!["-f".into(), paths.torrc.to_string_lossy().into_owned()],
        vec![],
        Readiness::File(paths.hostname_file.clone()),
    );

    state::update(&app, |inner| inner.setup_stage = Some(SetupStage::MintingAddress));

    // Wait for tor to mint (or re-load) the hidden-service hostname. [H5] tor's
    // supervision loop is already running by now, so a timeout here would orphan
    // it holding the SOCKS/onion ports — tear it down on error before returning.
    let onion = match wait_for_hostname(&app, gen, &paths.hostname_file, Duration::from_secs(180))
        .await
    {
        Ok(o) => o,
        Err(e) => return Err(fail_real(&app, gen, e)),
    };
    state::update(&app, |inner| inner.onion = Some(onion.clone()));
    let _ = state::persist(&app);

    // Now we know the server_name; render for real (incl. the turn_uris block
    // when we have a secret, the well_known livekit_url when voice is enabled,
    // and token-gated registration) and start the homeserver.
    config::render_tuwunel(&app, &onion, &turn_secret, &join_token, voice)?;
    spawn_supervised(
        app.clone(),
        gen,
        "homeserver",
        bins.join("tuwunel"),
        vec!["-c".into(), paths.tuwunel_toml.to_string_lossy().into_owned()],
        vec![],
        Readiness::Http(HOMESERVER_PORT + off()),
    );

    // Optional 1:1-voice sidecar. Render its config (needs the onion + secret)
    // and supervise it like the others — but only if the binary is present and
    // a secret exists. A box without voice just leaves this Stopped; it never
    // blocks startup.
    if turn_present(&app) && !turn_secret.is_empty() {
        if let Err(e) = config::render_turnserver(&app, &onion, &turn_secret) {
            eprintln!("[pureprivacy] voice config skipped: {e}");
        } else {
            spawn_supervised(
                app.clone(),
                gen,
                "voice",
                bins.join("turnserver"),
                vec!["-c".into(), paths.turnserver_conf.to_string_lossy().into_owned()],
                vec![],
                Readiness::Tcp(TURN_PORT + off()),
            );
        }
    }

    // Federation fed-proxy (Caddy): TLS-terminate inbound federation and enforce
    // the paired-peer allowlist rendered from pairings.json (Option B). Optional
    // like voice — a missing caddy binary just means no inbound federation; chat
    // still works. The Caddyfile is re-rendered from pairings on every boot, so
    // a pair change followed by a restart picks up the new allowlist.
    if caddy_present(&app) {
        match (|| -> Result<(), String> {
            config::ensure_fed_cert(&app, &onion)?;
            let peers = crate::pairing::onions(&paths.data_root);
            // voice=group_voice_present so the :7444 wss SFU site is rendered
            // from the start (not only after a pair-change reload).
            config::render_caddyfile(&app, &peers, voice)
        })() {
            Ok(()) => spawn_supervised(
                app.clone(),
                gen,
                "fedproxy",
                bins.join("caddy"),
                vec![
                    "run".into(),
                    "--config".into(),
                    paths.caddyfile.to_string_lossy().into_owned(),
                    "--adapter".into(),
                    "caddyfile".into(),
                ],
                vec![],
                Readiness::Tcp(FEDPROXY_PORT + off()),
            ),
            Err(e) => eprintln!("[pureprivacy] federation proxy skipped: {e}"),
        }
    }

    // Optional group-call sidecars (Element Call / LiveKit). Both must be present
    // (group_voice_present) and we need the shared api_key/secret. A box without
    // them just runs without group calls — it never blocks startup. Spawned after
    // the onion is known + tuwunel rendered + the caddy fed-proxy (which includes
    // the wss SFU site) is launched, so lk-jwt's handed-out wss://<onion>:7443
    // points at a site that exists.
    if voice {
        if livekit_api_key.is_empty() || livekit_api_secret.is_empty() {
            eprintln!("[pureprivacy] group voice skipped: missing livekit api key/secret");
        } else if let Err(e) = config::render_livekit_yaml(
            &app,
            &livekit_api_key,
            &livekit_api_secret,
            &onion,
            &turn_secret,
        ) {
            eprintln!("[pureprivacy] group voice skipped: {e}");
        } else {
            // LiveKit SFU: TCP-only signaling + media on loopback.
            spawn_supervised(
                app.clone(),
                gen,
                "livekit",
                bins.join("livekit-server"),
                vec!["--config".into(), paths.livekit_yaml.to_string_lossy().into_owned()],
                vec![],
                Readiness::Tcp(LIVEKIT_WS_PORT + off()),
            );
            // lk-jwt-service: configured entirely by env (no args). It validates
            // a caller's Matrix OpenID token and mints a LiveKit JWT.
            //
            // v0.1 used an /etc/hosts onion->fed-proxy override + CA trust;
            // modernized to ALL_PROXY=socks5h. Revert path:
            // docs/redesign/2026-06-voice-workarounds-vault.md
            let lkjwt_envs: Vec<(String, String)> = vec![
                ("LIVEKIT_KEY".into(), livekit_api_key.clone()),
                ("LIVEKIT_SECRET".into(), livekit_api_secret.clone()),
                // The env var is LIVEKIT_JWT_PORT (NOT LK_JWT_PORT) — lk-jwt
                // 0.2.0 ignores the latter and falls back to 8080, which then
                // mismatches the torrc onion map. (Caught by the live connect
                // test, 2026-06-13.)
                ("LIVEKIT_JWT_PORT".into(), (LKJWT_PORT + off()).to_string()),
                // The wss SFU URL handed to clients (KEEP — Element Call refuses
                // ws://). Caddy terminates TLS on the onion's 7443 and proxies
                // the WS upgrade to LiveKit.
                ("LIVEKIT_URL".into(), format!("wss://{onion}:{LIVEKIT_WSS_ONION_PORT}")),
                // Route lk-jwt's remote-user validation over Tor. lk-jwt dials
                // the peer as matrix://<onion>, but fclient rewrites that to
                // https:// before the request — so Go's http.ProxyFromEnvironment
                // honors HTTPS_PROXY (it does NOT read ALL_PROXY, and would skip
                // a still-matrix:// scheme). socks5h:// makes Tor resolve the
                // .onion. PROVEN over Tor by the live two-box connect test
                // (2026-06-13: "Got user info for @bob:<onion>" → JWT minted).
                // HTTP_PROXY too for the pre-flight well-known GET.
                ("HTTPS_PROXY".into(), format!("socks5h://127.0.0.1:{}", SOCKS_PORT + off())),
                ("HTTP_PROXY".into(), format!("socks5h://127.0.0.1:{}", SOCKS_PORT + off())),
                // Accept the self-signed onion certs the federation path uses.
                (
                    "LIVEKIT_INSECURE_SKIP_VERIFY_TLS".into(),
                    "YES_I_KNOW_WHAT_I_AM_DOING".into(),
                ),
            ];
            // Reap a stale lk-jwt orphaned by a prior generation's hard kill/crash before
            // we spawn the fresh one — otherwise the orphan keeps this port bound with the
            // OLD box's LiveKit keys, the new lk-jwt crash-loops on EADDRINUSE, and calls
            // fail with "livekit failed" (a JWT signed by mismatched keys). See below.
            reap_stale_lkjwt(LKJWT_PORT + off());
            spawn_supervised(
                app.clone(),
                gen,
                "lkjwt",
                bins.join("lk-jwt-service"),
                vec![],
                lkjwt_envs,
                Readiness::Tcp(LKJWT_PORT + off()),
            );
        }
    }

    // [H5] On a later-step failure (the wait_for_http timeout here, or the
    // create_admin error below), the already-spawned tor/homeserver/voice/
    // fedproxy/livekit/lkjwt loops keep running and holding their ports. fail_real
    // tears them down (and surfaces phase=Error) so a failed bring-up never
    // orphans loops.
    if let Err(e) =
        wait_for_http(&app, gen, HOMESERVER_PORT + off(), Duration::from_secs(120)).await
    {
        return Err(fail_real(&app, gen, e));
    }
    if is_stale(&app, gen) {
        return Ok(());
    }

    // First run: create the admin account now that the homeserver answers.
    // tuwunel makes the first registered user an admin; the registration token
    // keeps the box from being open-reg. Without this the box would come up
    // "running" with no account to log into.
    if let Some(password) = admin_password {
        if !username.is_empty() && !join_token.is_empty() {
            match crate::account::create_admin(&username, &password, &join_token).await {
                Ok(()) => eprintln!("[pureprivacy] admin account @{username} ready"),
                Err(e) => {
                    // [H5] Same teardown as the wait_for_http path: a create_admin
                    // failure must not leave the sidecar loops orphaned holding
                    // ports. fail_real is a no-op when we're already stale.
                    if !is_stale(&app, gen) {
                        return Err(fail_real(&app, gen, e));
                    }
                }
            }
        }
    }

    if is_stale(&app, gen) {
        return Ok(());
    }
    state::update(&app, |inner| {
        inner.setup_stage = Some(SetupStage::Ready);
        inner.phase = Phase::Running;
    });

    // QR-driven federation pairing: watch the owner's `pairings` account data
    // (their phone writes a peer's onion there the moment they scan that contact's
    // QR) and fold any new peer onions into the fed-proxy allowlist. This is what
    // makes a phone-to-phone QR exchange pair the two boxes — no separate desktop
    // pairing step. Reads the local client API only; the onion came from a scanned
    // code, never from federation, so there's no allowlist bootstrap deadlock.
    let ph = app.clone();
    tauri::async_runtime::spawn(async move { run_pairing_sync(ph, gen).await });

    // Tier-2 invisible federation keepalive: tuwunel's federation sender is
    // event-driven — a `Failed` destination only retries when a NEW outbound
    // event is queued. This sibling task periodically sends an INVISIBLE
    // `m.dummy` to-device message to each paired peer, which federates a
    // to-device EDU and wakes the sender so any stuck backlog flushes — with no
    // presence/read-receipt leak. See run_federation_keepalive below for detail.
    let fh = app.clone();
    tauri::async_runtime::spawn(async move { run_federation_keepalive(fh, gen).await });

    // Tier-3 TRANSPORT federation keepalive: Tier-1 (backoff cap) and Tier-2 (m.dummy
    // nudge) both ride tuwunel's own federation path, so if the Tor CIRCUIT to a peer
    // went cold they can't recover it. A call does exactly that — its media saturates
    // the box's shared Tor and starves the federation circuit, which stays cold after
    // the call (observed: a call killed jaime<->arnaud messaging both ways until the
    // circuit was manually re-warmed). This task keeps each paired peer's Tor circuit
    // warm directly, so messaging heals within a cadence of a call ending.
    let cw = app.clone();
    tauri::async_runtime::spawn(async move { run_federation_circuit_warm(cw, gen).await });

    // Federation allowlist validator (review item W3-T1): Caddy forward_auths each
    // authenticated federation request to this loopback endpoint, which parses the
    // X-Matrix Authorization origin and matches it against the live pairings
    // allowlist — replacing the old substring-bypassable header_regexp matcher.
    let gh = app.clone();
    tauri::async_runtime::spawn(async move { run_fedauth(gh, gen).await });

    // Box config from the phone (appliance-UX feature B): publish a read-only status
    // blob the phone's PP Config app reads, and execute the tightly-guarded commands
    // the phone writes — all over the SAME authenticated account-data channel as
    // pairing (no new network surface). See run_box_config for the security rules.
    let bh = app.clone();
    tauri::async_runtime::spawn(async move { run_box_config(bh, gen).await });
    Ok(())
}

/// Account-data event type the phone writes scanned-peer onions into.
const PAIR_ACCOUNT_DATA_TYPE: &str = "ai.tournesol.pureprivacy.pairings";
/// Account-data the box PUBLISHES (read-only for the phone) — PP Config's live view.
const BOXSTATUS_ACCOUNT_DATA_TYPE: &str = "ai.tournesol.pureprivacy.boxstatus";
/// Account-data the phone WRITES a command into; the box reads + executes it.
const COMMAND_ACCOUNT_DATA_TYPE: &str = "ai.tournesol.pureprivacy.command";
/// Account-data the box WRITES a command's outcome into; the phone reads it.
const COMMAND_RESULT_ACCOUNT_DATA_TYPE: &str = "ai.tournesol.pureprivacy.command_result";

/// [QW-rust c] A reqwest client with a request timeout, for the box's local
/// homeserver calls (login / account-data / keepalive). Without it, a hung
/// connection over flaky Tor could block a poll loop indefinitely (reqwest has
/// NO default timeout). All these calls hit loopback (the local tuwunel), so 15s
/// is generous headroom over a healthy local response while still bounding a
/// wedge. Falls back to the default client if the builder ever fails (it won't
/// for a plain timeout) so a loop never fails to start over this.
fn box_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Authenticate to the local homeserver as the box admin; returns an access token.
async fn pair_login(
    client: &reqwest::Client,
    base: &str,
    user: &str,
    pass: &str,
) -> Option<String> {
    let r = client
        .post(format!("{base}/_matrix/client/v3/login"))
        .json(&serde_json::json!({
            "type": "m.login.password",
            "identifier": { "type": "m.id.user", "user": user },
            "password": pass,
        }))
        .send()
        .await
        .ok()?;
    let v: serde_json::Value = r.json().await.ok()?;
    v.get("access_token").and_then(|t| t.as_str()).map(String::from)
}

/// Fetch the owner's recorded pairing onions. `Err(true)` = token rejected
/// (re-login), `Err(false)` = transient (retry), `Ok(vec)` = current list
/// (empty if the account data doesn't exist yet).
async fn pair_fetch_onions(
    client: &reqwest::Client,
    base: &str,
    user_id: &str,
    token: &str,
) -> Result<Vec<String>, bool> {
    // Percent-encode the user id for the path (@ and : are reserved).
    let enc = user_id.replace('@', "%40").replace(':', "%3A");
    let url = format!("{base}/_matrix/client/v3/user/{enc}/account_data/{PAIR_ACCOUNT_DATA_TYPE}");
    let r = client.get(url).bearer_auth(token).send().await.map_err(|_| false)?;
    if r.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(true);
    }
    if r.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new()); // owner hasn't scanned anyone yet
    }
    if !r.status().is_success() {
        return Err(false);
    }
    let v: serde_json::Value = r.json().await.map_err(|_| false)?;
    // SAFETY (guard #1): under the reconcile, an empty result WIPES the
    // allowlist — so the only thing that may yield Ok(empty) is a *genuine*
    // empty list (or the 404 above). The `onions` key being absent entirely
    // means the value is some other shape we don't understand → treat as a bad
    // read (Err) rather than collapsing to empty. If the key IS present it must
    // be an array of strings; anything else (object, string, number, an array
    // with a non-string element) is hostile/corrupt and must NOT clear live
    // federation — return Err(false) so the caller retries instead of wiping.
    match v.get("onions") {
        None => Err(false),
        Some(serde_json::Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => return Err(false), // non-string element → bad read
                }
            }
            Ok(out)
        }
        Some(_) => Err(false), // present but not an array → bad read
    }
}

/// Remove a single onion from the owner's `pairings` account data (the
/// authoritative source the reconcile syncs against). Called by the desktop
/// `pair_remove` command BEFORE it touches `pairings.json`, so the add-and-
/// remove reconcile can't re-add the peer within a tick (guard #4).
///
/// Idempotent + re-runnable: it logs in fresh, re-reads the current list, drops
/// `target`, and PUTs the remainder back, retrying ~5× over flaky Tor. If creds
/// are missing it's a no-op (the in-memory/local removal still proceeds).
pub(crate) async fn pair_remove_onion_from_account_data(
    app: &AppHandle,
    target: &str,
) -> Result<(), String> {
    let (username, onion, password) = state::read(app, |i| {
        (
            i.username.clone(),
            i.onion.clone().unwrap_or_default(),
            i.admin_password.clone(),
        )
    });
    if username.is_empty() || onion.is_empty() || password.is_empty() {
        // No way to authenticate (e.g. a box from before passwords were
        // stored). The local removal still cuts the allowlist; the reconcile
        // can't re-add because there's no account-data path here at all.
        return Ok(());
    }
    let base = format!("http://127.0.0.1:{}", HOMESERVER_PORT + off());
    let user_id = format!("@{username}:{onion}");
    // Same percent-encode as pair_fetch_onions (@ and : are reserved in the path).
    let enc = user_id.replace('@', "%40").replace(':', "%3A");
    let url = format!("{base}/_matrix/client/v3/user/{enc}/account_data/{PAIR_ACCOUNT_DATA_TYPE}");
    let client = box_http_client(); // [QW-rust c] request timeout (no reqwest default)

    for _ in 0..5 {
        let Some(token) = pair_login(&client, &base, &username, &password).await else {
            sleep(Duration::from_secs(2)).await;
            continue;
        };
        let current = match pair_fetch_onions(&client, &base, &user_id, &token).await {
            Ok(c) => c,
            Err(_) => {
                // Token rejected or transient — back off and retry the whole
                // login+read so we never PUT against a bad read.
                sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let kept: Vec<String> = current.into_iter().filter(|o| o != target).collect();
        let put = client
            .put(&url)
            .bearer_auth(&token)
            .json(&serde_json::json!({ "onions": kept }))
            .send()
            .await;
        match put {
            Ok(r) if r.status().is_success() => {
                eprintln!("[pureprivacy] pairing remove: dropped {target} from account-data");
                return Ok(());
            }
            _ => {
                sleep(Duration::from_secs(2)).await;
            }
        }
    }
    Err(format!("couldn't drop {target} from pairing account-data after retries"))
}

/// Add a single onion to the owner's `pairings` account data (the authoritative
/// source the reconcile syncs against). Called by the desktop `pair_accept`
/// command BEFORE it touches `pairings.json`, so the add-and-remove reconcile
/// can't REVOKE the just-added peer within a tick (finding C5: account-data is
/// the single source of truth — run_pairing_sync removes known−desired each
/// tick, so a peer that's only in pairings.json gets revoked within ~3s, making
/// the desktop "Connect a box" button non-functional).
///
/// Idempotent + re-runnable: it logs in fresh, re-reads the current list, adds
/// `target` if absent, and PUTs the union back, retrying up to `attempts` times
/// over flaky Tor. If creds are missing it's an Err (the caller still does the
/// local add, but the reconcile would then revoke it — so the caller logs the
/// gap). Mirrors pair_remove_onion_from_account_data exactly, inverted.
///
/// `attempts` lets the caller bound the interactive blast radius: the desktop
/// `pair_accept` command (which blocks the "Connect a box" button on its await)
/// passes a small budget so a fully-failing-Tor box can't hang the UI for
/// minutes — on a healthy box this completes in well under a second, and if it
/// does fail the reconcile (run_pairing_sync) union-adds from account-data on its
/// next good tick anyway, so a low budget never loses a pairing that can succeed.
pub(crate) async fn pair_add_onion_to_account_data(
    app: &AppHandle,
    target: &str,
    attempts: u32,
) -> Result<(), String> {
    let (username, onion, password) = state::read(app, |i| {
        (
            i.username.clone(),
            i.onion.clone().unwrap_or_default(),
            i.admin_password.clone(),
        )
    });
    if username.is_empty() || onion.is_empty() || password.is_empty() {
        // No way to authenticate (e.g. a box from before passwords were stored).
        // Signal it so the caller can warn: without an account-data write, the
        // reconcile will revoke the local add on its next tick.
        return Err("no admin credentials to record the pairing in account-data".into());
    }
    let base = format!("http://127.0.0.1:{}", HOMESERVER_PORT + off());
    let user_id = format!("@{username}:{onion}");
    // Same percent-encode as pair_fetch_onions (@ and : are reserved in the path).
    let enc = user_id.replace('@', "%40").replace(':', "%3A");
    let url = format!("{base}/_matrix/client/v3/user/{enc}/account_data/{PAIR_ACCOUNT_DATA_TYPE}");
    let client = box_http_client(); // [QW-rust c] request timeout (no reqwest default)

    for _ in 0..attempts {
        let Some(token) = pair_login(&client, &base, &username, &password).await else {
            sleep(Duration::from_secs(2)).await;
            continue;
        };
        let current = match pair_fetch_onions(&client, &base, &user_id, &token).await {
            Ok(c) => c,
            Err(_) => {
                // Token rejected or transient — back off and retry the whole
                // login+read so we never PUT against a bad read.
                sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        // Idempotent union: keep the current set, add target if absent.
        let mut next = current.clone();
        if !next.iter().any(|o| o == target) {
            next.push(target.to_string());
        }
        let put = client
            .put(&url)
            .bearer_auth(&token)
            .json(&serde_json::json!({ "onions": next }))
            .send()
            .await;
        match put {
            Ok(r) if r.status().is_success() => {
                eprintln!("[pureprivacy] pairing accept: recorded {target} in account-data");
                return Ok(());
            }
            _ => {
                sleep(Duration::from_secs(2)).await;
            }
        }
    }
    Err(format!("couldn't record {target} in pairing account-data after retries"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// GET an account-data blob. `Ok(None)` = absent (404), `Ok(Some)` = present,
/// `Err(true)` = token rejected (re-login), `Err(false)` = transient.
async fn get_account_data(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<Option<serde_json::Value>, bool> {
    let r = client.get(url).bearer_auth(token).send().await.map_err(|_| false)?;
    if r.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(true);
    }
    if r.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !r.status().is_success() {
        return Err(false);
    }
    Ok(Some(r.json().await.map_err(|_| false)?))
}

/// Validate a phone-issued command blob (feature B, hyper-secure rules):
/// allowlisted action, non-empty id NOT already handled, and a freshness window
/// (`expires_ts` in the near future). Returns `(id, action)` only if it may run.
fn validate_command(
    cmd: &serde_json::Value,
    handled: &std::collections::HashSet<String>,
) -> Option<(String, String)> {
    let id = cmd.get("id")?.as_str()?.to_string();
    if id.is_empty() || handled.contains(&id) {
        return None; // once-only: never run the same id twice
    }
    let action = cmd.get("action")?.as_str()?.to_string();
    if !matches!(action.as_str(), "restart" | "reset") {
        return None; // allowlist only (a cleared "done" command lands here → ignored)
    }
    // Freshness: the command must carry an expiry in the near future. This kills
    // replay of a stale/old blob (and a "done" tombstone has no expires_ts).
    let now = now_ms();
    let expires = cmd.get("expires_ts").and_then(|v| v.as_u64()).unwrap_or(0);
    if expires <= now || expires > now + 5 * 60 * 1000 {
        return None;
    }
    Some((id, action))
}

fn execute_command(app: &AppHandle, action: &str) {
    match action {
        "restart" => {
            eprintln!("[pureprivacy] box config: restart requested by the phone");
            stop_lifecycle(app);
            start_lifecycle(app, None);
        }
        "reset" => {
            eprintln!("[pureprivacy] box config: FACTORY RESET requested by the phone");
            let _ = crate::commands::reset_box(app.clone());
        }
        _ => {}
    }
}

/// Box config from the phone (feature B). Publishes a read-only status blob the
/// PP Config app reads, and executes tightly-guarded commands the phone writes —
/// all over the SAME authenticated account-data channel as pairing.
///
/// SECURITY: no new network surface (rides the local client API as the admin);
/// only the admin account's own devices can read/write these keys. The box only
/// ever READS commands + WRITES status/results — secrets NEVER enter account-data.
/// Commands are allowlisted, freshness-gated, and once-only (executed id recorded
/// + the command cleared to a no-op "done" so it can't re-fire).
async fn run_box_config(app: AppHandle, gen: u64) {
    let (username, onion, password) = state::read(&app, |i| {
        (
            i.username.clone(),
            i.onion.clone().unwrap_or_default(),
            i.admin_password.clone(),
        )
    });
    if username.is_empty() || onion.is_empty() || password.is_empty() {
        return; // can't authenticate (e.g. a box from before passwords were stored)
    }
    let base = format!("http://127.0.0.1:{}", HOMESERVER_PORT + off());
    let user_id = format!("@{username}:{onion}");
    let enc = user_id.replace('@', "%40").replace(':', "%3A");
    let ad_url =
        |ty: &str| format!("{base}/_matrix/client/v3/user/{enc}/account_data/{ty}");
    let client = box_http_client();
    let version = env!("CARGO_PKG_VERSION");
    let mut token: Option<String> = None;
    let mut handled: std::collections::HashSet<String> = std::collections::HashSet::new();

    loop {
        if is_stale(&app, gen) {
            return;
        }
        if token.is_none() {
            token = pair_login(&client, &base, &username, &password).await;
        }
        if let Some(t) = token.clone() {
            // 1) Publish read-only status for PP Config.
            let (hs, tor, voice, paired, box_name) = state::read(&app, |i| {
                (i.homeserver, i.tor, i.voice, i.paired_count, i.box_name.clone())
            });
            let status = serde_json::json!({
                "onion": onion,
                "box_name": box_name,
                "version": version,
                "services": { "homeserver": hs, "tor": tor, "voice": voice },
                "paired_count": paired,
                "updated_ts": now_ms(),
            });
            let _ = client
                .put(ad_url(BOXSTATUS_ACCOUNT_DATA_TYPE))
                .bearer_auth(&t)
                .json(&status)
                .send()
                .await;

            // 2) Read + execute a guarded command.
            match get_account_data(&client, &ad_url(COMMAND_ACCOUNT_DATA_TYPE), &t).await {
                Ok(Some(cmd)) => {
                    if let Some((id, action)) = validate_command(&cmd, &handled) {
                        handled.insert(id.clone());
                        // Ack first (a destructive action tears the box down), then clear
                        // the command to a no-op so it can never re-fire, THEN execute.
                        let _ = client
                            .put(ad_url(COMMAND_RESULT_ACCOUNT_DATA_TYPE))
                            .bearer_auth(&t)
                            .json(&serde_json::json!({ "id": id, "ok": true, "done_ts": now_ms() }))
                            .send()
                            .await;
                        let _ = client
                            .put(ad_url(COMMAND_ACCOUNT_DATA_TYPE))
                            .bearer_auth(&t)
                            .json(&serde_json::json!({ "id": id, "action": "done" }))
                            .send()
                            .await;
                        execute_command(&app, &action);
                        // restart bumps the generation / reset wipes the box → this loop
                        // exits via is_stale on the next tick.
                    }
                }
                Ok(None) => {}
                Err(true) => token = None, // token rejected → re-login next tick
                Err(false) => {}           // transient
            }
        }
        sleep(Duration::from_secs(4)).await;
    }
}

/// Poll the owner's pairing account data and keep the fed-proxy allowlist in sync.
/// Loopback federation-allowlist validator (review item W3-T1). Binds for the
/// life of this box generation; Caddy `forward_auth`s authenticated federation
/// requests here and we 200/403 each by matching the X-Matrix Authorization
/// origin against the live pairings allowlist (see [`crate::fedauth`]). A restart
/// (gen bump) drops the listener and frees the port; a malformed header fails
/// CLOSED. The `select!` races `accept()` against a short timer so the stale-gen
/// check runs even with no traffic.
async fn run_fedauth(app: AppHandle, gen: u64) {
    let Ok(paths) = config::ensure_dirs(&app) else { return };
    let port = FEDAUTH_PORT + off();
    // Bind with retry: on a fast restart the PREVIOUS generation's run_fedauth can still
    // hold this port until its ~2s select() cycle notices the stale gen and drops the
    // listener. A single bind would then hit EADDRINUSE and give up — leaving Caddy's
    // forward_auth with nothing to call (connection-refused) so ALL authenticated
    // federation is DENIED until a lucky later restart. Retry for a few seconds (bailing
    // if this generation itself goes stale), the same resilience spawn_supervised gives
    // every other sidecar.
    let listener = loop {
        if is_stale(&app, gen) {
            return;
        }
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => break l,
            Err(e) => {
                eprintln!("[pp][fedauth] bind 127.0.0.1:{port} failed ({e}); retrying…");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    };
    eprintln!("[pp][fedauth] federation allowlist validator on 127.0.0.1:{port}");
    loop {
        if is_stale(&app, gen) {
            return;
        }
        tokio::select! {
            res = listener.accept() => {
                if let Ok((mut sock, _)) = res {
                    let dir = paths.data_root.clone();
                    tauri::async_runtime::spawn(async move {
                        crate::fedauth::handle_conn(&mut sock, &dir).await;
                    });
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

async fn run_pairing_sync(app: AppHandle, gen: u64) {
    let (username, onion, password) = state::read(&app, |i| {
        (
            i.username.clone(),
            i.onion.clone().unwrap_or_default(),
            i.admin_password.clone(),
        )
    });
    if username.is_empty() || onion.is_empty() || password.is_empty() {
        return; // can't authenticate (e.g. a box from before passwords were stored)
    }
    let Ok(paths) = config::ensure_dirs(&app) else { return };
    let base = format!("http://127.0.0.1:{}", HOMESERVER_PORT + off());
    let user_id = format!("@{username}:{onion}");
    let client = box_http_client(); // [QW-rust c] request timeout (no reqwest default)
    let mut token: Option<String> = None;
    loop {
        if is_stale(&app, gen) {
            return;
        }
        if token.is_none() {
            token = pair_login(&client, &base, &username, &password).await;
        }
        if let Some(t) = token.clone() {
            match pair_fetch_onions(&client, &base, &user_id, &t).await {
                Ok(onions) => {
                    // RECONCILE: account-data is the single source of truth. We
                    // add desired−known AND remove known−desired so a removal on
                    // the phone (which drops the onion from account-data) cuts
                    // the allowlist within a tick. The strict v3-onion check +
                    // self-onion filter (guard #7) gate what reaches the desired
                    // set, so a malformed/own onion can never be allowlisted and
                    // never counts as "desired".
                    let desired: std::collections::HashSet<String> = onions
                        .into_iter()
                        .filter(|o| crate::pairing::is_valid_onion(o) && *o != onion)
                        .collect();
                    let known: std::collections::HashSet<String> =
                        crate::pairing::onions(&paths.data_root).into_iter().collect();
                    let mut changed = false;

                    // Add: desired − known.
                    for o in desired.difference(&known) {
                        if crate::pairing::add(&paths.data_root, o).is_ok() {
                            eprintln!("[pureprivacy] pairing reconcile: allowlisting {o}");
                            changed = true;
                            // Pre-warm the Tor circuit to this brand-new peer so the first
                            // federated invite/event lands in seconds instead of paying the
                            // full cold-circuit cost (which stalled first-time pairing for
                            // minutes in testing). Fire-and-forget.
                            let peer = o.clone();
                            tauri::async_runtime::spawn(async move { warm_peer_circuit(&peer, true).await });
                        }
                    }

                    // Remove: known − desired. WIPE-GUARD: if the authoritative
                    // list came back empty while we still know peers, this could
                    // be a transient/hostile read that pair_fetch_onions let
                    // through as a *genuine* empty (404 / empty array). Before
                    // honouring a mass-removal, re-fetch once more; only proceed
                    // if the SECOND read is ALSO Ok(empty). If it's Err or
                    // non-empty, skip removal this tick (no wipe on a fluke).
                    let mut do_remove = true;
                    if desired.is_empty() && !known.is_empty() {
                        match pair_fetch_onions(&client, &base, &user_id, &t).await {
                            Ok(second) => {
                                let confirmed_empty = second
                                    .iter()
                                    .all(|o| !crate::pairing::is_valid_onion(o) || *o == onion);
                                if !confirmed_empty {
                                    // Second read disagrees — not actually empty.
                                    do_remove = false;
                                }
                            }
                            // Err on the re-fetch → don't trust the empty.
                            Err(_) => do_remove = false,
                        }
                        if !do_remove {
                            eprintln!(
                                "[pureprivacy] pairing reconcile: empty list unconfirmed on re-fetch — skipping mass removal this tick"
                            );
                        }
                    }
                    if do_remove {
                        for o in known.difference(&desired) {
                            if crate::pairing::remove(&paths.data_root, o).is_ok() {
                                eprintln!("[pureprivacy] pairing reconcile: revoking {o}");
                                changed = true;
                            }
                        }
                    }

                    if changed {
                        reload_fedproxy(&app);
                        state::update(&app, |i| {
                            i.paired_count =
                                crate::pairing::onions(&paths.data_root).len() as u32
                        });
                    }
                }
                Err(true) => token = None, // re-login on next tick
                Err(false) => {}           // transient; retry
            }
        }
        sleep(Duration::from_secs(3)).await;
    }
}

/// Tier-3 TRANSPORT federation keepalive — keep the Tor CIRCUIT to each paired peer
/// warm so a call (or any Tor pressure) can't leave federation wedged.
///
/// Tier-1 caps tuwunel's federation backoff (`sender_retry_backoff_limit`) and Tier-2
/// ([run_federation_keepalive]) sends an `m.dummy` to wake tuwunel's sender — but BOTH
/// ride tuwunel's own federation path. If the underlying Tor circuit to a peer went
/// cold, the nudge fails too and federation stays stuck. A call does exactly that: its
/// media saturates the box's shared Tor and starves the federation circuit, which then
/// stays cold after the call (observed live — a jaime↔arnaud call killed messaging in
/// BOTH directions until the circuit was manually re-warmed with a `/version` GET).
///
/// This loop closes the gap at the TRANSPORT layer: every ~15s it GETs each paired
/// peer's `/_matrix/federation/v1/version` (an `@open` path) over the box's SOCKS,
/// forcing Tor to keep — and, after a call, rebuild — the rendezvous circuit that
/// tuwunel's sender then reuses. So a call can leave federation cold for at most one
/// cadence, and messaging heals within ~15s of the call ending instead of staying stuck.
/// Auth-free (no admin token needed), so it runs even when Tier-2's login can't.
async fn run_federation_circuit_warm(app: AppHandle, gen: u64) {
    let Ok(paths) = config::ensure_dirs(&app) else {
        return;
    };
    let mut ticks: u64 = 0;
    loop {
        if is_stale(&app, gen) {
            return;
        }
        let own = state::read(&app, |i| i.onion.clone().unwrap_or_default());
        // Same desired set as Tier-2: the allowlisted onions ARE the paired peers,
        // minus our own and any malformed entry.
        let peers: Vec<String> = crate::pairing::onions(&paths.data_root)
            .into_iter()
            .filter(|o| crate::pairing::is_valid_onion(o) && *o != own)
            .collect();
        for p in &peers {
            let p = p.clone();
            // Fire-and-forget + quiet (no per-tick log spam) — a cold/offline peer just
            // fails and the next tick retries.
            tauri::async_runtime::spawn(async move { warm_peer_circuit(&p, false).await });
        }
        // Rare heartbeat (~every 5 min) so the log shows the loop is alive, never per-peer.
        ticks += 1;
        if !peers.is_empty() && ticks % 20 == 1 {
            eprintln!(
                "[pureprivacy] federation circuit-warm: keeping {} peer(s) warm",
                peers.len()
            );
        }
        sleep(Duration::from_secs(15)).await;
    }
}

/// Tier-2 invisible federation keepalive.
///
/// tuwunel's federation sender is event-driven: once a destination is marked
/// `Failed` it only retries when the NEXT outbound event is queued for it —
/// nothing wakes it on a timer. In a totally silent room a message sent into a
/// transient outage stays stuck until some new traffic appears. This loop
/// closes that gap (Tier-2) WITHOUT leaking presence or
/// read position: every ~30s it sends an invisible `m.dummy` to-device message
/// to each paired peer. A to-device EDU federates to the peer's server, waking
/// tuwunel's sender for that destination so any backlog flushes — and is fully
/// invisible (no timeline event, no presence, no read marker). `m.dummy` is the
/// same benign no-op the matrix-rust-sdk already emits for key verification.
///
/// 30s cadence sits well under the 60s backoff cap (Tier-1), so a silent-room
/// stall is bounded to ~one cadence + flush. Terminates cleanly on is_stale
/// (box stop/restart) like every other supervision loop.
async fn run_federation_keepalive(app: AppHandle, gen: u64) {
    let (username, onion, password) = state::read(&app, |i| {
        (
            i.username.clone(),
            i.onion.clone().unwrap_or_default(),
            i.admin_password.clone(),
        )
    });
    if username.is_empty() || onion.is_empty() || password.is_empty() {
        return; // can't authenticate (e.g. a box from before passwords were stored)
    }
    let Ok(paths) = config::ensure_dirs(&app) else { return };
    let base = format!("http://127.0.0.1:{}", HOMESERVER_PORT + off());
    let client = box_http_client(); // [QW-rust c] request timeout (no reqwest default)
    let mut token: Option<String> = None;
    // Monotonic transaction-id counter — a unique txn id per send guarantees the
    // PUT is treated as a new request (not a dedup retry) without any reliance on
    // wall-clock/random uniqueness.
    let mut txn: u64 = 0;

    loop {
        if is_stale(&app, gen) {
            return;
        }

        // Cache the admin token; re-login on None (first tick, or after a prior
        // request error cleared it). Mirrors run_pairing_sync.
        if token.is_none() {
            token = pair_login(&client, &base, &username, &password).await;
        }
        let Some(t) = token.clone() else {
            sleep(Duration::from_secs(30)).await;
            continue;
        };

        // The box's allowlisted onions are exactly the paired destinations we
        // want to keep warm. Nothing paired → nothing to nudge. Apply the same
        // is_valid_onion + self-onion filter run_pairing_sync uses on its
        // desired set (parity): a malformed entry in pairings.json never matches
        // a real joined-room peer anyway, but filtering here keeps the two loops
        // consistent and excludes our own onion up front.
        let desired: std::collections::HashSet<String> =
            crate::pairing::onions(&paths.data_root)
                .into_iter()
                .filter(|o| crate::pairing::is_valid_onion(o) && *o != onion)
                .collect();
        if desired.is_empty() {
            sleep(Duration::from_secs(30)).await;
            continue;
        }

        // Resolve the paired peers' full user ids from the rooms we're joined to,
        // then nudge each distinct peer once. keepalive_tick is best-effort —
        // one peer's/room's transport error is skipped, not fatal. It returns
        // Err(()) ONLY when the auth-bearing joined_rooms GET itself fails, in
        // which case we drop the token to force a fresh login next tick rather
        // than aborting the loop.
        let nudged = match keepalive_tick(&client, &base, &t, &onion, &desired, &mut txn).await {
            Ok(n) => n,
            Err(()) => {
                // joined_rooms GET failed (likely a rejected/expired token) —
                // re-login next time.
                token = None;
                sleep(Duration::from_secs(30)).await;
                continue;
            }
        };

        // At most ONE concise line per tick (never per-peer).
        if nudged > 0 {
            eprintln!("[pureprivacy] federation keepalive: nudged {nudged} peer(s)");
        }

        sleep(Duration::from_secs(30)).await;
    }
}

/// One keepalive pass: enumerate joined rooms, find the distinct paired-peer
/// user ids in them, and send each an invisible `m.dummy` to-device message.
/// Returns the number of peers nudged, or `Err(())` only when the auth-bearing
/// `joined_rooms` GET fails (so the caller can re-login next tick). Best-effort,
/// one failure does not abort the tick: a transport error on any single
/// per-room `joined_members` GET or per-peer `sendToDevice` PUT is skipped so it
/// can never starve the remaining rooms/peers — the full set is retried 30s
/// later anyway. Nothing here mutates box state.
async fn keepalive_tick(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    our_onion: &str,
    desired: &std::collections::HashSet<String>,
    txn: &mut u64,
) -> Result<usize, ()> {
    // GET joined rooms. This is the auth-bearing call that gates the whole tick:
    // a transport error here is the ONLY thing that returns Err(()) (so the
    // caller drops the token and re-logins). Per-room and per-peer calls below
    // are best-effort and never abort the tick.
    let r = client
        .get(format!("{base}/_matrix/client/v3/joined_rooms"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| ())?;
    let v: serde_json::Value = r.json().await.map_err(|_| ())?;
    let rooms: Vec<String> = v
        .get("joined_rooms")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    // Collect the DISTINCT set of paired-peer user ids across all joined rooms.
    // One room's transport error is skipped (continue) so it can't abort the
    // enumeration of every later room.
    let mut peers: std::collections::HashSet<String> = std::collections::HashSet::new();
    for room in &rooms {
        // Percent-encode the room id for the path (! and : are reserved).
        let enc = room.replace('!', "%21").replace(':', "%3A");
        let r = match client
            .get(format!("{base}/_matrix/client/v3/rooms/{enc}/joined_members"))
            .bearer_auth(token)
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => continue, // transport error on this room → skip, keep going
        };
        let Ok(v) = r.json::<serde_json::Value>().await else {
            continue; // unparseable body → skip this room
        };
        if let Some(joined) = v.get("joined").and_then(|x| x.as_object()) {
            for user_id in joined.keys() {
                // Server part = everything after the last ':'. Keep it only if
                // it's a paired onion AND not our own server.
                if let Some(server) = user_id.rsplit(':').next() {
                    if server != our_onion && desired.contains(server) {
                        peers.insert(user_id.clone());
                    }
                }
            }
        }
    }

    // Nudge each distinct peer with an invisible to-device m.dummy. The "*"
    // device wildcard targets all of the peer's devices; the empty body is a
    // benign no-op — the EDU's federation to the peer's server is the point.
    // One peer's transport error is skipped (it just isn't counted) so it can
    // never starve the remaining peers; only 2xx counts as nudged.
    let mut nudged = 0usize;
    for peer in &peers {
        *txn += 1;
        let url = format!("{base}/_matrix/client/v3/sendToDevice/m.dummy/{txn}");
        match client
            .put(url)
            .bearer_auth(token)
            .json(&serde_json::json!({
                "messages": { peer: { "*": {} } }
            }))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => nudged += 1,
            _ => {} // transport error or non-2xx → skip this peer, keep going
        }
    }
    Ok(nudged)
}

async fn wait_for_hostname(
    app: &AppHandle,
    gen: u64,
    path: &std::path::Path,
    timeout: Duration,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if is_stale(app, gen) {
            return Err("cancelled".into());
        }
        if let Ok(contents) = std::fs::read_to_string(path) {
            let onion = contents.trim().to_string();
            if !onion.is_empty() {
                return Ok(onion);
            }
        }
        if Instant::now() >= deadline {
            return Err("tor didn't produce an address in time".into());
        }
        sleep(TICK).await;
    }
}

async fn wait_for_http(
    app: &AppHandle,
    gen: u64,
    port: u16,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if is_stale(app, gen) {
            return Err("cancelled".into());
        }
        if http_versions_ok(port).await {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("homeserver didn't come up in time".into());
        }
        sleep(TICK).await;
    }
}

/// Minimal raw HTTP/1.1 readiness probe of GET /_matrix/client/versions —
/// avoids pulling a full HTTP client just to read a status line.
async fn http_versions_ok(port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)).await else {
        return false;
    };
    let request = format!(
        "GET /_matrix/client/versions HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).await.is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    let Ok(n) = stream.read(&mut buf).await else {
        return false;
    };
    String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200")
}

// ---------------------------------------------------------------------------
// The supervision loop
// ---------------------------------------------------------------------------

enum Readiness {
    /// Healthy once this file exists and is non-empty (tor's hostname file).
    File(PathBuf),
    /// Healthy once GET /_matrix/client/versions answers 200 on this port.
    Http(u16),
    /// Healthy once a TCP connection to this loopback port succeeds (coturn).
    Tcp(u16),
}

impl Readiness {
    async fn check(&self) -> bool {
        match self {
            Readiness::File(path) => std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false),
            Readiness::Http(port) => http_versions_ok(*port).await,
            Readiness::Tcp(port) => TcpStream::connect(("127.0.0.1", *port)).await.is_ok(),
        }
    }
}

fn set_service(app: &AppHandle, name: &'static str, value: ServiceState) {
    state::update(app, |inner| match name {
        "tor" => inner.tor = value,
        "homeserver" => inner.homeserver = value,
        "voice" => inner.voice = value,
        _ => {}
    });
}

/// stdout+stderr targets for a sidecar. /dev/null unless PUREPRIVACY_SIDECAR_LOGS=1,
/// in which case both go to <data>/logs/<name>.log (debug aid for call/media issues).
fn sidecar_stdio(app: &AppHandle, name: &str) -> (Stdio, Stdio) {
    if std::env::var("PUREPRIVACY_SIDECAR_LOGS").ok().as_deref() == Some("1") {
        if let Ok(p) = config::paths(app) {
            let dir = p.data_root.join("logs");
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(format!("{name}.log"));
            if let (Ok(o), Ok(e)) = (
                std::fs::OpenOptions::new().create(true).append(true).open(&path),
                std::fs::OpenOptions::new().create(true).append(true).open(&path),
            ) {
                return (Stdio::from(o), Stdio::from(e));
            }
        }
    }
    (Stdio::null(), Stdio::null())
}

/// Spawn `program`, restart on crash with capped exponential backoff, and
/// die quietly when the generation goes stale (stop/restart/app exit).
fn spawn_supervised(
    app: AppHandle,
    gen: u64,
    name: &'static str,
    program: PathBuf,
    args: Vec<String>,
    envs: Vec<(String, String)>,
    readiness: Readiness,
) {
    tauri::async_runtime::spawn(async move {
        let mut backoff = BACKOFF_START;
        loop {
            if is_stale(&app, gen) {
                return;
            }
            set_service(&app, name, ServiceState::Starting);

            // Sidecar logs go to /dev/null by default (privacy + tidiness). For
            // debugging, PUREPRIVACY_SIDECAR_LOGS=1 redirects each sidecar's
            // stdout+stderr to <data>/logs/<name>.log so a call/media failure can be
            // traced (coturn allocations, LiveKit ICE state, tuwunel federation).
            let (out, err) = sidecar_stdio(&app, name);
            let mut cmd = Command::new(&program);
            cmd.args(&args)
                .envs(envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                .stdin(Stdio::null())
                .stdout(out)
                .stderr(err)
                // Backstop: SIGKILL if the runtime drops us with the child alive.
                .kill_on_drop(true);

            let mut child = match cmd.spawn() {
                Ok(child) => child,
                Err(err) => {
                    eprintln!("[pureprivacy] couldn't start {name}: {err}");
                    set_service(&app, name, ServiceState::Error);
                    jittered_sleep(backoff).await; // [QW-rust b] decorrelate respawns
                    backoff = (backoff * 2).min(BACKOFF_CAP);
                    continue;
                }
            };
            if let Some(pid) = child.id() {
                app.state::<Supervisor>().record_pid(name, pid);
            }

            // Poll loop: watches for child exit, stale generation, and
            // readiness — all on one tick so we never hold the Child across
            // an unbounded await (we need try_wait() + start_kill() access).
            let mut healthy = false;
            let exited_cleanly_cancelled = loop {
                if is_stale(&app, gen) {
                    let _ = child.start_kill();
                    let _ = child.wait().await; // reap, no zombie
                    break true;
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        eprintln!("[pureprivacy] {name} exited: {status}");
                        break false;
                    }
                    Ok(None) => {
                        if !healthy && readiness.check().await {
                            healthy = true;
                            backoff = BACKOFF_START; // it came up: reset backoff
                            set_service(&app, name, ServiceState::Healthy);
                        }
                        sleep(TICK).await;
                    }
                    Err(err) => {
                        eprintln!("[pureprivacy] {name} wait error: {err}");
                        break false;
                    }
                }
            };
            app.state::<Supervisor>().clear_pid(name);

            if exited_cleanly_cancelled || is_stale(&app, gen) {
                return;
            }
            // Crash: mark, back off, respawn.
            set_service(&app, name, ServiceState::Error);
            jittered_sleep(backoff).await; // [QW-rust b] decorrelate respawns
            backoff = (backoff * 2).min(BACKOFF_CAP);
        }
    });
}
