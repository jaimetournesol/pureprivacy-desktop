//! Application state: the single source of truth the UI polls via `get_status`.
//!
//! Everything lives behind one `Mutex` (`AppState`). Minimal non-secret facts
//! are persisted to `<app_data_dir>/box.json`; secrets (recovery phrase and
//! connect token) go to `<app_data_dir>/secrets.json` with 0600 perms.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Fresh,
    SettingUp,
    Running,
    Stopped,
    Error,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupStage {
    StartingServices,
    MintingAddress,
    Ready,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Starting,
    Healthy,
    Stopped,
    Error,
}

#[derive(Clone, Serialize)]
pub struct Service {
    pub name: &'static str,
    pub state: ServiceState,
}

/// The exact shape the frontend polls. Field names match the shared contract.
#[derive(Clone, Serialize)]
pub struct Status {
    pub phase: Phase,
    pub onion: Option<String>,
    pub demo_mode: bool,
    pub setup_stage: Option<SetupStage>,
    pub services: Vec<Service>,
    pub people_count: u32,
    pub paired_count: u32,
    pub box_name: String,
}

pub struct Inner {
    pub phase: Phase,
    pub onion: Option<String>,
    pub demo_mode: bool,
    pub setup_stage: Option<SetupStage>,
    pub homeserver: ServiceState,
    pub tor: ServiceState,
    pub people_count: u32,
    pub paired_count: u32,
    pub box_name: String,
    pub username: String,
    pub created: String,
    /// Six-word recovery phrase, empty until `begin_setup`.
    pub phrase: Vec<String>,
    /// Hex pairing token embedded in the connect QR, empty until `begin_setup`.
    pub token: String,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            phase: Phase::Fresh,
            onion: None,
            demo_mode: false,
            setup_stage: None,
            homeserver: ServiceState::Stopped,
            tor: ServiceState::Stopped,
            people_count: 0,
            paired_count: 0,
            box_name: String::new(),
            username: String::new(),
            created: String::new(),
            phrase: Vec::new(),
            token: String::new(),
        }
    }
}

impl Inner {
    pub fn status(&self) -> Status {
        Status {
            phase: self.phase,
            onion: self.onion.clone(),
            demo_mode: self.demo_mode,
            setup_stage: self.setup_stage,
            services: vec![
                Service { name: "homeserver", state: self.homeserver },
                Service { name: "tor", state: self.tor },
            ],
            people_count: self.people_count,
            paired_count: self.paired_count,
            box_name: self.box_name.clone(),
        }
    }
}

#[derive(Default)]
pub struct AppState(pub Mutex<Inner>);

/// One-line tray summary derived from phase. Plain, calm, no jargon.
fn tray_line(inner: &Inner) -> String {
    match inner.phase {
        Phase::Fresh => "PurePrivacy — not set up yet".to_string(),
        Phase::SettingUp => "PurePrivacy — setting up your box…".to_string(),
        Phase::Running => "PurePrivacy — running, people can reach you".to_string(),
        Phase::Stopped => "PurePrivacy — paused, your box is offline".to_string(),
        Phase::Error => "PurePrivacy — something needs attention".to_string(),
    }
}

/// Mutate state, then refresh the tray status line (outside the lock).
pub fn update<F: FnOnce(&mut Inner)>(app: &AppHandle, f: F) {
    let text = {
        let state = app.state::<AppState>();
        let mut guard = state.0.lock().expect("state mutex poisoned");
        f(&mut guard);
        tray_line(&guard)
    };
    crate::tray::set_status_text(app, &text);
}

/// Read state without mutating it.
pub fn read<T, F: FnOnce(&Inner) -> T>(app: &AppHandle, f: F) -> T {
    let state = app.state::<AppState>();
    let guard = state.0.lock().expect("state mutex poisoned");
    f(&guard)
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct PersistedBox {
    box_name: String,
    username: String,
    created: String,
    onion: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct PersistedSecrets {
    phrase: Vec<String>,
    token: String,
}

pub fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("couldn't resolve app data dir: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create app data dir: {e}"))?;
    Ok(dir)
}

#[cfg(unix)]
fn set_0600(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_0600(_path: &std::path::Path) {}

fn write_private(path: &std::path::Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    set_0600(path);
    Ok(())
}

/// Persist box.json + secrets.json from current state.
pub fn persist(app: &AppHandle) -> Result<(), String> {
    let dir = app_data_dir(app)?;
    let (boxed, secrets) = read(app, |inner| {
        (
            PersistedBox {
                box_name: inner.box_name.clone(),
                username: inner.username.clone(),
                created: inner.created.clone(),
                onion: inner.onion.clone(),
            },
            PersistedSecrets { phrase: inner.phrase.clone(), token: inner.token.clone() },
        )
    });
    write_private(
        &dir.join("box.json"),
        &serde_json::to_string_pretty(&boxed).map_err(|e| e.to_string())?,
    )?;
    write_private(
        &dir.join("secrets.json"),
        &serde_json::to_string_pretty(&secrets).map_err(|e| e.to_string())?,
    )?;
    Ok(())
}

/// Load persisted state at launch. If box.json exists the box was set up
/// before, so we come up in `stopped` (the user explicitly starts it).
pub fn load_persisted(app: &AppHandle) {
    let Ok(dir) = app_data_dir(app) else { return };
    let Ok(raw) = std::fs::read_to_string(dir.join("box.json")) else { return };
    let Ok(boxed) = serde_json::from_str::<PersistedBox>(&raw) else { return };
    let secrets: PersistedSecrets = std::fs::read_to_string(dir.join("secrets.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    update(app, |inner| {
        inner.phase = Phase::Stopped;
        inner.box_name = boxed.box_name;
        inner.username = boxed.username;
        inner.created = boxed.created;
        inner.onion = boxed.onion;
        inner.phrase = secrets.phrase;
        inner.token = secrets.token;
    });
}
