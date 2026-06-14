//! First-run admin account creation against the local tuwunel homeserver.
//!
//! tuwunel grants the FIRST registered user admin automatically. We gate
//! registration behind a generated `registration_token` (which also doubles as
//! the "join token" the owner shares to add more people later), so the box is
//! never an open-registration server.
//!
//! The flow is the standard Matrix User-Interactive Auth dance, verified
//! against tuwunel 1.7.1 (two steps, no trailing `m.login.dummy`):
//!   1. POST /register {username,password}              -> 401 {session, flows}
//!   2. POST /register {..., auth: registration_token}  -> 200 {user_id}

use crate::config::{off, HOMESERVER_PORT};

fn register_url() -> String {
    format!("http://127.0.0.1:{}/_matrix/client/v3/register", HOMESERVER_PORT + off())
}

fn errcode(v: &serde_json::Value) -> Option<&str> {
    v.get("errcode").and_then(|c| c.as_str())
}

/// Create the admin user. Idempotent: a re-run after a crash that finds the
/// user already present (`M_USER_IN_USE`) is treated as success.
pub async fn create_admin(username: &str, password: &str, token: &str) -> Result<(), String> {
    let url = register_url();
    let client = reqwest::Client::new();

    // Step 1 — provoke a UIA session.
    let r1 = client
        .post(&url)
        .json(&serde_json::json!({
            "username": username,
            "password": password,
            "inhibit_login": true,
        }))
        .send()
        .await
        .map_err(|e| format!("couldn't reach the homeserver to create your account: {e}"))?;

    if r1.status().is_success() {
        return Ok(()); // server accepted without UIA (open reg) — done
    }
    let v1: serde_json::Value = r1.json().await.map_err(|e| format!("register step 1: {e}"))?;
    if errcode(&v1) == Some("M_USER_IN_USE") {
        return Ok(()); // already created on a previous run
    }
    let session = v1
        .get("session")
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("the homeserver didn't offer a registration session: {v1}"))?;

    // Step 2 — complete with the registration token. First user => admin.
    let r2 = client
        .post(&url)
        .json(&serde_json::json!({
            "username": username,
            "password": password,
            "inhibit_login": true,
            "auth": {
                "type": "m.login.registration_token",
                "token": token,
                "session": session,
            },
        }))
        .send()
        .await
        .map_err(|e| format!("register step 2: {e}"))?;

    if r2.status().is_success() {
        return Ok(());
    }
    let v2: serde_json::Value = r2.json().await.map_err(|e| format!("register step 2: {e}"))?;
    if errcode(&v2) == Some("M_USER_IN_USE") {
        return Ok(());
    }
    Err(format!("creating your account failed: {v2}"))
}
