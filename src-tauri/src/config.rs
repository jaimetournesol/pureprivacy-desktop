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
/// Caddy fed-proxy: TLS-terminates inbound federation and enforces the paired-
/// peer allowlist (tuwunel has no allowlist of its own). tor maps the onion's
/// federation port 8448 here; Caddy reverse-proxies to the homeserver.
pub const FEDPROXY_PORT: u16 = 8449;

/// LiveKit SFU group-call sidecars (Element Call). All loopback; Tor maps the
/// well-known onion ports here. These mirror the v0.1 appliance exactly:
/// LiveKit is TCP-only (Tor carries no UDP) and the SFU URL handed to clients
/// is wss:// (Element Call refuses ws://), terminated by a second Caddy site.
/// LiveKit signaling WebSocket (loopback; Caddy wss site reverse-proxies here).
pub const LIVEKIT_WS_PORT: u16 = 7880;
/// LiveKit TCP media relay (Tor carries no UDP, so media rides TCP).
pub const LIVEKIT_TCP_PORT: u16 = 7881;
/// lk-jwt-service: validates a Matrix OpenID token and mints a LiveKit JWT.
pub const LKJWT_PORT: u16 = 8082;
/// Onion port for the wss-terminated SFU signaling path (handed to clients).
pub const LIVEKIT_WSS_ONION_PORT: u16 = 7443;
/// Caddy's loopback listener for the wss SFU site; tor maps 7443 here.
pub const CADDY_WSS_PORT: u16 = 7444;

