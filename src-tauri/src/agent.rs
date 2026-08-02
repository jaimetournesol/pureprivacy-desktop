//! Agents — provisioning the optional agent runtime that runs beside the box.
//!
//! An "agent" is a Hermes profile with its own Matrix account on THIS box. The box owns
//! provisioning because it holds the registration token; the agent container owns running
//! the model. The two meet at a handoff file on a shared volume — credentials never ride
//! account-data, which the phone can read.
//!
//! What the phone's one-tap setup triggers, in order:
//!   1. register `@<localpart>:<onion>` against the local client API (token-gated UIA),
//!   2. write the credentials to the handoff volume for the agent container to pick up,
//!   3. create a DM room and invite the agent, so it has somewhere to talk,
//!   4. publish the roster to account-data, which is what makes the phone's Agents app
//!      show it — and, crucially, what keeps it OUT of Messaging.
//!
//! Step 4 is the security-relevant one: the phone decides "human or AI" from this roster
//! and nothing else. A username is a string a remote box can choose, so inferring it from
//! the user id would let a federated peer masquerade as (or hide) an AI.

use serde_json::{json, Value};

/// The roster the phone reads. Mirrors `ai.tournesol.pureprivacy.pairings` in spirit:
/// box-published, owner-only, and the single source of truth for a client-side decision.
pub const AGENTS_ACCOUNT_DATA_TYPE: &str = "ai.tournesol.pureprivacy.agents";

/// Where the box drops credentials for the agent container. A shared volume, not
/// account-data: an access token is secret material and account-data is readable by every
/// device signed into the owner's account.
const HANDOFF_PATH: &str = "/handoff/matrix.env";

/// One provisioned agent.
pub struct Provisioned {
    pub user_id: String,
    pub display_name: String,
    pub room_id: Option<String>,
}

/// Register an agent account using the box's registration token.
///
/// tuwunel offers exactly one flow (`m.login.registration_token`) because `config.rs`
/// renders `allow_registration = true` together with a token — it is never an open
/// registration server. UIA is two calls: one to get a session id, one to satisfy the stage.
async fn register(
    client: &reqwest::Client,
    base: &str,
    localpart: &str,
    password: &str,
    join_token: &str,
) -> Result<(String, String, String), String> {
    let url = format!("{base}/_matrix/client/v3/register");

    // Stage 1 — an empty auth POST is expected to 401 WITH a session id. That is the
    // documented UIA handshake, not an error, so a 401 here is the success path.
    let r = client
        .post(&url)
        .json(&json!({}))
        .send()
        .await
        .map_err(|e| format!("couldn't reach the homeserver: {e}"))?;
    let v: Value = r
        .json()
        .await
        .map_err(|e| format!("bad registration response: {e}"))?;
    let session = v
        .get("session")
        .and_then(|s| s.as_str())
        .ok_or_else(|| "the homeserver didn't offer a registration session".to_string())?;

    // Stage 2 — satisfy the token stage and take the account.
    let r = client
        .post(&url)
        .json(&json!({
            "username": localpart,
            "password": password,
            "inhibit_login": false,
            "auth": {
                "type": "m.login.registration_token",
                "token": join_token,
                "session": session,
            }
        }))
        .send()
        .await
        .map_err(|e| format!("registration failed: {e}"))?;
    let v: Value = r
        .json()
        .await
        .map_err(|e| format!("bad registration response: {e}"))?;

    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        // M_USER_IN_USE is worth naming plainly: it means setup already ran once, and the
        // fix is to reuse the existing agent rather than to retry.
        let code = v.get("errcode").and_then(|c| c.as_str()).unwrap_or("");
        if code == "M_USER_IN_USE" {
            return Err(format!("an agent named '{localpart}' already exists on this box"));
        }
        return Err(err.to_string());
    }

    let user_id = v
        .get("user_id")
        .and_then(|u| u.as_str())
        .ok_or("registration returned no user_id")?
        .to_string();
    let token = v
        .get("access_token")
        .and_then(|t| t.as_str())
        .ok_or("registration returned no access_token")?
        .to_string();
    let device = v
        .get("device_id")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string();
    Ok((user_id, token, device))
}

