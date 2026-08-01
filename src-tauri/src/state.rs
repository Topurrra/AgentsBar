use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::RwLock;

use crate::config::Config;
pub use crate::history::{History, Sample};
pub use crate::providers::{
    AuthKind, FetchContext, Provider, ProviderError, ProviderInfo, UsageSnapshot, UsageWindow,
};

pub struct AppState {
    pub snapshots: RwLock<HashMap<String, UsageSnapshot>>,
    pub config: RwLock<Config>,
    /// Sparkline samples, loaded from disk at startup and appended after each refresh.
    pub history: RwLock<History>,
    pub http: reqwest::Client,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("AgentBar/0.1")
            .build()
            .expect("failed to build http client");
        Self {
            snapshots: RwLock::new(HashMap::new()),
            config: RwLock::new(config),
            history: RwLock::new(History::load()),
            http,
        }
    }

    /// Append a sample per successful snapshot and persist if anything changed.
    /// Call after a refresh has stored its snapshots.
    pub async fn record_history(&self, snapshots: &[UsageSnapshot]) {
        let refresh_secs = self.config.read().await.refresh_minutes.saturating_mul(60) as i64;
        let mut history = self.history.write().await;
        let mut changed = false;
        for snapshot in snapshots {
            changed |= history.record(snapshot, refresh_secs);
        }
        if changed {
            if let Err(e) = history.save() {
                log::warn!("history save failed: {e}");
            }
        }
    }

    /// Snapshots sorted by registry order, for the frontend and the tray.
    pub async fn snapshots_in_order(&self) -> Vec<UsageSnapshot> {
        let map = self.snapshots.read().await;
        crate::providers::all_providers()
            .iter()
            .filter_map(|p| map.get(p.id()).cloned())
            .collect()
    }

    pub async fn fetch_context(&self) -> FetchContext {
        FetchContext {
            http: self.http.clone(),
            config: self.config.read().await.clone(),
        }
    }
}
