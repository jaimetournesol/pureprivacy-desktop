//! Tauri commands — the exact shared contract the frontend codes against.
//! All return Result<_, String>; arg keys arrive camelCase from JS and Tauri
//! v2 maps them onto these snake_case parameters automatically.

use rand::seq::SliceRandom;
use rand::Rng;
use serde::Serialize;
use tauri::AppHandle;

use crate::state::{self, Phase, Status};
use crate::supervisor;
use crate::words::WORDS;

#[tauri::command]
pub fn get_status(app: AppHandle) -> Result<Status, String> {
    Ok(state::read(&app, |inner| inner.status()))
}

/// Word-dash style: "coral-armada-poem-baker-42".
#[tauri::command]
pub fn suggest_password() -> Result<String, String> {
    let mut rng = rand::thread_rng();
    let words: Vec<&str> = WORDS.choose_multiple(&mut rng, 4).copied().collect();
    let digits: u8 = rng.gen_range(10..100);
    Ok(format!("{}-{}", words.join("-"), digits))
}

#[tauri::command]
pub fn begin_setup(
    app: AppHandle,
    box_name: String,
    username: String,
    password: String,
) -> Result<(), String> {
    let box_name = box_name.trim().to_string();
    let username = username.trim().to_string();
    if box_name.is_empty() {
        return Err("Give your box a name first.".into());
    }
    if username.is_empty() {
        return Err("Pick a username first.".into());
    }
    if password.trim().is_empty() {
        return Err("Pick a password first.".into());
    }
    // Note: the password is handed to account creation during first run; we
    // deliberately never persist it to disk.

    let mut rng = rand::thread_rng();
    let phrase: Vec<String> =
        WORDS.choose_multiple(&mut rng, 6).map(|w| w.to_string()).collect();
    let token_bytes: [u8; 16] = rng.gen();
    let token: String = token_bytes.iter().map(|b| format!("{b:02x}")).collect();
    // coturn long-term auth secret (32 random bytes, hex). The homeserver signs
    // short-lived TURN credentials with it; never leaves the box.
    let turn_bytes: [u8; 32] = rng.gen();
    let turn_secret: String = turn_bytes.iter().map(|b| format!("{b:02x}")).collect();
    let created = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();

    state::update(&app, |inner| {
        inner.box_name = box_name;
        inner.username = username;
        inner.created = created;
        inner.phrase = phrase;
        inner.token = token;
        inner.turn_secret = turn_secret;
    });
    state::persist(&app)?;

    // Real sidecars if the binaries are there, demo simulation otherwise.
    // Progress is observable via get_status().setup_stage.
    supervisor::start_lifecycle(&app);
    Ok(())
}

#[derive(Serialize)]
pub struct RecoveryKit {
    pub phrase: String,
    pub onion: Option<String>,
    pub created: String,
    pub box_name: String,
}

fn kit_from_state(app: &AppHandle) -> Result<RecoveryKit, String> {
    state::read(app, |inner| {
        if inner.phrase.is_empty() {
            return Err("No recovery kit yet — set up your box first.".to_string());
        }
        Ok(RecoveryKit {
            phrase: inner.phrase.join(" "),
            onion: inner.onion.clone(),
            created: inner.created.clone(),
            box_name: inner.box_name.clone(),
        })
    })
}

#[tauri::command]
pub fn get_recovery_kit(app: AppHandle) -> Result<RecoveryKit, String> {
    kit_from_state(&app)
}

/// `index` is 0-based into the phrase words.
#[tauri::command]
pub fn confirm_recovery_word(app: AppHandle, index: usize, word: String) -> Result<bool, String> {
    state::read(&app, |inner| {
        let expected = inner
            .phrase
            .get(index)
            .ok_or_else(|| format!("No word at position {index}."))?;
        Ok(expected.eq_ignore_ascii_case(word.trim()))
    })
}

