//! Renders tuwunel.toml + torrc + turnserver.conf into `<app_data_dir>/config`,
//! mirroring the proven spike / v0.1 appliance configs.
//!
//! Real-mode sequencing quirk: tuwunel needs `server_name = <onion>`, and
//! coturn needs the onion for `realm`/`external-ip` — but the onion only
//! exists after tor mints it. So we first write a placeholder tuwunel.toml
//! (so the file always exists), and re-render with the real onion once
//! `<data>/tor/hs/hostname` appears — only then are tuwunel + coturn started.

use std::fmt::Write as _;
use std::path::PathBuf;
use tauri::AppHandle;

use crate::state::app_data_dir;

/// Homeserver listens here. 8118 deliberately avoids colliding with a dev
/// Synapse on 8008/8448.
pub const HOMESERVER_PORT: u16 = 8118;
/// Tor SOCKS port tuwunel uses for outbound federation.
pub const SOCKS_PORT: u16 = 9150;
/// coturn's loopback listener. Tor maps the well-known onion ports 3478 and
/// 5349 here; 3479 locally avoids colliding with a system coturn on 3478.
pub const TURN_PORT: u16 = 3479;
/// coturn TCP relay range. Each active TURN allocation pins one port for
/// ~15 minutes, so 40 ports ≈ 40 concurrent 1:1 calls — the same sizing the
/// v0.1 appliance proved out. v0.1 used 49152-49191 inside a dedicated
/// container netns; on a desktop host that sits inside Linux's default
/// ephemeral range (32768-60999), so we move just above it to keep loopback
/// relay binds from racing the kernel's source-port allocator.
///
/// THIS RANGE MUST EXACTLY MATCH the HiddenServicePort lines in torrc — tor
/// cannot wildcard-map a port range, so every port needs an explicit line.
/// Both renders below derive from these two constants, so they agree by
/// construction; if you change them, both files re-render together.
pub const TURN_RELAY_PORT_MIN: u16 = 61000;
pub const TURN_RELAY_PORT_MAX: u16 = 61039;

