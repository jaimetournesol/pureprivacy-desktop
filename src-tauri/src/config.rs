//! Renders tuwunel.toml + torrc into `<app_data_dir>/config`, mirroring the
//! proven spike configs.
//!
//! Real-mode sequencing quirk: tuwunel needs `server_name = <onion>`, but the
//! onion only exists after tor mints it. So we first write a placeholder
//! tuwunel.toml (so the file always exists), and re-render with the real
//! onion once `<data>/tor/hs/hostname` appears — only then is tuwunel started.

use std::path::PathBuf;
use tauri::AppHandle;

use crate::state::app_data_dir;

/// Homeserver listens here. 8118 deliberately avoids colliding with a dev
/// Synapse on 8008/8448.
pub const HOMESERVER_PORT: u16 = 8118;
/// Tor SOCKS port tuwunel uses for outbound federation.
pub const SOCKS_PORT: u16 = 9150;

pub struct Paths {
    pub config_dir: PathBuf,
    pub torrc: PathBuf,
    pub tuwunel_toml: PathBuf,
    pub tor_data: PathBuf,
    pub hs_dir: PathBuf,
    pub hostname_file: PathBuf,
    pub tuwunel_data: PathBuf,
}

pub fn paths(app: &AppHandle) -> Result<Paths, String> {
    let base = app_data_dir(app)?;
    let config_dir = base.join("config");
    let tor_data = base.join("data").join("tor");
    let hs_dir = tor_data.join("hs");
    Ok(Paths {
        torrc: config_dir.join("torrc"),
        tuwunel_toml: config_dir.join("tuwunel.toml"),
        hostname_file: hs_dir.join("hostname"),
        tuwunel_data: base.join("data").join("tuwunel"),
        config_dir,
        tor_data,
        hs_dir,
    })
}

#[cfg(unix)]
fn set_0700(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn set_0700(_path: &std::path::Path) {}

/// Create config/data directories. The hidden-service dir must be 0700 or
/// tor refuses to start.
pub fn ensure_dirs(app: &AppHandle) -> Result<Paths, String> {
    let p = paths(app)?;
    for dir in [&p.config_dir, &p.tor_data, &p.hs_dir, &p.tuwunel_data] {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
    }
    set_0700(&p.tor_data);
    set_0700(&p.hs_dir);
    Ok(p)
}

pub fn render_torrc(app: &AppHandle) -> Result<(), String> {
    let p = paths(app)?;
    let torrc = format!(
        "SocksPort {socks}\n\
         DataDirectory {data}\n\
         HiddenServiceDir {hs}\n\
         HiddenServicePort 8448 127.0.0.1:{hsport}\n\
         HiddenServicePort 8008 127.0.0.1:{hsport}\n",
        socks = SOCKS_PORT,
        data = p.tor_data.display(),
        hs = p.hs_dir.display(),
        hsport = HOMESERVER_PORT,
    );
    std::fs::write(&p.torrc, torrc).map_err(|e| format!("couldn't write torrc: {e}"))
}

/// Render tuwunel.toml with the given server_name (the onion, or a
/// placeholder before tor has minted one).
pub fn render_tuwunel(app: &AppHandle, server_name: &str) -> Result<(), String> {
    let p = paths(app)?;
    let toml = format!(
        "[global]\n\
         server_name = \"{server_name}\"\n\
         database_path = \"{db}\"\n\
         port = {port}\n\
         address = \"127.0.0.1\"\n\
         allow_federation = true\n\
         allow_invalid_tls_certificates = true\n\
         trusted_servers = []\n\
         \n\
         [global.proxy.global]\n\
         url = \"socks5h://127.0.0.1:{socks}\"\n",
        db = p.tuwunel_data.display(),
        port = HOMESERVER_PORT,
        socks = SOCKS_PORT,
    );
    std::fs::write(&p.tuwunel_toml, toml).map_err(|e| format!("couldn't write tuwunel.toml: {e}"))
}
