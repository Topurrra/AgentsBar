use std::collections::HashMap;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::config::{Config, COOKIE_BROWSERS, COOKIE_SOURCES};
use crate::history::Sample;
use crate::providers::{all_providers, ProviderInfo, UsageSnapshot};
use crate::state::AppState;

#[tauri::command]
pub async fn list_providers(state: State<'_, AppState>) -> Result<Vec<ProviderInfo>, String> {
    let config = state.config.read().await.clone();
    // `is_configured` reads cookie databases with blocking file IO on a cold cache, so it
    // does not belong on a runtime worker thread.
    tauri::async_runtime::spawn_blocking(move || {
        all_providers()
            .iter()
            .map(|p| ProviderInfo {
                id: p.id(),
                name: p.name(),
                auth: p.auth_kind(),
                configured: p.is_configured(&config),
                doc_url: p.doc_url(),
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
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
    // A changed cookie source or browser must not be answered from the memo.
    crate::cookies::invalidate();
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
        let entry = config.providers.entry(id.clone()).or_default();
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

// ------------------------------------------------------------------ cookies

#[derive(Debug, Serialize)]
pub struct BrowserInfo {
    /// One of `COOKIE_BROWSERS`, matching `ProviderConfig::cookie_browser`.
    pub id: &'static str,
    pub label: String,
    /// False when the browser writes app bound (v20) cookies we deliberately do not read.
    pub supported: bool,
    /// Why it is limited, when it is. `None` means fully supported.
    pub note: Option<String>,
}

/// Detected browsers with a cookie database, one row per browser (not per profile),
/// in the order `cookie_source = "auto"` tries them.
#[tauri::command]
pub fn list_browsers() -> Vec<BrowserInfo> {
    let mut out: Vec<BrowserInfo> = Vec::new();
    for profile in crate::cookies::detect_browsers() {
        let note = crate::cookies::limitation(&profile);
        match out.iter_mut().find(|b| b.id == profile.browser) {
            // A second profile of the same browser: the browser counts as usable if any
            // one of its profiles is.
            Some(existing) if note.is_none() => {
                existing.supported = true;
                existing.note = None;
            }
            Some(_) => {}
            None => out.push(BrowserInfo {
                id: profile.browser,
                label: base_label(&profile.label).to_string(),
                supported: note.is_none(),
                note,
            }),
        }
    }
    out
}

/// "Chrome (Profile 1)" back to "Chrome".
fn base_label(label: &str) -> &str {
    label.split(" (").next().unwrap_or(label).trim()
}

#[tauri::command]
pub async fn set_cookie_source(
    app: AppHandle,
    id: String,
    source: String,
    browser: Option<String>,
) -> Result<(), String> {
    if !COOKIE_SOURCES.contains(&source.as_str()) {
        return Err(format!("unknown cookie source {source:?}"));
    }
    if let Some(b) = browser.as_deref() {
        if !COOKIE_BROWSERS.contains(&b) {
            return Err(format!("unknown browser {b:?}"));
        }
    }
    {
        let state = app.state::<AppState>();
        let mut config = state.config.write().await;
        let entry = config.providers.entry(id.clone()).or_default();
        entry.cookie_source = source.clone();
        entry.cookie_browser = browser;
        // "off" is the user turning the provider off, anything else turns it on.
        entry.enabled = source != "off";
        config.save().map_err(|e| e.to_string())?;
    }
    crate::cookies::invalidate();
    crate::scheduler::refresh_one(&app, &id).await;
    Ok(())
}

/// The header is a secret and is never echoed back: `get_config` redacts it.
#[tauri::command]
pub async fn set_cookie_header(app: AppHandle, id: String, header: String) -> Result<(), String> {
    let header = header.trim().to_string();
    {
        let state = app.state::<AppState>();
        let mut config = state.config.write().await;
        let entry = config.providers.entry(id.clone()).or_default();
        if header.is_empty() {
            entry.cookie_header = None;
            // Fall back to reading the browser rather than leaving a dead manual source.
            if entry.cookie_source == "manual" {
                entry.cookie_source = "auto".to_string();
            }
        } else {
            entry.cookie_header = Some(header);
            entry.cookie_source = "manual".to_string();
            entry.enabled = true;
        }
        config.save().map_err(|e| e.to_string())?;
    }
    crate::cookies::invalidate();
    crate::scheduler::refresh_one(&app, &id).await;
    Ok(())
}

// ------------------------------------------------------------------ history

#[tauri::command]
pub async fn get_history(state: State<'_, AppState>) -> Result<HashMap<String, Vec<Sample>>, String> {
    Ok(state.history.read().await.series().clone())
}
