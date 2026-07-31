//! Ported from Sources/CodexBarCore/Providers/DeepSeek/DeepSeekUsageFetcher.swift,
//! API-key path only (`GET https://api.deepseek.com/user/balance`). The richer
//! per-day usage numbers live behind a platform.deepseek.com browser session.

use async_trait::async_trait;
use serde::Deserialize;

use super::api_token::{api_key, get_json, Auth};
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot};
use crate::config::Config;

const BALANCE_URL: &str = "https://api.deepseek.com/user/balance";

pub struct Deepseek;

#[derive(Debug, Deserialize)]
struct BalanceResponse {
    #[serde(default)]
    is_available: bool,
    #[serde(default)]
    balance_infos: Vec<BalanceInfo>,
}

#[derive(Debug, Deserialize)]
struct BalanceInfo {
    currency: String,
    total_balance: String,
}

impl BalanceInfo {
    fn total(&self) -> f64 {
        self.total_balance.trim().parse().unwrap_or(0.0)
    }
}

/// Prefer a funded USD wallet, then any funded wallet, then USD, then whatever
/// came first: a positive CNY balance must not hide behind an empty USD one.
fn select(infos: &[BalanceInfo]) -> Option<&BalanceInfo> {
    infos
        .iter()
        .find(|i| i.currency == "USD" && i.total() > 0.0)
        .or_else(|| infos.iter().find(|i| i.total() > 0.0))
        .or_else(|| infos.iter().find(|i| i.currency == "USD"))
        .or_else(|| infos.first())
}

fn to_snapshot(response: &BalanceResponse) -> Result<UsageSnapshot, ProviderError> {
    let selected =
        select(&response.balance_infos).ok_or_else(|| ProviderError::Parse("no balance".into()))?;
    let mut snapshot = UsageSnapshot::new("deepseek");
    snapshot.credits = Some(selected.total());
    snapshot.plan = Some(if response.is_available {
        selected.currency.clone()
    } else {
        format!("{} (unavailable)", selected.currency)
    });
    Ok(snapshot)
}

#[async_trait]
impl Provider for Deepseek {
    fn id(&self) -> &'static str {
        "deepseek"
    }

    fn name(&self) -> &'static str {
        "DeepSeek"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn doc_url(&self) -> &'static str {
        "https://platform.deepseek.com/api_keys"
    }

    fn is_configured(&self, config: &Config) -> bool {
        config.api_key(self.id()).is_some()
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let key = api_key(&ctx.config, self.id())?;
        let response: BalanceResponse =
            get_json(&ctx.http, BALANCE_URL, &Auth::Bearer(key), &[]).await?;
        to_snapshot(&response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "is_available": true,
      "balance_infos": [
        {"currency":"CNY","total_balance":"110.00","granted_balance":"10.00","topped_up_balance":"100.00"},
        {"currency":"USD","total_balance":"0.00","granted_balance":"0.00","topped_up_balance":"0.00"}
      ]
    }"#;

    #[test]
    fn prefers_the_funded_wallet_over_an_empty_usd_one() {
        let parsed: BalanceResponse = serde_json::from_str(SAMPLE).unwrap();
        let snapshot = to_snapshot(&parsed).unwrap();
        assert_eq!(snapshot.credits, Some(110.0));
        assert_eq!(snapshot.plan.as_deref(), Some("CNY"));
    }

    #[test]
    fn unavailable_balance_is_flagged_in_the_plan_label() {
        let parsed: BalanceResponse = serde_json::from_str(
            r#"{"is_available":false,"balance_infos":[{"currency":"USD","total_balance":"0.00"}]}"#,
        )
        .unwrap();
        let snapshot = to_snapshot(&parsed).unwrap();
        assert_eq!(snapshot.credits, Some(0.0));
        assert_eq!(snapshot.plan.as_deref(), Some("USD (unavailable)"));
    }

    #[test]
    fn empty_balance_list_is_a_parse_error() {
        let parsed: BalanceResponse =
            serde_json::from_str(r#"{"is_available":true,"balance_infos":[]}"#).unwrap();
        assert!(to_snapshot(&parsed).is_err());
    }
}
