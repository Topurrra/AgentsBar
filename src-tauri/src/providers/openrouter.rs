//! Ported from Sources/CodexBarCore/Providers/OpenRouter/OpenRouterUsageStats.swift.
//! Credits balance from `/api/v1/credits`, optional per-key quota from `/api/v1/key`.

use async_trait::async_trait;
use serde::Deserialize;

use super::api_token::{api_key, get_json, has_api_key, Auth};
use super::util::percent;
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow};
use crate::config::Config;

const BASE: &str = "https://openrouter.ai/api/v1";

pub struct OpenRouter;

#[derive(Debug, Deserialize)]
struct CreditsEnvelope {
    data: CreditsData,
}

#[derive(Debug, Deserialize)]
struct CreditsData {
    #[serde(default)]
    total_credits: f64,
    #[serde(default)]
    total_usage: f64,
}

#[derive(Debug, Deserialize)]
struct KeyEnvelope {
    data: KeyData,
}

#[derive(Debug, Default, Deserialize)]
struct KeyData {
    limit: Option<f64>,
    usage: Option<f64>,
}

/// Remaining balance is total minus spend, never negative (Swift: `balance`).
fn balance(data: &CreditsData) -> f64 {
    (data.total_credits - data.total_usage).max(0.0)
}

/// The key window exists only when the key itself has a spend limit configured.
fn key_window(key: &KeyData) -> Option<UsageWindow> {
    let (limit, usage) = (key.limit?, key.usage?);
    if limit <= 0.0 || usage < 0.0 {
        return None;
    }
    Some(UsageWindow::new(
        "Key limit",
        Some(percent(usage, limit)),
        None,
        None,
    ))
}

#[async_trait]
impl Provider for OpenRouter {
    fn id(&self) -> &'static str {
        "openrouter"
    }

    fn name(&self) -> &'static str {
        "OpenRouter"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn doc_url(&self) -> &'static str {
        "https://openrouter.ai/settings/keys"
    }

    fn env_key(&self) -> Option<&'static str> {
        Some("OPENROUTER_API_KEY")
    }

    fn is_configured(&self, config: &Config) -> bool {
        has_api_key(config, self)
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let key = api_key(&ctx.config, self)?;
        let auth = Auth::Bearer(&key);
        let headers = [("X-Title", "AgentsBar")];

        let credits: CreditsEnvelope =
            get_json(&ctx.http, &format!("{BASE}/credits"), &auth, &headers).await?;

        // Enrichment only: a key with no configured limit answers without limit/usage,
        // and that must never fail the credits reading.
        let key_data = get_json::<KeyEnvelope>(&ctx.http, &format!("{BASE}/key"), &auth, &headers)
            .await
            .map(|e| e.data)
            .unwrap_or_default();

        let mut snapshot = UsageSnapshot::new(self.id());
        snapshot.credits = Some(balance(&credits.data));
        snapshot.primary = key_window(&key_data);
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_credits_and_key_quota() {
        let credits: CreditsEnvelope =
            serde_json::from_str(r#"{"data":{"total_credits":100.0,"total_usage":25.5}}"#).unwrap();
        assert_eq!(balance(&credits.data), 74.5);

        let key: KeyEnvelope = serde_json::from_str(
            r#"{"data":{"limit":10.0,"usage":2.5,"rate_limit":{"requests":10,"interval":"10s"}}}"#,
        )
        .unwrap();
        assert_eq!(key_window(&key.data).unwrap().used_percent, Some(25.0));
    }

    #[test]
    fn key_without_limit_yields_no_window() {
        let key: KeyEnvelope =
            serde_json::from_str(r#"{"data":{"limit":null,"usage":2.5}}"#).unwrap();
        assert!(key_window(&key.data).is_none());
    }

    #[test]
    fn overspent_balance_floors_at_zero() {
        let credits: CreditsEnvelope =
            serde_json::from_str(r#"{"data":{"total_credits":1.0,"total_usage":3.0}}"#).unwrap();
        assert_eq!(balance(&credits.data), 0.0);
    }
}