pub struct Paths {
    pub config_dir: PathBuf,
    pub torrc: PathBuf,
    pub tuwunel_toml: PathBuf,
    pub turnserver_conf: PathBuf,
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
        turnserver_conf: config_dir.join("turnserver.conf"),
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

/// Pure builder for torrc — unit-tested; `render_torrc` just adds paths + I/O.
fn torrc_string(socks: u16, data: &str, hs: &str, hsport: u16) -> String {
    // NoIsolateClientAddr: share circuits across local SOCKS clients.
    // G1 finding (2026-06-13): tor's default per-client isolation hands the
    // homeserver cold circuits, and first-contact federation then exceeds
    // its connect timeout.  See docs/redesign/2026-06-phase0-spike-results.md.
    let mut torrc = format!(
        "SocksPort {socks} NoIsolateClientAddr\n\
         DataDirectory {data}\n\
         HiddenServiceDir {hs}\n\
         HiddenServicePort 8448 127.0.0.1:{hsport}\n\
         HiddenServicePort 8008 127.0.0.1:{hsport}\n\
         HiddenServicePort 3478 127.0.0.1:{turn}\n\
         HiddenServicePort 5349 127.0.0.1:{turn}\n",
        turn = TURN_PORT,
    );
    // Tor cannot wildcard-map a port RANGE, so the coturn TCP relay range
    // needs one explicit HiddenServicePort line per port. Same constants the
    // turnserver.conf min/max-port derive from, so the two files always agree.
    for port in TURN_RELAY_PORT_MIN..=TURN_RELAY_PORT_MAX {
        let _ = writeln!(torrc, "HiddenServicePort {port} 127.0.0.1:{port}");
    }
    torrc
}

pub fn render_torrc(app: &AppHandle) -> Result<(), String> {
    let p = paths(app)?;
    let torrc = torrc_string(
        SOCKS_PORT,
        &p.tor_data.display().to_string(),
        &p.hs_dir.display().to_string(),
        HOMESERVER_PORT,
    );
    std::fs::write(&p.torrc, torrc).map_err(|e| format!("couldn't write torrc: {e}"))
}

/// Render tuwunel.toml with the given server_name (the onion, or a
/// placeholder before tor has minted one). When `turn_secret` is non-empty and
/// the server_name is the real onion, advertise the 1:1 voice TURN server —
/// tuwunel signs short-lived credentials with the shared secret and hands them
/// to clients via /_matrix/client/v3/voip/turnServer (spike-verified wired).
/// Pure builder for tuwunel.toml — unit-tested.
fn tuwunel_toml_string(
    server_name: &str,
    db: &str,
    port: u16,
    socks: u16,
    turn_secret: &str,
    join_token: &str,
) -> String {
    let mut toml = format!(
        "[global]\n\
         server_name = \"{server_name}\"\n\
         database_path = \"{db}\"\n\
         port = {port}\n\
         address = \"127.0.0.1\"\n\
         allow_federation = true\n\
         allow_invalid_tls_certificates = true\n\
         trusted_servers = []\n\
         query_trusted_key_servers_first = false\n\
         # Cold onion circuits legitimately take tens of seconds on first\n\
         # contact — G1-proven values (2026-06-13).\n\
         request_conn_timeout = 90\n\
         request_total_timeout = 320\n\
         sender_timeout = 300\n\
         well_known_conn_timeout = 30\n\
         well_known_timeout = 60\n"
    );
    if !join_token.is_empty() {
        // Registration is token-gated, never open. The app creates the admin
        // (first user => auto-admin) with this token, and the owner shares it
        // to add more people.
        let _ = write!(
            toml,
            "allow_registration = true\n\
             registration_token = \"{join_token}\"\n"
        );
    }
    if !turn_secret.is_empty() && !server_name.ends_with("placeholder.onion") {
        // turn:<onion>:3478 — tor maps that onion port to the loopback coturn.
        // TCP transport only: Tor carries no UDP. (onion-purist: 1:1 voice
        // rides Tor, best-effort. Same-box calls only; cross-install voice is
        // the Element Call / LiveKit path.)
        let _ = write!(
            toml,
            "turn_uris = [\"turn:{server_name}:3478?transport=tcp\"]\n\
             turn_secret = \"{turn_secret}\"\n\
             turn_ttl = 86400\n"
        );
    }
    let _ = write!(
        toml,
        "\n[global.proxy.global]\n\
         url = \"socks5h://127.0.0.1:{socks}\"\n"
    );
    toml
}

/// Render tuwunel.toml with the given server_name (the onion, or a
/// placeholder before tor has minted one). When `turn_secret` is non-empty and
/// the server_name is the real onion, advertise the 1:1 voice TURN server —
/// tuwunel signs short-lived credentials with the shared secret and hands them
/// to clients via /_matrix/client/v3/voip/turnServer (spike-verified wired).
pub fn render_tuwunel(
    app: &AppHandle,
    server_name: &str,
    turn_secret: &str,
    join_token: &str,
) -> Result<(), String> {
    let p = paths(app)?;
    let toml = tuwunel_toml_string(
        server_name,
        &p.tuwunel_data.display().to_string(),
        HOMESERVER_PORT,
        SOCKS_PORT,
        turn_secret,
        join_token,
    );
    std::fs::write(&p.tuwunel_toml, toml).map_err(|e| format!("couldn't write tuwunel.toml: {e}"))
}

/// Render turnserver.conf for the 1:1-voice coturn sidecar, mirroring the
/// v0.1 appliance's TCP-only-over-Tor config. Needs the minted onion (for
/// `realm`/`external-ip`) and the shared auth secret.
///
/// `min-port`/`max-port` MUST equal the relay range published in torrc — both
/// derive from the same constants, so they agree by construction.
/// Pure builder for turnserver.conf — unit-tested.
fn turnserver_conf_string(onion: &str, secret: &str) -> String {
    format!(
        "# PurePrivacy coturn — TCP-only relay over Tor (generated, do not edit).\n\
         listening-port={turn}\n\
         min-port={min}\n\
         max-port={max}\n\
         # No public IP: advertise the .onion so client SDP carries the right host.\n\
         external-ip={onion}\n\
         realm={onion}\n\
         use-auth-secret\n\
         static-auth-secret={secret}\n\
         # Tor carries no UDP, so refuse UDP on the client leg. Same-box calls\n\
         # forward relay->relay on loopback and never ask Tor to carry UDP.\n\
         no-udp\n\
         no-multicast-peers\n\
         no-cli\n\
         no-stdout-log\n\
         fingerprint\n\
         total-quota=200\n\
         user-quota=20\n\
         log-file=stdout\n",
        turn = TURN_PORT,
        min = TURN_RELAY_PORT_MIN,
        max = TURN_RELAY_PORT_MAX,
    )
}

pub fn render_turnserver(app: &AppHandle, onion: &str, turn_secret: &str) -> Result<(), String> {
    let p = paths(app)?;
    let conf = turnserver_conf_string(onion, turn_secret);
    std::fs::write(&p.turnserver_conf, conf)
        .map_err(|e| format!("couldn't write turnserver.conf: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relay_count() -> usize {
        (TURN_RELAY_PORT_MAX - TURN_RELAY_PORT_MIN + 1) as usize
    }

    #[test]
    fn torrc_publishes_every_relay_port_plus_the_fixed_ones() {
        let torrc = torrc_string(9150, "/d", "/d/hs", 8118);
        let lines = torrc.matches("HiddenServicePort").count();
        // 8448 + 8008 + 3478 + 5349 fixed, plus one per relay port.
        assert_eq!(lines, 4 + relay_count());
        assert!(torrc.contains("SocksPort 9150 NoIsolateClientAddr"));
        // The coturn relay range and torrc MUST agree port-for-port.
        for p in [TURN_RELAY_PORT_MIN, TURN_RELAY_PORT_MAX] {
            assert!(torrc.contains(&format!("HiddenServicePort {p} 127.0.0.1:{p}")));
        }
    }

    #[test]
    fn tuwunel_advertises_turn_only_with_secret_and_real_onion() {
        let onion = "abc123.onion";
        let with = tuwunel_toml_string(onion, "/db", 8118, 9150, "deadbeef", "jointok");
        assert!(with.contains("turn_uris = [\"turn:abc123.onion:3478?transport=tcp\"]"));
        assert!(with.contains("turn_secret = \"deadbeef\""));
        assert!(with.contains("socks5h://127.0.0.1:9150"));

        // No secret yet → no turn block.
        let without = tuwunel_toml_string(onion, "/db", 8118, 9150, "", "jointok");
        assert!(!without.contains("turn_uris"));

        // Placeholder server_name (pre-mint) → never advertise turn.
        let placeholder = tuwunel_toml_string("placeholder.onion", "/db", 8118, 9150, "deadbeef", "jointok");
        assert!(!placeholder.contains("turn_uris"));
    }

    #[test]
    fn tuwunel_gates_registration_on_a_token_never_open() {
        let with = tuwunel_toml_string("abc.onion", "/db", 8118, 9150, "", "jointok123");
        assert!(with.contains("allow_registration = true"));
        assert!(with.contains("registration_token = \"jointok123\""));

        // No token (e.g. pre-setup placeholder) → registration stays absent
        // (tuwunel defaults registration OFF), never an open-reg server.
        let without = tuwunel_toml_string("placeholder.onion", "/db", 8118, 9150, "", "");
        assert!(!without.contains("allow_registration"));
        assert!(!without.contains("registration_token"));
    }

    #[test]
    fn turnserver_conf_scopes_to_the_onion_and_refuses_udp() {
        let conf = turnserver_conf_string("abc123.onion", "s3cr3t");
        assert!(conf.contains("realm=abc123.onion"));
        assert!(conf.contains("external-ip=abc123.onion"));
        assert!(conf.contains("static-auth-secret=s3cr3t"));
        assert!(conf.contains("no-udp"));
        assert!(conf.contains(&format!("min-port={TURN_RELAY_PORT_MIN}")));
        assert!(conf.contains(&format!("max-port={TURN_RELAY_PORT_MAX}")));
    }
}