/// Hand the agent its credentials on the shared volume.
///
/// Written 0600 and only to the handoff mount. If the mount isn't there the agent add-on
/// isn't installed, which is a normal state (agents are optional), so say so clearly
/// rather than failing with an io error the owner can't act on.
fn write_handoff(
    onion: &str,
    user_id: &str,
    token: &str,
    device: &str,
    owner: &str,
) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let dir = std::path::Path::new(HANDOFF_PATH)
        .parent()
        .ok_or("bad handoff path")?;
    if !dir.is_dir() {
        return Err("the agents add-on isn't installed on this box".to_string());
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(HANDOFF_PATH)
        .map_err(|e| format!("couldn't write the agent handoff: {e}"))?;
    // The homeserver URL is the box's own loopback: the agent shares our network namespace,
    // so it must NOT take a Tor circuit to reach the box sitting next to it.
    write!(
        f,
        "MATRIX_HOMESERVER=http://127.0.0.1:{port}\n\
         MATRIX_USER_ID={user_id}\n\
         MATRIX_ACCESS_TOKEN={token}\n\
         MATRIX_DEVICE_ID={device}\n\
         PP_BOX_ONION={onion}\n\
         PP_OWNER={owner}\n",
        port = crate::config::HOMESERVER_PORT + crate::config::off(),
    )
    .map_err(|e| format!("couldn't write the agent handoff: {e}"))?;
    Ok(())
}

/// Create the owner↔agent room and invite the agent.
///
/// `is_direct` + an explicit invite mirrors what the phone's contact exchange builds, so the
/// room reads as a normal DM to the client — the ONLY thing that marks it as an agent room
/// is the roster, which is what keeps that decision un-spoofable.
async fn create_room(
    client: &reqwest::Client,
    base: &str,
    owner_token: &str,
    agent_user: &str,
    name: &str,
) -> Option<String> {
    let r = client
        .post(format!("{base}/_matrix/client/v3/createRoom"))
        .bearer_auth(owner_token)
        .json(&json!({
            "preset": "trusted_private_chat",
            "is_direct": true,
            "name": name,
            "invite": [agent_user],
        }))
        .send()
        .await
        .ok()?;
    let v: Value = r.json().await.ok()?;
    v.get("room_id").and_then(|r| r.as_str()).map(String::from)
}

/// Publish the roster. This is what the phone keys "is this an AI?" off.
pub async fn publish_registry(
    client: &reqwest::Client,
    url: &str,
    owner_token: &str,
    agents: &[Provisioned],
) -> Result<(), String> {
    let list: Vec<Value> = agents
        .iter()
        .map(|a| {
            json!({
                "user_id": a.user_id,
                "display_name": a.display_name,
                "group": "",
                "description": "Runs on your box",
                "room_id": a.room_id,
            })
        })
        .collect();
    let r = client
        .put(url)
        .bearer_auth(owner_token)
        .json(&json!({ "agents": list, "updated_ts": crate::agent::now_ms() }))
        .send()
        .await
        .map_err(|e| format!("couldn't publish the agent roster: {e}"))?;
    if r.status().is_success() {
        Ok(())
    } else {
        Err(format!("couldn't publish the agent roster ({})", r.status()))
    }
}

/// Loopback port the agent container's WebUI listens on. It shares the box's network
/// namespace, so this is reachable from here exactly when the add-on is running.
const AGENT_WEBUI_PORT: u16 = 8787;

/// Is the agent runtime actually up? See the note in [`setup`] for why this beats testing
/// for the handoff directory.
pub async fn agent_running(client: &reqwest::Client) -> bool {
    client
        .get(format!("http://127.0.0.1:{AGENT_WEBUI_PORT}/"))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .is_ok()
}