pub struct Paths {
    pub config_dir: PathBuf,
    pub torrc: PathBuf,
    pub tuwunel_toml: PathBuf,
    pub turnserver_conf: PathBuf,
    pub caddyfile: PathBuf,
    pub livekit_yaml: PathBuf,
    pub fed_cert: PathBuf,
    pub fed_key: PathBuf,
    pub data_root: PathBuf,
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
        caddyfile: config_dir.join("Caddyfile"),
        livekit_yaml: config_dir.join("livekit.yaml"),
        fed_cert: config_dir.join("fed-cert.pem"),
        fed_key: config_dir.join("fed-key.pem"),
        hostname_file: hs_dir.join("hostname"),
        tuwunel_data: base.join("data").join("tuwunel"),
        data_root: base.clone(),
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

#[cfg(unix)]
fn set_0600(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_0600(_path: &std::path::Path) {}

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
/// Federation (8448) goes to the Caddy fed-proxy (TLS + allowlist); the client
/// API (8008) and TURN go straight to their services.
fn torrc_string(socks: u16, data: &str, hs: &str, hsport: u16, fedproxy: u16, voice: bool) -> String {
    // NoIsolateClientAddr: share circuits across local SOCKS clients.
    // G1 finding (2026-06-13): tor's default per-client isolation hands the
    // homeserver cold circuits, and first-contact federation then exceeds
    // its connect timeout.  See docs/redesign/2026-06-phase0-spike-results.md.
    let mut torrc = format!(
        "SocksPort {socks} NoIsolateClientAddr\n\
         DataDirectory {data}\n\
         HiddenServiceDir {hs}\n\
         HiddenServicePort 8448 127.0.0.1:{fedproxy}\n\
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
    // Group-call (Element Call / LiveKit) onion port map. Only published when the
    // LiveKit + lk-jwt sidecars are present; harmless to omit otherwise. Mirrors
    // the v0.1 appliance torrc: wss-terminated SFU signaling (Caddy wss site),
    // the TCP media relay, and the lk-jwt token service.
    if voice {
        let _ = writeln!(
            torrc,
            "HiddenServicePort {wss} 127.0.0.1:{caddy_wss}\n\
             HiddenServicePort {media} 127.0.0.1:{media}\n\
             HiddenServicePort {jwt} 127.0.0.1:{jwt}",
            wss = LIVEKIT_WSS_ONION_PORT,
            caddy_wss = CADDY_WSS_PORT,
            media = LIVEKIT_TCP_PORT,
            jwt = LKJWT_PORT,
        );
    }
    torrc
}

pub fn render_torrc(app: &AppHandle, voice: bool) -> Result<(), String> {
    let p = paths(app)?;
    let torrc = torrc_string(
        SOCKS_PORT,
        &p.tor_data.display().to_string(),
        &p.hs_dir.display().to_string(),
        HOMESERVER_PORT,
        FEDPROXY_PORT,
        voice,
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
    voice: bool,
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
    if voice && !server_name.ends_with("placeholder.onion") {
        // Group calls (Element Call / LiveKit). Advertising livekit_url makes
        // tuwunel publish org.matrix.msc4143.rtc_foci, which clients read to
        // discover the SFU + the lk-jwt token endpoint. client must be set
        // alongside it (tuwunel only emits the rtc_foci block when both are
        // present). lk-jwt is fetched over http://<onion>:8082 — Tor maps that
        // onion port to the loopback lk-jwt service.
        let _ = write!(
            toml,
            "\n[global.well_known]\n\
             client = \"http://{server_name}\"\n\
             livekit_url = \"http://{server_name}:{jwt}\"\n",
            jwt = LKJWT_PORT,
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
    voice: bool,
) -> Result<(), String> {
    let p = paths(app)?;
    let toml = tuwunel_toml_string(
        server_name,
        &p.tuwunel_data.display().to_string(),
        HOMESERVER_PORT,
        SOCKS_PORT,
        turn_secret,
        join_token,
        voice,
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
         # The co-located LiveKit SFU is a loopback peer (127.0.0.1): the relayed\n\
         # group-call media is forwarded to it locally. coturn forbids loopback\n\
         # peers by default (verified: 403 Forbidden IP), so allow them — safe on a\n\
         # single-user appliance where the only loopback peer is our own SFU and\n\
         # the relay still requires a valid auth-secret credential.\n\
         allow-loopback-peers\n\
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

/// Pure builder for the fed-proxy Caddyfile — unit-tested. Enforces the
/// paired-peer allowlist (Option B, verified): key-exchange/discovery is open
/// (peers need it before they can be allowlisted); the authenticated federation
/// API is allowed only when the X-Matrix `Authorization` origin is a paired
/// peer; everything else is 403. With NO peers, the `@paired` block is omitted
/// entirely, so all authenticated federation is refused.
fn caddyfile_string(
    caddy_port: u16,
    hs_port: u16,
    cert: &str,
    key: &str,
    peers: &[String],
    voice: bool,
) -> String {
    let mut s = format!(
        "{{\n\
         \tauto_https off\n\
         }}\n\
         https://:{caddy_port} {{\n\
         \ttls {cert} {key}\n\
         \t@open path /_matrix/key/* /_matrix/federation/v1/version /_matrix/federation/v1/openid/* /.well-known/*\n\
         \thandle @open {{\n\
         \t\treverse_proxy http://127.0.0.1:{hs_port}\n\
         \t}}\n"
    );
    if !peers.is_empty() {
        // origin="?(p1\.onion|p2\.onion)"?. The X-Matrix auth header's params
        // may be quoted OR unquoted per the Matrix spec (tuwunel sends them
        // unquoted) — so the surrounding quotes are OPTIONAL, or paired real
        // federation gets 403'd. (Live two-box test caught this, 2026-06-13.)
        // Onions are [a-z2-7]+.onion, so only the dot needs escaping. Do NOT
        // wrap the regex in backticks — that silently fails to match.
        let alt = peers
            .iter()
            .map(|o| o.replace('.', "\\."))
            .collect::<Vec<_>>()
            .join("|");
        let _ = write!(
            s,
            "\t@paired header_regexp Authorization origin=\"?({alt})\"?\n\
             \thandle @paired {{\n\
             \t\treverse_proxy http://127.0.0.1:{hs_port}\n\
             \t}}\n"
        );
    }
    s.push_str("\thandle {\n\t\trespond \"not a paired peer\" 403\n\t}\n}\n");
    if voice {
        // Second site: TLS-terminate the wss SFU signaling path and reverse-proxy
        // the WS upgrade to LiveKit. NOT allowlist-gated — call participants are
        // authed by the LiveKit JWT (minted by lk-jwt after validating their
        // Matrix OpenID token), not by federation origin. Caddy's reverse_proxy
        // handles the WebSocket upgrade automatically. Reuses the same onion
        // self-signed cert as the federation site. tor maps onion 7443 here.
        let _ = write!(
            s,
            "https://:{wss_port} {{\n\
             \ttls {cert} {key}\n\
             \treverse_proxy http://127.0.0.1:{livekit}\n\
             }}\n",
            wss_port = CADDY_WSS_PORT,
            cert = cert,
            key = key,
            livekit = LIVEKIT_WS_PORT,
        );
    }
    s
}

/// Render the fed-proxy Caddyfile from the current pairings.
pub fn render_caddyfile(app: &AppHandle, peers: &[String], voice: bool) -> Result<(), String> {
    let p = paths(app)?;
    let conf = caddyfile_string(
        FEDPROXY_PORT,
        HOMESERVER_PORT,
        &p.fed_cert.display().to_string(),
        &p.fed_key.display().to_string(),
        peers,
        voice,
    );
    std::fs::write(&p.caddyfile, conf).map_err(|e| format!("couldn't write Caddyfile: {e}"))
}

/// Mint a long-lived coturn REST credential (the `use-auth-secret` scheme):
/// `username = <unix-expiry>`, `credential = base64(HMAC-SHA1(secret, username))`.
/// coturn validates this without any stored user, so the LiveKit SFU can
/// authenticate to coturn with a *static* username/credential pair (LiveKit's
/// `turn_servers` config can't compute time-limited REST creds itself).
///
/// The expiry MUST fit in 32 bits: coturn parses the REST timestamp into a 32-bit
/// time and a value past 2^31 overflows → it silently treats the request as a
/// long-term-cred lookup, fails to find the user, and 401s. (Verified live: a
/// year-3000 expiry "Cannot complete Allocation"; 2147483647 authenticates.) So
/// we use the 32-bit max, 2147483647 = 2038-01-19 — ~12 years, far longer than
/// any appliance lifecycle, and never needs rotation in practice.
fn turn_rest_credential(secret: &str) -> (String, String) {
    use base64::Engine as _;
    use hmac::{Hmac, Mac};
    use sha1::Sha1;
    // 32-bit-max unix time (2038-01-19). A larger value overflows coturn's REST
    // parser and the credential is rejected — see the doc comment above.
    let username = "2147483647".to_string();
    let mut mac =
        Hmac::<Sha1>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(username.as_bytes());
    let credential = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    (username, credential)
}

/// Pure builder for livekit.yaml — unit-tested. Tor-only: TCP-only because Tor
/// carries no UDP; loopback/host ICE candidates leak + are unreachable over the
/// onion. The SFU's own embedded TURN stays off — instead we **force-relay all
/// media through the coturn-at-onion** by advertising it in `rtc.turn_servers`.
/// That's the proven media-over-Tor path: clients reach coturn over Tor (the
/// only Tor leg), and the SFU↔coturn leg is local. See
/// `docs/redesign/2026-06-media-over-tor.md` (TURN-relay-over-onion, 0% loss).
/// The api_key/secret pair is shared with lk-jwt so its JWTs are accepted; the
/// turn_secret is the same one coturn enforces (use-auth-secret).
fn livekit_yaml_string(
    ws_port: u16,
    tcp_port: u16,
    api_key: &str,
    api_secret: &str,
    onion: &str,
    turn_secret: &str,
) -> String {
    let mut s = format!(
        "# PurePrivacy LiveKit SFU config (generated, do not edit).\n\
         # Tor-only mode: TCP fallback only, since UDP cannot traverse a hidden service.\n\
         port: {ws_port}\n\
         bind_addresses:\n\
         \x20 - 127.0.0.1\n\
         \n\
         rtc:\n\
         \x20 tcp_port: {tcp_port}\n\
         \x20 # Force TCP relay so the WebRTC media path can ride Tor's hidden-service\n\
         \x20 # tunnel.  use_external_ip + loopback candidates are useless over Tor.\n\
         \x20 use_external_ip: false\n\
         \x20 enable_loopback_candidate: false\n"
    );
    // Advertise the coturn-at-onion to clients so they gather a *relay* candidate
    // (the only ICE candidate type that survives Tor). Plaintext `turn:` over TCP
    // is fine — the onion is the encryption layer. Only rendered with a real
    // onion + secret; a placeholder onion would hand clients a dead TURN URI.
    if !onion.ends_with("placeholder.onion") && !turn_secret.is_empty() {
        let (user, cred) = turn_rest_credential(turn_secret);
        let _ = write!(
            s,
            "\x20 turn_servers:\n\
             \x20   - host: {onion}\n\
             \x20     port: 3478\n\
             \x20     protocol: tcp\n\
             \x20     username: \"{user}\"\n\
             \x20     credential: \"{cred}\"\n"
        );
    }
    let _ = write!(
        s,
        "\n\
         keys:\n\
         \x20 {api_key}: {api_secret}\n\
         \n\
         logging:\n\
         \x20 level: info\n\
         \x20 json: false\n\
         \n\
         turn:\n\
         \x20 enabled: false\n"
    );
    s
}

/// Render livekit.yaml for the group-call SFU sidecar. Needs the shared
/// api_key/secret (generated at setup, persisted with the other secrets), plus
/// the onion + turn_secret so the SFU force-relays media through coturn-at-onion.
pub fn render_livekit_yaml(
    app: &AppHandle,
    api_key: &str,
    api_secret: &str,
    onion: &str,
    turn_secret: &str,
) -> Result<(), String> {
    let p = paths(app)?;
    let conf = livekit_yaml_string(
        LIVEKIT_WS_PORT,
        LIVEKIT_TCP_PORT,
        api_key,
        api_secret,
        onion,
        turn_secret,
    );
    std::fs::write(&p.livekit_yaml, conf).map_err(|e| format!("couldn't write livekit.yaml: {e}"))
}

/// Mint the fed-proxy's self-signed TLS cert (CN = the onion). Peers accept it
/// because they federate with `allow_invalid_tls_certificates` (onion-only).
/// Idempotent: only generates if the cert is missing.
pub fn ensure_fed_cert(app: &AppHandle, onion: &str) -> Result<(), String> {
    let p = paths(app)?;
    if p.fed_cert.is_file() && p.fed_key.is_file() {
        return Ok(());
    }
    let certified = rcgen::generate_simple_self_signed(vec![onion.to_string()])
        .map_err(|e| format!("couldn't mint federation cert: {e}"))?;
    std::fs::write(&p.fed_cert, certified.cert.pem())
        .map_err(|e| format!("couldn't write fed cert: {e}"))?;
    std::fs::write(&p.fed_key, certified.key_pair.serialize_pem())
        .map_err(|e| format!("couldn't write fed key: {e}"))?;
    set_0600(&p.fed_key);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relay_count() -> usize {
        (TURN_RELAY_PORT_MAX - TURN_RELAY_PORT_MIN + 1) as usize
    }

    #[test]
    fn torrc_publishes_every_relay_port_plus_the_fixed_ones() {
        let torrc = torrc_string(9150, "/d", "/d/hs", 8118, 8449, false);
        let lines = torrc.matches("HiddenServicePort").count();
        // 8448 + 8008 + 3478 + 5349 fixed, plus one per relay port.
        assert_eq!(lines, 4 + relay_count());
        assert!(torrc.contains("SocksPort 9150 NoIsolateClientAddr"));
        // Federation (8448) goes to the fed-proxy; client API (8008) to tuwunel.
        assert!(torrc.contains("HiddenServicePort 8448 127.0.0.1:8449"));
        assert!(torrc.contains("HiddenServicePort 8008 127.0.0.1:8118"));
        // The coturn relay range and torrc MUST agree port-for-port.
        for p in [TURN_RELAY_PORT_MIN, TURN_RELAY_PORT_MAX] {
            assert!(torrc.contains(&format!("HiddenServicePort {p} 127.0.0.1:{p}")));
        }
    }

    #[test]
    fn torrc_maps_group_call_ports_only_when_voice_enabled() {
        // voice=true publishes the wss (7443→caddy 7444), media (7881), and
        // lk-jwt (8082) onion ports on top of the base map.
        let with = torrc_string(9150, "/d", "/d/hs", 8118, 8449, true);
        assert!(with.contains(&format!(
            "HiddenServicePort {LIVEKIT_WSS_ONION_PORT} 127.0.0.1:{CADDY_WSS_PORT}"
        )));
        assert!(with.contains(&format!(
            "HiddenServicePort {LIVEKIT_TCP_PORT} 127.0.0.1:{LIVEKIT_TCP_PORT}"
        )));
        assert!(with.contains(&format!("HiddenServicePort {LKJWT_PORT} 127.0.0.1:{LKJWT_PORT}")));

        // voice=false omits all three.
        let without = torrc_string(9150, "/d", "/d/hs", 8118, 8449, false);
        assert!(!without.contains(&format!("127.0.0.1:{CADDY_WSS_PORT}")));
        assert!(!without.contains(&format!("HiddenServicePort {LIVEKIT_TCP_PORT}")));
        assert!(!without.contains(&format!("HiddenServicePort {LKJWT_PORT}")));
    }

    #[test]
    fn caddyfile_allowlists_paired_peers_only() {
        let peers = vec!["aaa.onion".to_string(), "bbb.onion".to_string()];
        let cf = caddyfile_string(8449, 8118, "/c.pem", "/k.pem", &peers, false);
        // open endpoints + allowlist + catch-all 403
        assert!(cf.contains("@open path /_matrix/key/*"));
        // openid/userinfo MUST be open — lk-jwt's cross-box call validation hits
        // it with no X-Matrix origin header, so @paired can't match it.
        assert!(cf.contains("/_matrix/federation/v1/openid/*"));
        assert!(cf.contains(r#"@paired header_regexp Authorization origin="?(aaa\.onion|bbb\.onion)"?"#));
        assert!(cf.contains("respond \"not a paired peer\" 403"));
        assert!(cf.contains("reverse_proxy http://127.0.0.1:8118"));

        // No peers → NO @paired block → all authed federation refused.
        let none = caddyfile_string(8449, 8118, "/c.pem", "/k.pem", &[], false);
        assert!(!none.contains("@paired"));
        assert!(none.contains("respond \"not a paired peer\" 403"));
    }

    #[test]
    fn caddyfile_adds_wss_sfu_site_only_when_voice_enabled() {
        // voice=true appends a SECOND site on :7444 reverse-proxying LiveKit's
        // signaling WS (:7880). It is NOT allowlist-gated (JWT-authed).
        let cf = caddyfile_string(8449, 8118, "/c.pem", "/k.pem", &[], true);
        assert!(cf.contains(&format!("https://:{CADDY_WSS_PORT} {{")));
        assert!(cf.contains(&format!("reverse_proxy http://127.0.0.1:{LIVEKIT_WS_PORT}")));
        // The wss site still keeps the federation site intact below it.
        assert!(cf.contains(&format!("https://:{FEDPROXY_PORT} {{")));

        // voice=false → no wss site at all.
        let without = caddyfile_string(8449, 8118, "/c.pem", "/k.pem", &[], false);
        assert!(!without.contains(&format!("https://:{CADDY_WSS_PORT}")));
        assert!(!without.contains(&format!("reverse_proxy http://127.0.0.1:{LIVEKIT_WS_PORT}")));
    }

    #[test]
    fn livekit_yaml_is_tcp_only_with_the_shared_keys() {
        let yaml = livekit_yaml_string(7880, 7881, "lkkey", "lksecret", "abc123.onion", "deadbeef");
        // TCP media port + no external IP (Tor carries no UDP) — KEEP from v0.1.
        assert!(yaml.contains("tcp_port: 7881"));
        assert!(yaml.contains("use_external_ip: false"));
        assert!(yaml.contains("enable_loopback_candidate: false"));
        // The shared api_key: api_secret pair lk-jwt also signs with.
        assert!(yaml.contains("lkkey: lksecret"));
        // Built-in TURN stays off — we relay over Tor, not LiveKit's TURN.
        assert!(yaml.contains("enabled: false"));
        assert!(yaml.contains("port: 7880"));
        // Force-relay: the onion coturn is advertised so clients gather a relay
        // candidate (the only ICE type that survives Tor).
        assert!(yaml.contains("turn_servers:"));
        assert!(yaml.contains("host: abc123.onion"));
        assert!(yaml.contains("protocol: tcp"));
    }

    #[test]
    fn livekit_turn_servers_omitted_without_real_onion_or_secret() {
        // Placeholder onion (pre-mint) → no dead TURN URI handed to clients.
        let pre = livekit_yaml_string(7880, 7881, "k", "s", "placeholder.onion", "deadbeef");
        assert!(!pre.contains("turn_servers:"));
        // No secret yet → no TURN block.
        let nosec = livekit_yaml_string(7880, 7881, "k", "s", "abc123.onion", "");
        assert!(!nosec.contains("turn_servers:"));
    }

    #[test]
    fn turn_rest_credential_is_deterministic_hmac() {
        // Same secret → same long-lived credential (so reboots don't churn it),
        // and the username is the far-future expiry coturn validates against.
        let (u1, c1) = turn_rest_credential("deadbeef");
        let (u2, c2) = turn_rest_credential("deadbeef");
        // 32-bit-max expiry — a larger value overflows coturn's REST parser.
        assert_eq!(u1, "2147483647");
        assert_eq!((u1, c1.clone()), (u2, c2));
        // Different secret → different credential.
        let (_, c3) = turn_rest_credential("other");
        assert_ne!(c1, c3);
        assert!(!c1.is_empty());
    }

    #[test]
    fn tuwunel_advertises_turn_only_with_secret_and_real_onion() {
        let onion = "abc123.onion";
        let with = tuwunel_toml_string(onion, "/db", 8118, 9150, "deadbeef", "jointok", false);
        assert!(with.contains("turn_uris = [\"turn:abc123.onion:3478?transport=tcp\"]"));
        assert!(with.contains("turn_secret = \"deadbeef\""));
        assert!(with.contains("socks5h://127.0.0.1:9150"));

        // No secret yet → no turn block.
        let without = tuwunel_toml_string(onion, "/db", 8118, 9150, "", "jointok", false);
        assert!(!without.contains("turn_uris"));

        // Placeholder server_name (pre-mint) → never advertise turn.
        let placeholder = tuwunel_toml_string("placeholder.onion", "/db", 8118, 9150, "deadbeef", "jointok", false);
        assert!(!placeholder.contains("turn_uris"));
    }

    #[test]
    fn tuwunel_advertises_well_known_livekit_only_with_voice_and_real_onion() {
        let onion = "abc123.onion";
        // voice=true + real onion → well_known with client + livekit_url.
        let with = tuwunel_toml_string(onion, "/db", 8118, 9150, "deadbeef", "jointok", true);
        assert!(with.contains("[global.well_known]"));
        assert!(with.contains("client = \"http://abc123.onion\""));
        assert!(with.contains(&format!("livekit_url = \"http://abc123.onion:{LKJWT_PORT}\"")));

        // voice=false → no well_known block.
        let no_voice = tuwunel_toml_string(onion, "/db", 8118, 9150, "deadbeef", "jointok", false);
        assert!(!no_voice.contains("[global.well_known]"));
        assert!(!no_voice.contains("livekit_url"));

        // Placeholder onion (pre-mint) → never advertise even with voice=true.
        let placeholder =
            tuwunel_toml_string("placeholder.onion", "/db", 8118, 9150, "deadbeef", "jointok", true);
        assert!(!placeholder.contains("[global.well_known]"));
        assert!(!placeholder.contains("livekit_url"));
    }

    #[test]
    fn tuwunel_gates_registration_on_a_token_never_open() {
        let with = tuwunel_toml_string("abc.onion", "/db", 8118, 9150, "", "jointok123", false);
        assert!(with.contains("allow_registration = true"));
        assert!(with.contains("registration_token = \"jointok123\""));

        // No token (e.g. pre-setup placeholder) → registration stays absent
        // (tuwunel defaults registration OFF), never an open-reg server.
        let without = tuwunel_toml_string("placeholder.onion", "/db", 8118, 9150, "", "", false);
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
        // Co-located SFU is a loopback peer — must be permitted.
        assert!(conf.contains("allow-loopback-peers"));
        assert!(conf.contains(&format!("min-port={TURN_RELAY_PORT_MIN}")));
        assert!(conf.contains(&format!("max-port={TURN_RELAY_PORT_MAX}")));
    }
}
