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

use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time::{sleep, Instant};

use crate::config::{
    self, off, FEDPROXY_PORT, HOMESERVER_PORT, LIVEKIT_WS_PORT, LIVEKIT_WSS_ONION_PORT, LKJWT_PORT,
    SOCKS_PORT, TURN_PORT,
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
        // `caddy reload` talks to the running instance's admin API and swaps
        // config atomically; no-op (errors ignored) if caddy isn't running.
        let _ = std::process::Command::new(bins.join("caddy"))
            .args([
                "reload",
                "--config",
                &paths.caddyfile.to_string_lossy(),
                "--adapter",
                "caddyfile",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
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

    // Wait for tor to mint (or re-load) the hidden-service hostname.
    let onion = wait_for_hostname(&app, gen, &paths.hostname_file, Duration::from_secs(180)).await?;
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

    wait_for_http(&app, gen, HOMESERVER_PORT + off(), Duration::from_secs(120)).await?;
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
                    if !is_stale(&app, gen) {
                        return Err(e);
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
    Ok(())
}

/// Account-data event type the phone writes scanned-peer onions into.
const PAIR_ACCOUNT_DATA_TYPE: &str = "ai.tournesol.pureprivacy.pairings";

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
    Ok(v.get("onions")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default())
}

/// Poll the owner's pairing account data and keep the fed-proxy allowlist in sync.
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
    let client = reqwest::Client::new();
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
                    let known: std::collections::HashSet<String> =
                        crate::pairing::onions(&paths.data_root).into_iter().collect();
                    let mut changed = false;
                    for o in onions {
                        // Strict v3-onion check before this account-data value
                        // reaches the Caddy allowlist (regex-injection surface).
                        if crate::pairing::is_valid_onion(&o)
                            && o != onion
                            && !known.contains(&o)
                            && crate::pairing::add(&paths.data_root, &o).is_ok()
                        {
                            eprintln!("[pureprivacy] QR pairing: allowlisting {o}");
                            changed = true;
                        }
                    }
                    if changed {
                        reload_fedproxy(&app);
                    }
                }
                Err(true) => token = None, // re-login on next tick
                Err(false) => {}           // transient; retry
            }
        }
        sleep(Duration::from_secs(3)).await;
    }
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
                    sleep(backoff).await;
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
            sleep(backoff).await;
            backoff = (backoff * 2).min(BACKOFF_CAP);
        }
    });
}
