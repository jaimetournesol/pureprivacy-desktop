//! PurePrivacy desktop backend. The frontend polls get_status() every 1.5s;
//! nothing is pushed via events.

mod account;
mod commands;
mod config;
mod crypto;
mod pairing;
mod state;
mod supervisor;
mod tray;
mod words;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(state::AppState::default())
        .manage(supervisor::Supervisor::default())
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::suggest_password,
            commands::begin_setup,
            commands::get_recovery_kit,
            commands::confirm_recovery_word,
            commands::save_recovery_kit_html,
            commands::get_connect_qr,
            commands::stop_box,
            commands::start_box,
            commands::detect_legacy_install,
            commands::get_join_info,
            commands::app_info,
            commands::reset_box,
            commands::pair_create,
            commands::pair_accept,
            commands::pair_list,
            commands::pair_remove,
        ])
        .setup(|app| {
            state::load_persisted(app.handle());
            tray::init(app.handle())?;
            // Opt-in auto-resume: with PUREPRIVACY_AUTOSTART=1, a provisioned box
            // comes back up on launch (tor + homeserver + sidecars) without a manual
            // Start click. Used by the multi-box demo launcher; default behaviour
            // (come up Stopped, user starts it) is unchanged when unset.
            if std::env::var("PUREPRIVACY_AUTOSTART").ok().as_deref() == Some("1") {
                if state::read(app.handle(), |i| i.onion.is_some()) {
                    supervisor::start_lifecycle(app.handle(), None);
                } else if let (Ok(user), Ok(pass)) = (
                    std::env::var("PUREPRIVACY_PROVISION_USER"),
                    std::env::var("PUREPRIVACY_PROVISION_PASS"),
                ) {
                    // Headless first-run provisioning (demos/tests): same path as the
                    // GUI wizard — mint the onion + create the admin account — driven
                    // by env instead of a click. No-op once the box has an onion.
                    let box_name = std::env::var("PUREPRIVACY_PROVISION_BOX")
                        .unwrap_or_else(|_| format!("{user}box"));
                    if let Err(e) = commands::begin_setup(app.handle().clone(), box_name, user, pass) {
                        eprintln!("[pureprivacy] headless provision failed: {e}");
                    }
                }
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                // Graceful SIGTERM to sidecars; kill_on_drop(true) on each
                // child is the SIGKILL backstop when the runtime tears down.
                app.state::<supervisor::Supervisor>().shutdown();
            }
        });
}
