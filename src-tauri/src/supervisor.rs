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

use crate::config::{self, HOMESERVER_PORT};
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

// ---------------------------------------------------------------------------
// Lifecycle entry points (called from commands + tray)
// ---------------------------------------------------------------------------

/// Start (or restart) the box. Picks real or demo mode by binary presence.
pub fn start_lifecycle(app: &AppHandle) {
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
            if let Err(err) = run_real(handle.clone(), gen).await {
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
    });
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

async fn run_real(app: AppHandle, gen: u64) -> Result<(), String> {
    let paths = config::ensure_dirs(&app)?;
    config::render_torrc(&app)?;
    // Placeholder so the file always exists; re-rendered with the real onion
    // below, before tuwunel ever starts.
    let known_onion = state::read(&app, |inner| inner.onion.clone());
    config::render_tuwunel(&app, known_onion.as_deref().unwrap_or("placeholder.onion"))?;

    let bins = bin_dir(&app)?;
    spawn_supervised(
        app.clone(),
        gen,
        "tor",
        bins.join("tor"),
        vec!["-f".into(), paths.torrc.to_string_lossy().into_owned()],
        Readiness::File(paths.hostname_file.clone()),
    );

    state::update(&app, |inner| inner.setup_stage = Some(SetupStage::MintingAddress));

    // Wait for tor to mint (or re-load) the hidden-service hostname.
    let onion = wait_for_hostname(&app, gen, &paths.hostname_file, Duration::from_secs(180)).await?;
    state::update(&app, |inner| inner.onion = Some(onion.clone()));
    let _ = state::persist(&app);

    // Now we know the server_name; render for real and start the homeserver.
    config::render_tuwunel(&app, &onion)?;
    spawn_supervised(
        app.clone(),
        gen,
        "homeserver",
        bins.join("tuwunel"),
        vec!["-c".into(), paths.tuwunel_toml.to_string_lossy().into_owned()],
        Readiness::Http(HOMESERVER_PORT),
    );

    wait_for_http(&app, gen, HOMESERVER_PORT, Duration::from_secs(120)).await?;
    if is_stale(&app, gen) {
        return Ok(());
    }
    state::update(&app, |inner| {
        inner.setup_stage = Some(SetupStage::Ready);
        inner.phase = Phase::Running;
    });
    Ok(())
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
}

impl Readiness {
    async fn check(&self) -> bool {
        match self {
            Readiness::File(path) => std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false),
            Readiness::Http(port) => http_versions_ok(*port).await,
        }
    }
}

fn set_service(app: &AppHandle, name: &'static str, value: ServiceState) {
    state::update(app, |inner| match name {
        "tor" => inner.tor = value,
        "homeserver" => inner.homeserver = value,
        _ => {}
    });
}

/// Spawn `program`, restart on crash with capped exponential backoff, and
/// die quietly when the generation goes stale (stop/restart/app exit).
fn spawn_supervised(
    app: AppHandle,
    gen: u64,
    name: &'static str,
    program: PathBuf,
    args: Vec<String>,
    readiness: Readiness,
) {
    tauri::async_runtime::spawn(async move {
        let mut backoff = BACKOFF_START;
        loop {
            if is_stale(&app, gen) {
                return;
            }
            set_service(&app, name, ServiceState::Starting);

            let mut cmd = Command::new(&program);
            cmd.args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
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
