//! PurePrivacy desktop backend. The frontend polls get_status() every 1.5s;
//! nothing is pushed via events.

mod account;
mod commands;
mod config;
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
