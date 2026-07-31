pub mod commands;
pub mod config;
pub mod providers;
pub mod scheduler;
pub mod state;
pub mod tray;

use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .build(),
        )
        .setup(|app| {
            app.manage(state::AppState::new(config::Config::load()));
            tray::setup(app.handle())?;
            scheduler::start(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_providers,
            commands::get_snapshots,
            commands::refresh_all,
            commands::refresh_provider,
            commands::get_config,
            commands::set_config,
            commands::set_api_key,
            commands::quit_app,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AgentBar");
}
