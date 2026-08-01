//! Ported from Sources/CodexBarCore/Providers/XAI/XAIBillingFetcher.swift, prepaid
//! balance only. The 30-day spend history that Swift also fetches has nowhere to go
//! in AgentBar's snapshot model, so it is not requested.
//!
//! The Management API needs a team id as well as the key. AgentBar's per-provider
//! config carries a single string, so the key is written as `MANAGEMENT_KEY:TEAM_ID`
//! (or the team id comes from `XAI_TEAM_ID`, the same variable the Swift app reads).

use async_trait::async_trait;
use serde::Deserialize;

use super::api_token::{api_key, get_json, has_api_key, Auth};
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot};
use crate::config::Config;

const BASE: &str = "https://management-api.x.ai/v1/billing/teams";

pub struct Xai;

#[derive(Debug, Deserialize)]
struct BalanceEnvelope {
    total: Amount,
}

#[derive(Debug, Deserialize)]
struct Amount {
    /// Required: a 200 error envelope must not decode into a $0.00 balance.
    val: String,
}

/// key, team. The team id may be appended to the configured key after a colon.
fn split_credentials(raw: &str) -> (String, Option<String>) {
    match raw.rsplit_once(':') {
        Some((key, team)) if !key.trim().is_empty() && !team.trim().is_empty() => {
            (key.trim().to_string(), Some(team.trim().to_string()))
        }
        _ => (raw.trim().to_string(), None),
    }
}

fn resolve_team(from_key: Option<String>) -> Result<String, ProviderError> {
    let team = from_key
        .or_else(|| std::env::var("XAI_TEAM_ID").ok())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            ProviderError::Auth(
                "missing xAI team id: save the key as MANAGEMENT_KEY:TEAM_ID or set XAI_TEAM_ID"
                    .into(),
            )
        })?;
    if team.contains('/') || team == "." || team == ".." {
        return Err(ProviderError::Auth("invalid xAI team id".into()));
    }
    Ok(team)
}

/// The ledger records credit as negative cents (a $10 top-up is "-1000"), so the
/// remaining balance is the negated cent value in dollars. A body without a
/// parseable total fails loudly: it must never read as $0.00.
fn balance_usd(raw: &str) -> Result<f64, ProviderError> {
    let value = raw.trim();
    let cents: f64 = value.parse().map_err(|_| {
        ProviderError::Parse(format!("balance total.val is not a cent amount: {value}"))
    })?;
    if !cents.is_finite() {
        return Err(ProviderError::Parse(
            "balance total.val is not finite".into(),
        ));
    }
    Ok(-cents / 100.0)
}

#[async_trait]
impl Provider for Xai {
    fn id(&self) -> &'static str {
        "xai"
    }

    fn name(&self) -> &'static str {
        "xAI"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn doc_url(&self) -> &'static str {
        "https://console.x.ai"
    }

    fn env_key(&self) -> Option<&'static str> {
        Some("XAI_MANAGEMENT_API_KEY")
    }

    fn is_configured(&self, config: &Config) -> bool {
        has_api_key(config, self)
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let (key, team_from_key) = split_credentials(&api_key(&ctx.config, self)?);
        let team = resolve_team(team_from_key)?;

        let envelope: BalanceEnvelope = get_json(
            &ctx.http,
            &format!("{BASE}/{team}/prepaid/balance"),
            &Auth::Bearer(&key),
            &[],
        )
        .await?;

        let mut snapshot = UsageSnapshot::new(self.id());
        snapshot.credits = Some(balance_usd(&envelope.total.val)?);
        snapshot.plan = Some("Prepaid".to_string());
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negated_cent_ledger_becomes_dollars() {
        let envelope: BalanceEnvelope =
            serde_json::from_str(r#"{"total":{"val":"-1000"}}"#).unwrap();
        assert_eq!(balance_usd(&envelope.total.val).unwrap(), 10.0);
        assert_eq!(balance_usd("0").unwrap(), 0.0);
        // A team in deficit reads as a negative balance, not as zero.
        assert_eq!(balance_usd("250").unwrap(), -2.5);
        assert!(balance_usd("not-a-number").is_err());
    }

    #[test]
    fn team_id_comes_from_the_key_suffix() {
        let (key, team) = split_credentials("xai-abc123:team-42");
        assert_eq!(key, "xai-abc123");
        assert_eq!(resolve_team(team).unwrap(), "team-42");

        let (key, team) = split_credentials("xai-abc123");
        assert_eq!(key, "xai-abc123");
        assert!(team.is_none());
    }

    #[test]
    fn path_separators_are_rejected_in_the_team_id() {
        assert!(resolve_team(Some("../other".into())).is_err());
    }
}