/// A throwaway password for the agent account. Nothing ever types it: the agent
/// authenticates with the access token from registration, and this only exists because
/// the register endpoint requires one. Generated, used once, never stored.
fn random_password() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Read the roster the box previously published, so setup is additive rather than
/// clobbering agents provisioned earlier.
pub async fn read_registry(
    client: &reqwest::Client,
    url: &str,
    owner_token: &str,
) -> Vec<Provisioned> {
    let Ok(r) = client.get(url).bearer_auth(owner_token).send().await else {
        return Vec::new();
    };
    let Ok(v) = r.json::<Value>().await else {
        return Vec::new();
    };
    v.get("agents")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|o| {
                    Some(Provisioned {
                        user_id: o.get("user_id")?.as_str()?.to_string(),
                        display_name: o
                            .get("display_name")
                            .and_then(|d| d.as_str())
                            .unwrap_or("Agent")
                            .to_string(),
                        room_id: o
                            .get("room_id")
                            .and_then(|r| r.as_str())
                            .map(String::from),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Re-publish the roster from the handoff file, which is the box's own record of what it
/// provisioned. Called on every supervisor pass, like the status blob.
///
/// Two reasons this is not just belt-and-braces: a roster written once can be missed by a
/// client that wasn't synced at that moment (and account-data has no re-delivery), and the
/// phone's human/AI split is only correct while it holds a roster — so publishing it
/// repeatedly is what makes that split self-healing rather than dependent on one write
/// landing at the right time.
pub async fn republish_from_handoff(
    client: &reqwest::Client,
    registry_url: &str,
    owner_token: &str,
) {
    let Ok(body) = std::fs::read_to_string(HANDOFF_PATH) else {
        return; // no agent provisioned (or the add-on isn't installed) — nothing to say
    };
    let mut user_id = String::new();
    for line in body.lines() {
        if let Some(v) = line.strip_prefix("MATRIX_USER_ID=") {
            user_id = v.trim().to_string();
        }
    }
    if user_id.is_empty() {
        return;
    }
    // Keep whatever the current roster says about this agent (room id, group, name) so a
    // republish never clobbers detail we already published.
    let existing = read_registry(client, registry_url, owner_token).await;
    if let Some(known) = existing.iter().find(|a| a.user_id == user_id) {
        let one = [Provisioned {
            user_id: known.user_id.clone(),
            display_name: known.display_name.clone(),
            room_id: known.room_id.clone(),
        }];
        let _ = publish_registry(client, registry_url, owner_token, &one).await;
    } else {
        let one = [Provisioned {
            user_id,
            display_name: "Hermes".to_string(),
            room_id: None,
        }];
        let _ = publish_registry(client, registry_url, owner_token, &one).await;
    }
}

/// The whole one-tap flow. Returns the message the phone shows.
pub async fn setup(
    client: &reqwest::Client,
    base: &str,
    registry_url: &str,
    owner_token: &str,
    onion: &str,
    join_token: &str,
    owner: &str,
) -> Result<String, String> {
    if join_token.is_empty() {
        return Err("this box has no registration token, so it can't create an agent".into());
    }
    // Refuse early when the add-on isn't running — otherwise we'd mint a Matrix account for
    // a runtime that will never come up, and the owner would have to clean it up.
    //
    // Probe the agent's own port rather than testing for the handoff directory: the volume
    // is mounted on the box unconditionally (the box is its writer), so its existence says
    // nothing about whether the agent is actually there. A reply on the WebUI port does —
    // it can only come from the agent container sharing our network namespace. Any HTTP
    // status counts, including 302/401: we're proving something is listening, not
    // authenticating.
    if !agent_running(client).await {
        return Err("the agents add-on isn't running on this box — enable it with \
                    './pp-box agents on', then try again"
            .to_string());
    }

    let mut existing = read_registry(client, registry_url, owner_token).await;

    // A fixed first agent keeps setup idempotent-ish and predictable. `-ai` is a readable
    // convention, NOT the thing that makes it an agent (the roster is).
    let localpart = "hermes-ai";
    let display = "Hermes";
    let agent_user = format!("@{localpart}:{onion}");
    if existing.iter().any(|a| a.user_id == agent_user) {
        return Ok("agents are already set up on this box".to_string());
    }

    let password = random_password();
    let (user_id, token, device) = register(client, base, localpart, &password, join_token).await?;
    write_handoff(onion, &user_id, &token, &device, owner)?;

    let room_id = create_room(client, base, owner_token, &user_id, display).await;
    if room_id.is_none() {
        // Not fatal: the account and runtime are live, and a room can be created later.
        // Better to report a working-but-incomplete setup than to fail the whole thing.
        eprintln!("[pureprivacy] agent: account created but the room wasn't — will retry later");
    }

    existing.push(Provisioned {
        user_id: user_id.clone(),
        display_name: display.to_string(),
        room_id,
    });
    publish_registry(client, registry_url, owner_token, &existing).await?;

    Ok(format!("{display} is ready — say hello in the Agents app"))
}