#[tauri::command]
pub fn save_recovery_kit_html(app: AppHandle) -> Result<String, String> {
    let kit = kit_from_state(&app)?;
    let downloads = dirs::download_dir().ok_or("Couldn't find your Downloads folder.")?;

    let slug: String = kit
        .box_name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let filename = if slug.is_empty() {
        "pureprivacy-recovery-kit.html".to_string()
    } else {
        format!("pureprivacy-recovery-kit-{slug}.html")
    };
    let path = downloads.join(filename);

    let words_html: String = kit
        .phrase
        .split_whitespace()
        .enumerate()
        .map(|(i, w)| format!("<li><span class=\"num\">{}</span> {w}</li>", i + 1))
        .collect();
    let onion_line = kit.onion.as_deref().unwrap_or("(not minted yet — re-save this kit once your box is running)");

    // Self-contained, printable: dark-on-white print style, no external assets.
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>PurePrivacy recovery kit — {box_name}</title>
<style>
  body {{ font-family: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
         color: #1A1A1A; background: #FFFFFF; max-width: 640px; margin: 48px auto; padding: 0 24px; }}
  h1 {{ font-size: 22px; }} h1 .dot {{ color: #F2B705; }}
  .meta {{ color: #555; font-size: 14px; margin-bottom: 28px; }}
  ol.phrase {{ list-style: none; padding: 0; display: grid; grid-template-columns: 1fr 1fr 1fr; gap: 12px; }}
  ol.phrase li {{ border: 1px solid #ccc; border-radius: 12px; padding: 12px 14px;
                  font-size: 18px; font-weight: 600; }}
  ol.phrase .num {{ color: #999; font-weight: 400; margin-right: 6px; font-size: 13px; }}
  .address {{ font-family: ui-monospace, monospace; font-size: 13px; word-break: break-all;
              border: 1px solid #ccc; border-radius: 12px; padding: 12px 14px; }}
  .warning {{ margin-top: 28px; border-left: 4px solid #F2B705; padding: 8px 14px; font-size: 15px; }}
  .note {{ color: #777; font-size: 13px; margin-top: 20px; }}
  .sig {{ color: #777; font-size: 13px; margin-top: 36px; font-style: italic; }}
  @media print {{ body {{ margin: 0 auto; }} }}
</style>
</head>
<body>
  <h1>PurePrivacy recovery kit <span class="dot">●</span></h1>
  <div class="meta">Box: <strong>{box_name}</strong> &nbsp;·&nbsp; Created: {created}</div>

  <h2>Your six recovery words</h2>
  <ol class="phrase">{words_html}</ol>

  <h2>Your private address</h2>
  <div class="address">{onion_line}</div>

  <p class="warning">If you forget your password, this kit is the only way back in.
  No company can reset it — that's the point.</p>

  <p class="note">Printer spools retain copies — collect your printout.</p>

  <p class="sig">Private, and a little slower — that's the deal.</p>
</body>
</html>
"#,
        box_name = kit.box_name,
        created = kit.created,
    );

    std::fs::write(&path, html).map_err(|e| format!("Couldn't save the kit: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

#[derive(Serialize)]
pub struct ConnectQr {
    pub payload: String,
    pub svg: String,
}

#[tauri::command]
pub fn get_connect_qr(app: AppHandle) -> Result<ConnectQr, String> {
    let (onion, username, token) = state::read(&app, |inner| {
        (inner.onion.clone(), inner.username.clone(), inner.token.clone())
    });
    let onion = onion.ok_or("Your box doesn't have an address yet.")?;
    if token.is_empty() {
        return Err("Set up your box first.".into());
    }
    let payload = format!("pureprivacy://connect?hs={onion}&user={username}&token={token}");

    let code = qrcode::QrCode::new(payload.as_bytes())
        .map_err(|e| format!("Couldn't build the QR code: {e}"))?;
    let rendered = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(240, 240)
        .quiet_zone(true)
        .dark_color(qrcode::render::svg::Color("#1A1A1A"))
        .light_color(qrcode::render::svg::Color("#FFFFFF"))
        .build();
    // The renderer prefixes an XML declaration; the contract wants a complete
    // <svg> element, so trim anything before it.
    let svg = match rendered.find("<svg") {
        Some(idx) => rendered[idx..].to_string(),
        None => rendered,
    };
    Ok(ConnectQr { payload, svg })
}

#[tauri::command]
pub fn stop_box(app: AppHandle) -> Result<(), String> {
    supervisor::stop_lifecycle(&app);
    Ok(())
}

#[tauri::command]
pub fn start_box(app: AppHandle) -> Result<(), String> {
    let configured = state::read(&app, |inner| inner.phase != Phase::Fresh && !inner.box_name.is_empty());
    if !configured {
        return Err("Set up your box first.".into());
    }
    supervisor::start_lifecycle(&app);
    Ok(())
}

#[derive(serde::Serialize)]
pub struct LegacyInstall {
    /// True if a v0.1 Docker appliance is running on this machine.
    pub present: bool,
    /// The `pureprivacy-*` container names found (e.g. for the UI to list).
    pub containers: Vec<String>,
}

/// Detect an existing v0.1 (Docker appliance) PurePrivacy install so the native
/// app never silently orphans someone's running box. (Plan task T-MIG.) The
/// native app uses a different engine (tuwunel, fresh server identity — no DB
/// migration from Synapse), so the UI must offer an explicit choice rather than
/// stomping the running stack. Best-effort: missing/!running docker => none.
#[tauri::command]
pub fn detect_legacy_install() -> Result<LegacyInstall, String> {
    let out = std::process::Command::new("docker")
        .args([
            "ps",
            "--filter",
            "name=pureprivacy-",
            "--format",
            "{{.Names}}",
        ])
        .output();
    let containers: Vec<String> = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            // Don't mistake a second native install's helpers for the appliance:
            // the v0.1 stack uses these exact service suffixes.
            .filter(|l| {
                l.starts_with("pureprivacy-")
                    && (l.contains("synapse")
                        || l.contains("tor")
                        || l.contains("wizard")
                        || l.contains("postgres")
                        || l.contains("coturn")
                        || l.contains("mcp"))
            })
            .map(String::from)
            .collect(),
        // docker absent, daemon down, or no perms => treat as "no legacy box".
        _ => Vec::new(),
    };
    Ok(LegacyInstall {
        present: !containers.is_empty(),
        containers,
    })
}
