use tauri::{AppHandle, Manager, State};

use crate::config::{Config, ProviderConfig};
use crate::providers::{all_providers, ProviderInfo, UsageSnapshot};
use crate::state::AppState;

#[tauri::command]
pub async fn list_providers(state: State<'_, AppState>) -> Result<Vec<ProviderInfo>, String> {
    let config = state.config.read().await;
    Ok(all_providers()
        .iter()
        .map(|p| ProviderInfo {
            id: p.id(),
            name: p.name(),
            auth: p.auth_kind(),
            configured: p.is_configured(&config),
            doc_url: p.doc_url(),
        })
        .collect())
}

#[tauri::command]
pub async fn get_snapshots(state: State<'_, AppState>) -> Result<Vec<UsageSnapshot>, String> {
    Ok(state.snapshots_in_order().await)
}

#[tauri::command]
pub async fn refresh_all(app: AppHandle) -> Result<(), String> {
    crate::scheduler::refresh_now(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn refresh_provider(app: AppHandle, id: String) -> Result<(), String> {
    crate::scheduler::refresh_one(&app, &id).await;
    Ok(())
}

#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> Result<Config, String> {
    // API keys never leave the backend: the UI only needs ProviderInfo::configured.
    Ok(state.config.read().await.redacted())
}

#[tauri::command]
pub async fn set_config(app: AppHandle, mut config: Config) -> Result<(), String> {
    config.normalize();
    let launch_at_startup = config.launch_at_startup;
    {
        let state = app.state::<AppState>();
        let mut current = state.config.write().await;
        // The incoming copy is redacted and may be stale, so keys stay as stored.
        config.merge_keys_from(&current);
        config.save().map_err(|e| e.to_string())?;
        *current = config;
    }
    crate::tray::apply_autostart(&app, launch_at_startup);
    // Refreshing also restarts the scheduler countdown, so a new interval applies now.
    crate::scheduler::refresh_now(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn set_api_key(app: AppHandle, id: String, key: String) -> Result<(), String> {
    let key = key.trim().to_string();
    {
        let state = app.state::<AppState>();
        let mut config = state.config.write().await;
        let entry = config
            .providers
            .entry(id.clone())
            .or_insert(ProviderConfig {
                enabled: false,
                api_key: None,
            });
        entry.enabled = !key.is_empty();
        entry.api_key = if key.is_empty() { None } else { Some(key) };
        config.save().map_err(|e| e.to_string())?;
    }
    crate::scheduler::refresh_one(&app, &id).await;
    Ok(())
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}
