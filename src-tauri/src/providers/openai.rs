//! OpenAI platform spend, ported from CodexBar `Providers/OpenAI/OpenAIAPIUsageFetcher.swift`
//! and `OpenAIAPICreditBalanceFetcher.swift`.
//!
//! The organization admin key reports cost per UTC day; there is no quota to draw a bar
//! from, so the 30 day spend goes into the plan line. Legacy user keys that still
//! answer the billing credit grants endpoint do have a granted/used ratio, and that path
//! fills the primary window and the credits field.

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveTime, Utc};
use serde::Deserialize;

use super::api_token::{api_key, has_api_key};
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow};
use crate::config::Config;

const COSTS_URL: &str = "https://api.openai.com/v1/organization/costs";
const CREDIT_GRANTS_URL: &str = "https://api.openai.com/v1/dashboard/billing/credit_grants";
const HISTORY_DAYS: i64 = 30;
/// The costs endpoint refuses more than 31 daily buckets per request.
const MAX_PAGES: usize = 20;

pub struct OpenAi;

#[async_trait]
impl Provider for OpenAi {
    fn id(&self) -> &'static str {
        "openai"
    }

    fn name(&self) -> &'static str {
        "OpenAI"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn doc_url(&self) -> &'static str {
        "https://platform.openai.com/settings/organization/admin-keys"
    }

    fn env_key(&self) -> Option<&'static str> {
        Some("OPENAI_API_KEY")
    }

    fn is_configured(&self, config: &Config) -> bool {
        has_api_key(config, self)
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let key = api_key(&ctx.config, self)?;

        match fetch_spend(&ctx.http, &key).await {
            Ok(spend) => {
                let mut snap = UsageSnapshot::new("openai");
                // `account` is an email or account label, so the spend rides along in
                // `plan`, the same free text slot deepgram uses for its usage line.
                snap.plan = Some(format!("Admin API, ${spend:.2} last {HISTORY_DAYS} days"));
                Ok(snap)
            }
            // Project and user keys cannot read organization costs. The older billing
            // endpoint still answers for some of them and carries a real balance.
            Err(err @ ProviderError::Auth(_)) => match fetch_balance(&ctx.http, &key).await {
                Ok(snap) => Ok(snap),
                Err(_) => Err(err),
            },
            Err(err) => Err(err),
        }
    }
}

#[derive(Debug, Deserialize)]
struct CostsResponse {
    #[serde(default)]
    data: Vec<CostBucket>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    next_page: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CostBucket {
    #[serde(default)]
    results: Vec<CostResult>,
}

#[derive(Debug, Deserialize)]
struct CostResult {
    #[serde(default)]
    amount: Option<Amount>,
}

#[derive(Debug, Deserialize)]
struct Amount {
    #[serde(default)]
    value: Option<f64>,
}

/// Total USD billed over the trailing `HISTORY_DAYS` UTC days.
async fn fetch_spend(http: &reqwest::Client, key: &str) -> Result<f64, ProviderError> {
    let today = Utc::now().date_naive().and_time(NaiveTime::MIN).and_utc();
    let start = (today - Duration::days(HISTORY_DAYS - 1)).timestamp();
    let end = (today + Duration::days(1)).timestamp();

    let mut total = 0.0;
    let mut page: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut request = http
            .get(COSTS_URL)
            .bearer_auth(key)
            .header("Accept", "application/json")
            .query(&[
                ("start_time", start.to_string()),
                ("end_time", end.to_string()),
                ("bucket_width", "1d".to_string()),
                ("limit", HISTORY_DAYS.to_string()),
            ]);
        if let Some(cursor) = &page {
            request = request.query(&[("page", cursor)]);
        }

        let response = request.send().await?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ProviderError::Auth(
                "OpenAI rejected this key for organization usage, an admin key is required".into(),
            ));
        }
        if !status.is_success() {
            return Err(super::util::http_error(&response, || {
                format!("OpenAI costs request failed with HTTP {}", status.as_u16())
            }));
        }

        let body: CostsResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::Parse(format!("openai costs response: {e}")))?;
        total += sum_costs(&body);

        match (body.has_more, body.next_page) {
            (true, Some(cursor)) if Some(&cursor) != page.as_ref() => page = Some(cursor),
            (true, _) => {
                return Err(ProviderError::Parse(
                    "openai costs pagination stalled".into(),
                ))
            }
            (false, _) => return Ok(total),
        }
    }
    Err(ProviderError::Parse(
        "openai costs pagination too long".into(),
    ))
}

