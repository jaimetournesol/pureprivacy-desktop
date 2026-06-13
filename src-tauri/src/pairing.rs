//! Federation pairing: the QR-exchanged "pair codes" + the allowlist of peers
//! we federate with. This is PurePrivacy's "only talk to boxes you've paired
//! with" model.
//!
//! tuwunel has no federation allowlist of its own (only a global on/off + a
//! denylist), so enforcement lives one layer up in the Caddy fed-proxy:
//! `config::render_caddyfile` turns this list into an `Authorization`-origin
//! allowlist (verified — see docs/redesign/2026-06-13-desktop-build-findings.md).
//!
//! Pairings persist as `<data_dir>/pairings.json`. A pair code is a base64 JSON
//! blob carrying the minting box's onion + a 15-minute expiry + a nonce; the
//! operator reads it off a screen they control (trust root = their eyeballs),
//! mirroring the v0.1 appliance.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};

const CODE_TTL_SECS: u64 = 15 * 60;

#[derive(Serialize, Deserialize, Clone)]
pub struct Pairing {
    pub onion: String,
    pub added_at: u64,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Pairings {
    pub peers: Vec<Pairing>,
}

#[derive(Serialize, Deserialize)]
struct PairCode {
    version: u8,
    onion: String,
    expires_at: u64,
    nonce: String,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

pub fn pairings_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("pairings.json")
}

pub fn load(data_dir: &std::path::Path) -> Pairings {
    std::fs::read_to_string(pairings_path(data_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(data_dir: &std::path::Path, p: &Pairings) -> Result<(), String> {
    let json = serde_json::to_string_pretty(p).map_err(|e| e.to_string())?;
    std::fs::write(pairings_path(data_dir), json)
        .map_err(|e| format!("couldn't save pairings: {e}"))
}

/// Mint a short-lived pair code carrying our own onion, for the peer to accept.
pub fn mint_code(my_onion: &str, nonce_hex: &str) -> Result<String, String> {
    let code = PairCode {
        version: 1,
        onion: my_onion.to_string(),
        expires_at: now() + CODE_TTL_SECS,
        nonce: nonce_hex.to_string(),
    };
    let json = serde_json::to_vec(&code).map_err(|e| e.to_string())?;
    Ok(b64().encode(json))
}

/// Parse + validate a peer's pair code; returns their onion.
pub fn parse_code(code: &str) -> Result<String, String> {
    let raw = b64()
        .decode(code.trim())
        .map_err(|_| "That doesn't look like a valid pair code.".to_string())?;
    let parsed: PairCode = serde_json::from_slice(&raw)
        .map_err(|_| "That pair code is malformed.".to_string())?;
    if parsed.version != 1 {
        return Err("That pair code is from an incompatible version.".into());
    }
    if now() > parsed.expires_at {
        return Err("That pair code has expired — ask for a fresh one (codes last 15 minutes).".into());
    }
    if !parsed.onion.ends_with(".onion") {
        return Err("That pair code doesn't contain a valid address.".into());
    }
    Ok(parsed.onion)
}

/// Add a peer (idempotent).
pub fn add(data_dir: &std::path::Path, onion: &str) -> Result<(), String> {
    let mut p = load(data_dir);
    if !p.peers.iter().any(|x| x.onion == onion) {
        p.peers.push(Pairing { onion: onion.to_string(), added_at: now() });
        save(data_dir, &p)?;
    }
    Ok(())
}

/// Remove a peer (idempotent).
pub fn remove(data_dir: &std::path::Path, onion: &str) -> Result<(), String> {
    let mut p = load(data_dir);
    let before = p.peers.len();
    p.peers.retain(|x| x.onion != onion);
    if p.peers.len() != before {
        save(data_dir, &p)?;
    }
    Ok(())
}

pub fn onions(data_dir: &std::path::Path) -> Vec<String> {
    load(data_dir).peers.into_iter().map(|x| x.onion).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_round_trips() {
        let code = mint_code("abc123def456.onion", "deadbeef").unwrap();
        assert_eq!(parse_code(&code).unwrap(), "abc123def456.onion");
    }

    #[test]
    fn rejects_garbage_and_non_onion() {
        assert!(parse_code("not-base64!!!").is_err());
        let bad = b64().encode(br#"{"version":1,"onion":"evil.com","expires_at":99999999999,"nonce":"x"}"#);
        assert!(parse_code(&bad).is_err());
    }

    #[test]
    fn rejects_expired() {
        let raw = serde_json::to_vec(&PairCode {
            version: 1,
            onion: "x.onion".into(),
            expires_at: 1, // long past
            nonce: "n".into(),
        })
        .unwrap();
        assert!(parse_code(&b64().encode(raw)).is_err());
    }
}
