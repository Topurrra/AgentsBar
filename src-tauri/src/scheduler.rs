use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use chrono::Utc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinSet;

use crate::providers::{all_providers, provider_by_id, FetchContext, ProviderError, UsageSnapshot};
use crate::state::AppState;

/// Notified whenever the refresh countdown must restart (config change, manual refresh).
static WAKE: Notify = Notify::const_new();
/// Unix seconds of the last completed full refresh, 0 when there was none.
static LAST_REFRESH: AtomicI64 = AtomicI64::new(0);
/// Only one refresh at a time. Codex, Claude and Gemini rotate a single use OAuth
/// refresh token during a fetch, so two overlapping refreshes would race to persist
/// it and leave a dead token behind.
/// ponytail: one global lock, per provider locks only if a slow provider starts
/// holding up the others.
static REFRESH: Mutex<()> = Mutex::const_new(());

const STALE_SECS: i64 = 60;

/// Periodic refresh loop. Sleeping is interrupted by [`WAKE`] so a changed
/// `refresh_minutes` takes effect on the next tick instead of the next hour.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        refresh_now(&app).await;
        loop {
            let minutes = {
                let state = app.state::<AppState>();
                let cfg = state.config.read().await;
                cfg.refresh_minutes.max(1)
            };
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(minutes.saturating_mul(60))) => refresh_now(&app).await,
                _ = WAKE.notified() => {}
            }
        }
    });
}

/// Fetch every enabled and configured provider concurrently, then publish.
pub async fn refresh_now(app: &AppHandle) {
    let _guard = REFRESH.lock().await;
    refresh_all_locked(app).await;
}

async fn refresh_all_locked(app: &AppHandle) {
    let state = app.state::<AppState>();
    let http = state.http.clone();
    let config = state.config.read().await.clone();

    let mut tasks = JoinSet::new();
    for provider in all_providers() {
        if !config.is_enabled(provider.id()) || !provider.is_configured(&config) {
            continue;
        }
        let ctx = FetchContext {
            http: http.clone(),
            config: config.clone(),
        };
        tasks.spawn(async move {
            let id = provider.id().to_string();
            (id, provider.fetch(&ctx).await)
        });
    }

    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((id, result)) => store(app, &id, result).await,
            Err(e) => log::warn!("provider task failed: {e}"),
        }
    }

    LAST_REFRESH.store(Utc::now().timestamp(), Ordering::Relaxed);
    publish(app).await;
    WAKE.notify_one();
}

/// Refresh a single provider, ignoring the enabled flag (explicit user action).
pub async fn refresh_one(app: &AppHandle, id: &str) {
    let Some(provider) = provider_by_id(id) else {
        return;
    };
    let _guard = REFRESH.lock().await;
    let ctx = {
        let state = app.state::<AppState>();
        state.fetch_context().await
    };
    let result = provider.fetch(&ctx).await;
    store(app, id, result).await;
    publish(app).await;
}

/// Used when the popover opens: only hits the network if the data is old.
pub async fn refresh_if_stale(app: &AppHandle) {
    // Staleness is rechecked under the lock: reopening the popover while a refresh is
    // still running must wait for it, not queue a second one behind it.
    let _guard = REFRESH.lock().await;
    if Utc::now().timestamp() - LAST_REFRESH.load(Ordering::Relaxed) >= STALE_SECS {
        refresh_all_locked(app).await;
    }
}

/// A failed fetch keeps the last good snapshot and only sets `error`.
async fn store(app: &AppHandle, id: &str, result: Result<UsageSnapshot, ProviderError>) {
    let state = app.state::<AppState>();
    let mut snapshots = state.snapshots.write().await;
    match result {
        Ok(snapshot) => {
            snapshots.insert(id.to_string(), snapshot);
        }
        Err(e) => {
            let message = e.to_string();
            log::warn!("{id} refresh failed: {message}");
            snapshots
                .entry(id.to_string())
                .or_insert_with(|| UsageSnapshot::new(id))
                .error = Some(message);
        }
    }
}

async fn publish(app: &AppHandle) {
    let state = app.state::<AppState>();
    let snapshots = state.snapshots_in_order().await;
    let config = state.config.read().await.clone();
    crate::tray::update(app, &snapshots, &config);
    if let Err(e) = app.emit("usage-updated", &snapshots) {
        log::warn!("usage-updated emit failed: {e}");
    }
}