fn sum_costs(body: &CostsResponse) -> f64 {
    body.data
        .iter()
        .flat_map(|bucket| bucket.results.iter())
        .filter_map(|result| result.amount.as_ref()?.value)
        .sum()
}

#[derive(Debug, Deserialize)]
struct CreditGrants {
    #[serde(default)]
    total_granted: f64,
    #[serde(default)]
    total_used: f64,
    #[serde(default)]
    total_available: f64,
    #[serde(default)]
    grants: Option<GrantList>,
}

#[derive(Debug, Deserialize)]
struct GrantList {
    #[serde(default)]
    data: Vec<Grant>,
}

#[derive(Debug, Deserialize)]
struct Grant {
    #[serde(default)]
    expires_at: Option<i64>,
}

async fn fetch_balance(http: &reqwest::Client, key: &str) -> Result<UsageSnapshot, ProviderError> {
    let response = http
        .get(CREDIT_GRANTS_URL)
        .bearer_auth(key)
        .header("Accept", "application/json")
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(super::util::http_error(&response, || {
            format!(
                "OpenAI credit balance request failed with HTTP {}",
                status.as_u16()
            )
        }));
    }
    let grants: CreditGrants = response
        .json()
        .await
        .map_err(|e| ProviderError::Parse(format!("openai credit grants response: {e}")))?;
    Ok(balance_snapshot(grants, Utc::now()))
}

fn balance_snapshot(grants: CreditGrants, now: DateTime<Utc>) -> UsageSnapshot {
    let used_percent = if grants.total_granted > 0.0 {
        grants.total_used / grants.total_granted * 100.0
    } else if grants.total_available > 0.0 {
        0.0
    } else {
        100.0
    };
    let next_expiry = grants
        .grants
        .map(|list| list.data)
        .unwrap_or_default()
        .iter()
        .filter_map(|grant| DateTime::from_timestamp(grant.expires_at?, 0))
        .filter(|expiry| *expiry > now)
        .min();

    let mut snap = UsageSnapshot::new("openai");
    snap.primary = Some(UsageWindow::at(
        "Credits",
        Some(used_percent),
        next_expiry,
        None,
        now,
    ));
    snap.credits = Some(grants.total_available);
    snap.plan = Some("API credits".to_string());
    snap
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn costs_are_summed_across_buckets_and_results() {
        let body: CostsResponse = serde_json::from_str(
            r#"{"data":[{"results":[{"amount":{"value":1.5}},{"amount":{"value":2.25}}]},
                {"results":[{"amount":null}]}],"has_more":false}"#,
        )
        .unwrap();
        assert_eq!(sum_costs(&body), 3.75);
    }

    #[test]
    fn balance_maps_used_ratio_and_available_credits() {
        let grants: CreditGrants = serde_json::from_str(
            r#"{"total_granted":100,"total_used":25,"total_available":75,
                "grants":{"data":[{"expires_at":4102444800}]}}"#,
        )
        .unwrap();
        let snap = balance_snapshot(grants, Utc::now());
        assert_eq!(snap.primary.as_ref().unwrap().used_percent, Some(25.0));
        assert_eq!(snap.credits, Some(75.0));
        assert!(snap.primary.as_ref().unwrap().resets_at.is_some());
    }

    #[test]
    fn exhausted_balance_without_grants_reads_as_full() {
        let grants: CreditGrants = serde_json::from_str(r#"{}"#).unwrap();
        let snap = balance_snapshot(grants, Utc::now());
        assert_eq!(snap.primary.as_ref().unwrap().used_percent, Some(100.0));
    }

    /// Needs an admin key in the AgentBar config.
    /// cargo test -p agentbar openai_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn openai_live() {
        let ctx = FetchContext {
            http: reqwest::Client::new(),
            config: Config::load(),
        };
        let snap = OpenAi.fetch(&ctx).await.expect("openai fetch");
        println!(
            "plan={:?} account={:?} credits={:?} primary={:?}",
            snap.plan, snap.account, snap.credits, snap.primary
        );
    }
}
