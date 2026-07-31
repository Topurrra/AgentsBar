use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::RwLock;

use crate::config::Config;
pub use crate::providers::{
    AuthKind, FetchContext, Provider, ProviderError, ProviderInfo, UsageSnapshot, UsageWindow,
};

pub struct AppState {
    pub snapshots: RwLock<HashMap<String, UsageSnapshot>>,
    pub config: RwLock<Config>,
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
            http,
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
