pub mod commands;
pub mod config;
pub mod cookies;
pub mod history;
pub mod providers;
pub mod scheduler;
pub mod state;
pub mod tray;
pub mod updater;

mod redact;

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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                // Every log line leaves through here, so redaction is a property of the
                // sink and not of whoever wrote the `log::info!`.
                .format(|out, message, record| {
                    out.finish(format_args!(
                        "[{}][{}][{}] {}",
                        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                        record.level(),
                        record.target(),
                        redact::redact(&message.to_string())
                    ))
                })
                .build(),
        )
        .setup(|app| {
            // One line per run, so "the log is empty" means the logger is broken rather
            // than being indistinguishable from "nothing happened worth logging".
            log::info!("AgentsBar {} starting", env!("CARGO_PKG_VERSION"));
            // A killed or aborted run leaves its cookie database copies behind, and a
            // Firefox copy holds cleartext cookie values. Clear them before anything else.
            cookies::sweep_temp_copies();
            // Ensure the Start Menu shortcut has the AppUserModelID, so toast notifications
            // can find it. Self-healing for any install method.
            tray::ensure_shortcut_aumid();
            // Renamed from AgentBar: pick up the old directory before reading it, and drop
            // the old autostart value so a wave 4 upgrader does not get two tray icons.
            config::migrate_legacy_dir();
            config::remove_legacy_autostart();
            app.manage(state::AppState::new(config::Config::load()));
            tray::setup(app.handle())?;
            scheduler::start(app.handle().clone());
            // Silent unless there is something to install, and it still asks before
            // installing it. Row 29.
            updater::check_on_startup(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_providers,
            commands::get_snapshots,
            commands::refresh_all,
            commands::refresh_provider,
            commands::get_config,
            commands::get_cadence_secs,
            commands::set_config,
            commands::set_api_key,
            commands::quit_app,
            commands::list_browsers,
            commands::set_cookie_source,
            commands::set_cookie_header,
            commands::get_history,
            commands::export_diagnostics,
            commands::clear_cookie_cache,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AgentsBar");
}
