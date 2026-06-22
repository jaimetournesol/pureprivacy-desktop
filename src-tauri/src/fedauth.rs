//! Loopback federation-allowlist validator (review item W3-T1).
//!
//! Caddy `forward_auth`s every authenticated federation request to us. We parse
//! the `X-Matrix` Authorization header per the Matrix spec, pull out the CANONICAL
//! `origin` param value, and 200/403 it against the live `pairings.json` allowlist.
//!
//! This replaces the old `@paired header_regexp Authorization origin="?(...)"?`
//! matcher, which substring-matched the WHOLE header: an unpaired box that knew a
//! paired peer's (non-secret, QR-exchanged) onion could bury `origin=<paired>` in
//! a junk/sig param and slip past the gate. Proper param parsing returns the real
//! `origin` value only, so that bypass is closed. tuwunel still re-validates the
//! request signature downstream, so this is purely the origin allowlist gate.

use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const RESP_OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESP_DENY: &[u8] =
    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// Handle one `forward_auth` subrequest: read the request headers (Caddy sends a
/// GET, no body), decide allow/deny from the Authorization origin vs the live
/// allowlist, and write a minimal 200/403. Never propagates errors as a panic —
/// any read/parse failure fails CLOSED (deny).
pub async fn handle_conn(sock: &mut TcpStream, data_dir: &Path) {
    let allowed = decide(sock, data_dir).await.unwrap_or(false);
    let _ = sock
        .write_all(if allowed { RESP_OK } else { RESP_DENY })
        .await;
    let _ = sock.flush().await;
}

