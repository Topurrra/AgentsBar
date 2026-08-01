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

    // `is_configured` does blocking file IO for the cookie providers (a cold scan copies
    // and decrypts each browser database), so the whole gate runs off the runtime.
    let gate = config.clone();
    let ready: Vec<&'static str> = tauri::async_runtime::spawn_blocking(move || {
        all_providers()
            .iter()
            .filter(|p| gate.is_enabled(p.id()) && p.is_configured(&gate))
            .map(|p| p.id())
            .collect()
    })
    .await
    .unwrap_or_default();

    let mut tasks = JoinSet::new();
    for provider in all_providers() {
        if !ready.contains(&provider.id()) {
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

    let mut fresh = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((id, result)) => {
                fresh.extend(sampleable(&result));
                store(app, &id, result).await;
            }
            Err(e) => log::warn!("provider task failed: {e}"),
        }
    }

    LAST_REFRESH.store(Utc::now().timestamp(), Ordering::Relaxed);
    record_samples(app, fresh);
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
    record_samples(app, sampleable(&result).into_iter().collect());
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

/// Only a successful fetch becomes a sparkline sample. A failed fetch keeps the previous
/// snapshot on screen, and re-recording that stale value would draw a flat stretch that
/// never happened.
fn sampleable(result: &Result<UsageSnapshot, ProviderError>) -> Option<UsageSnapshot> {
    result.as_ref().ok().cloned()
}

/// Persist samples off the refresh path: the history write hits the disk and the refresh
/// loop must not wait on it. Concurrent writes serialize on the history lock inside
/// [`AppState::record_history`], so the file cannot tear.
fn record_samples(app: &AppHandle, fresh: Vec<UsageSnapshot>) {
    if fresh.is_empty() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        app.state::<AppState>().record_history(&fresh).await;
    });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::History;
    use crate::providers::UsageWindow;

    fn ok(used: f64) -> Result<UsageSnapshot, ProviderError> {
        let mut s = UsageSnapshot::new("codex");
        s.primary = Some(UsageWindow {
            label: "5h".into(),
            used_percent: used,
            resets_at: None,
            window_minutes: None,
        });
        Ok(s)
    }

    /// The scheduler half of the sampling rule: only fetches that came back Ok reach the
    /// history. History's own tests cover dedup, clamping and the ring cap.
    #[test]
    fn only_successful_fetches_are_sampled() {
        let mut history = History::default();
        let results = [
            ok(12.0),
            Err(ProviderError::Http("502".into())),
            Err(ProviderError::NotConfigured),
        ];
        for result in &results {
            if let Some(snapshot) = sampleable(result) {
                history.record(&snapshot, 300);
            }
        }
        assert_eq!(history.series().len(), 1);
        assert_eq!(history.series()["codex"].len(), 1);
        assert_eq!(history.series()["codex"][0].u, 12.0);
    }
}