async fn decide(sock: &mut TcpStream, data_dir: &Path) -> std::io::Result<bool> {
    // Read until end-of-headers. forward_auth sends a GET, so there's no body.
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut tmp = [0u8; 2048];
    loop {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if find_subslice(&buf, b"\r\n\r\n").is_some() || buf.len() > MAX_HEADER_BYTES {
            break;
        }
    }
    let req = String::from_utf8_lossy(&buf);
    let Some(origin) = find_auth_header(&req).and_then(extract_origin) else {
        return Ok(false);
    };
    // Exact match against the live allowlist (re-read per request, so a pairing
    // change applies immediately — no Caddy reload needed).
    let allowed = crate::pairing::onions(data_dir).iter().any(|p| *p == origin);
    Ok(allowed)
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Find the value of the `Authorization` request header (case-insensitive name).
fn find_auth_header(req: &str) -> Option<&str> {
    for line in req.split("\r\n").skip(1) {
        if line.is_empty() {
            break; // end of headers
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("authorization") {
                return Some(value.trim());
            }
        }
    }
    None
}

/// Extract the canonical `origin` parameter from an `X-Matrix` Authorization
/// header value, honoring quoted values and ignoring `origin=` substrings buried
/// inside other params. Returns `None` if the scheme isn't X-Matrix or there's no
/// `origin` param.
pub fn extract_origin(auth: &str) -> Option<String> {
    let auth = auth.trim();
    if auth.len() < 8 || !auth[..8].eq_ignore_ascii_case("X-Matrix") {
        return None;
    }
    let params = auth[8..].trim_start();
    for param in split_params(params) {
        if let Some((key, val)) = param.split_once('=') {
            if key.trim().eq_ignore_ascii_case("origin") {
                let v = unquote(val.trim());
                if v.is_empty() {
                    return None;
                }
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Split a comma-separated param list, treating commas inside double quotes as
/// literal so a value (e.g. a base64 `sig`) can't be split mid-token.
fn split_params(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                cur.push(c);
            }
            ',' if !in_quotes => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn unquote(v: &str) -> &str {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PAIRED: &str = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwx.onion";
    const ATTACKER: &str = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz.onion";

    #[test]
    fn quoted_origin() {
        let h = format!("X-Matrix origin=\"{PAIRED}\",key=\"ed25519:1\",sig=\"abc==\"");
        assert_eq!(extract_origin(&h).as_deref(), Some(PAIRED));
    }

    #[test]
    fn unquoted_origin() {
        // tuwunel emits params unquoted.
        let h = format!("X-Matrix origin={PAIRED},key=ed25519:1,sig=abc");
        assert_eq!(extract_origin(&h).as_deref(), Some(PAIRED));
    }

    #[test]
    fn origin_not_first() {
        let h = format!("X-Matrix key=\"ed25519:1\",sig=\"abc==\",origin=\"{PAIRED}\"");
        assert_eq!(extract_origin(&h).as_deref(), Some(PAIRED));
    }

    #[test]
    fn case_insensitive_scheme() {
        let h = format!("x-matrix origin={PAIRED}");
        assert_eq!(extract_origin(&h).as_deref(), Some(PAIRED));
    }

    #[test]
    fn bypass_substring_in_sig_is_ignored() {
        // THE bug this whole module exists to kill: the real origin is the
        // attacker's; a paired onion is buried inside the quoted sig param.
        let h = format!(
            "X-Matrix origin=\"{ATTACKER}\",destination=\"x\",key=\"ed25519:1\",sig=\"zzorigin={PAIRED}zz\""
        );
        assert_eq!(extract_origin(&h).as_deref(), Some(ATTACKER));
        // ...so a gate that exact-matches the extracted origin against {PAIRED}
        // would reject it, whereas the old substring regex would have matched.
        assert_ne!(extract_origin(&h).as_deref(), Some(PAIRED));
    }

    #[test]
    fn no_origin_param() {
        assert_eq!(extract_origin("X-Matrix key=\"ed25519:1\",sig=\"abc\""), None);
    }

    #[test]
    fn not_x_matrix_scheme() {
        assert_eq!(extract_origin("Bearer sometoken"), None);
        assert_eq!(extract_origin(""), None);
    }

    #[test]
    fn empty_origin_value() {
        assert_eq!(extract_origin("X-Matrix origin=\"\",key=\"k\""), None);
    }

    #[test]
    fn finds_authorization_header_case_insensitively() {
        let req = "GET /check HTTP/1.1\r\nHost: x\r\nauthorization: X-Matrix origin=a.onion\r\n\r\n";
        assert_eq!(find_auth_header(req), Some("X-Matrix origin=a.onion"));
    }

    // --- socket-level integration: drive handle_conn over a real loopback TCP
    // connection (what Caddy's forward_auth does), reading pairings.json from a
    // temp dir. No box, no Caddy, no GUI window. ---

    fn unique_tmp() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "pp_fedauth_test_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ))
    }

    async fn gate(data_dir: &std::path::Path, auth: Option<&str>) -> u16 {
        use tokio::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let dir = data_dir.to_path_buf();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            handle_conn(&mut sock, &dir).await;
        });
        let mut client = TcpStream::connect(addr).await.unwrap();
        let mut req = String::from("GET /check HTTP/1.1\r\nHost: x\r\n");
        if let Some(a) = auth {
            req.push_str(&format!("Authorization: {a}\r\n"));
        }
        req.push_str("\r\n");
        client.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        let _ = client.read_to_end(&mut buf).await; // server sends Connection: close
        server.await.unwrap();
        String::from_utf8_lossy(&buf)
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn gate_allows_paired_and_denies_the_rest() {
        let dir = unique_tmp();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pairings.json"),
            format!("{{\"peers\":[{{\"onion\":\"{PAIRED}\",\"added_at\":1}}]}}"),
        )
        .unwrap();

        // paired origin -> allowed (proxied on to tuwunel)
        assert_eq!(gate(&dir, Some(&format!("X-Matrix origin=\"{PAIRED}\",sig=\"x\""))).await, 200);
        // unpaired origin -> denied
        assert_eq!(gate(&dir, Some(&format!("X-Matrix origin=\"{ATTACKER}\",sig=\"x\""))).await, 403);
        // THE bypass: real origin unpaired, paired onion buried in the sig -> denied
        assert_eq!(
            gate(&dir, Some(&format!("X-Matrix origin=\"{ATTACKER}\",sig=\"zzorigin={PAIRED}zz\""))).await,
            403
        );
        // no Authorization at all -> denied
        assert_eq!(gate(&dir, None).await, 403);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn empty_allowlist_denies_everything() {
        let dir = unique_tmp();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pairings.json"), "{\"peers\":[]}").unwrap();
        assert_eq!(gate(&dir, Some(&format!("X-Matrix origin=\"{PAIRED}\",sig=\"x\""))).await, 403);
        std::fs::remove_dir_all(&dir).ok();
    }
}
