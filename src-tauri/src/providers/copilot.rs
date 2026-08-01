//! GitHub Copilot usage, ported from CodexBar `Providers/Copilot/CopilotUsageFetcher.swift`
//! and `CopilotUsageModels.swift`.
//!
//! The GitHub OAuth token is the one the Copilot editor plugins store in
//! `%LOCALAPPDATA%\github-copilot\apps.json` (older clients: `hosts.json`).
//! It is sent as-is to the Copilot internal user endpoint, which reports the
//! premium interaction and chat quotas plus the plan name.

use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::Deserialize;
use serde_json::Value;

use super::util::local_app_data;
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow};
use crate::config::Config;

const USAGE_URL: &str = "https://api.github.com/copilot_internal/user";

fn copilot_dir() -> PathBuf {
    local_app_data().join("github-copilot")
}

/// The GitHub OAuth token plus the login it belongs to, from the Copilot plugin store.
struct Credentials {
    token: String,
    user: Option<String>,
}

/// `apps.json` keys look like `github.com:Iv1.b507a08c87ecfe98`, `hosts.json` keys are
/// bare hosts. Both map to an object with `oauth_token` and `user`.
fn read_credentials() -> Option<Credentials> {
    let dir = copilot_dir();
    for name in ["apps.json", "hosts.json"] {
        let Ok(text) = std::fs::read_to_string(dir.join(name)) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(entries) = json.as_object() else {
            continue;
        };
        let found = entries
            .iter()
            .filter(|(key, _)| key.starts_with("github.com"))
            .chain(entries.iter())
            .find_map(|(_, value)| {
                let token = value["oauth_token"].as_str()?.trim();
                if token.is_empty() {
                    return None;
                }
                Some(Credentials {
                    token: token.to_string(),
                    user: value["user"].as_str().map(str::to_string),
                })
            });
        if found.is_some() {
            return found;
        }
    }
    None
}

pub struct Copilot;

#[async_trait]
impl Provider for Copilot {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn name(&self) -> &'static str {
        "Copilot"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::OauthFile
    }

    fn doc_url(&self) -> &'static str {
        "https://docs.github.com/copilot"
    }

    fn is_configured(&self, _config: &Config) -> bool {
        read_credentials().is_some()
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let creds = read_credentials().ok_or(ProviderError::NotConfigured)?;
        let response = ctx
            .http
            .get(USAGE_URL)
            .header("Authorization", format!("token {}", creds.token))
            .header("Accept", "application/json")
            .header("Editor-Version", "vscode/1.96.2")
            .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
            .header("User-Agent", "GitHubCopilotChat/0.26.7")
            .header("X-Github-Api-Version", "2025-04-01")
            .send()
            .await?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ProviderError::Auth(
                "GitHub rejected the stored Copilot token, sign in again in your editor".into(),
            ));
        }
        if !status.is_success() {
            return Err(super::util::http_error(&response, || {
                format!("Copilot usage request failed with HTTP {}", status.as_u16())
            }));
        }

        let body: UsageResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::Parse(format!("copilot usage response: {e}")))?;
        snapshot(body, creds.user)
    }
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    #[serde(default)]
    quota_snapshots: Quotas,
    #[serde(default)]
    copilot_plan: Option<String>,
    #[serde(default)]
    token_based_billing: bool,
    #[serde(default)]
    quota_reset_date: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Quotas {
    #[serde(default)]
    premium_interactions: Option<Quota>,
    #[serde(default)]
    chat: Option<Quota>,
}

#[derive(Debug, Deserialize)]
struct Quota {
    #[serde(default)]
    entitlement: Option<Value>,
    #[serde(default)]
    remaining: Option<Value>,
    #[serde(default)]
    percent_remaining: Option<Value>,
    #[serde(default)]
    unlimited: bool,
}

/// GitHub sends these numbers as numbers or as numeric strings depending on the plan.
fn number(value: &Option<Value>) -> Option<f64> {
    let value = value.as_ref()?;
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
}

impl Quota {
    /// Percent of the quota already used, or None when the snapshot carries no signal.
    /// Zero entitlement with zero remaining is GitHub's placeholder for seats that are
    /// not metered at all, rendering that as "0% used" would be a lie.
    fn used_percent(&self) -> Option<f64> {
        if self.unlimited {
            return None;
        }
        let entitlement = number(&self.entitlement);
        let remaining = number(&self.remaining);
        if entitlement == Some(0.0) && remaining == Some(0.0) {
            return None;
        }
        let percent_remaining = match number(&self.percent_remaining) {
            Some(p) => p,
            None => match (entitlement, remaining) {
                (Some(e), Some(r)) if e > 0.0 => r / e * 100.0,
                _ => return None,
            },
        };
        Some(100.0 - percent_remaining)
    }
}

fn window(
    label: &str,
    quota: &Option<Quota>,
    resets_at: Option<DateTime<Utc>>,
) -> Option<UsageWindow> {
    let used_percent = quota.as_ref()?.used_percent()?;
    Some(UsageWindow::new(label, Some(used_percent), resets_at, None))
}

fn snapshot(body: UsageResponse, user: Option<String>) -> Result<UsageSnapshot, ProviderError> {
    let resets_at = body.quota_reset_date.as_deref().and_then(parse_reset);
    let premium = window(
        "Premium",
        &body.quota_snapshots.premium_interactions,
        resets_at,
    );
    let chat = window("Chat", &body.quota_snapshots.chat, resets_at);
    let unlimited = body
        .quota_snapshots
        .premium_interactions
        .as_ref()
        .is_some_and(|q| q.unlimited)
        || body
            .quota_snapshots
            .chat
            .as_ref()
            .is_some_and(|q| q.unlimited);

    let mut snap = UsageSnapshot::new("copilot");
    snap.account = user;
    snap.plan = body.copilot_plan.as_deref().map(capitalized);
    match (premium, chat) {
        // Premium interactions are the metered quota, chat is the secondary window.
        (Some(premium), chat) => {
            snap.primary = Some(premium);
            snap.secondary = chat;
        }
        // Chat-only plans keep chat in the secondary slot so the labels stay honest.
        (None, Some(chat)) => snap.secondary = Some(chat),
        // Token based billing and explicitly unlimited seats are not metered windows.
        (None, None) if body.token_based_billing || unlimited => {}
        (None, None) => return Err(ProviderError::Parse("no Copilot quota in response".into())),
    }
    Ok(snap)
}

fn capitalized(plan: &str) -> String {
    let mut chars = plan.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The reset date is an RFC 3339 timestamp on most plans and a bare `yyyy-MM-dd` on some.
fn parse_reset(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(date) = DateTime::parse_from_rfc3339(raw) {
        return Some(date.with_timezone(&Utc));
    }
    let day = NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()?;
    Some(Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<UsageSnapshot, ProviderError> {
        snapshot(serde_json::from_str(json).unwrap(), Some("octocat".into()))
    }

    #[test]
    fn premium_quota_maps_to_primary() {
        let snap = parse(
            r#"{"copilot_plan":"individual","quota_reset_date":"2033-09-01",
                "quota_snapshots":{"premium_interactions":{"entitlement":300,"remaining":75,
                "percent_remaining":25,"unlimited":false},
                "chat":{"entitlement":100,"remaining":90,"unlimited":false}}}"#,
        )
        .unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Individual"));
        assert_eq!(snap.primary.as_ref().unwrap().used_percent, Some(75.0));
        assert_eq!(snap.secondary.as_ref().unwrap().used_percent, Some(10.0));
        assert!(snap.primary.as_ref().unwrap().resets_at.is_some());
    }

    #[test]
    fn placeholder_and_unlimited_quotas_are_dropped() {
        let snap = parse(
            r#"{"copilot_plan":"business","token_based_billing":true,
                "quota_snapshots":{"premium_interactions":{"entitlement":0,"remaining":0,
                "percent_remaining":100},"chat":{"unlimited":true,"percent_remaining":100}}}"#,
        )
        .unwrap();
        assert!(snap.primary.is_none() && snap.secondary.is_none());
        assert_eq!(snap.plan.as_deref(), Some("Business"));
    }

    #[test]
    fn chat_only_plan_keeps_chat_secondary() {
        let snap = parse(
            r#"{"copilot_plan":"free","quota_snapshots":{"chat":{"entitlement":"50","remaining":"20"}}}"#,
        )
        .unwrap();
        assert!(snap.primary.is_none());
        assert_eq!(snap.secondary.as_ref().unwrap().used_percent, Some(60.0));
    }

    #[test]
    fn missing_quota_is_an_error() {
        assert!(parse(r#"{"copilot_plan":"free","quota_snapshots":{}}"#).is_err());
    }

    /// Hits the real Copilot endpoint with the token the editor plugin stored.
    /// cargo test -p agentsbar copilot_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn copilot_live() {
        let ctx = FetchContext {
            http: reqwest::Client::new(),
            config: Config::default(),
        };
        let snap = Copilot.fetch(&ctx).await.expect("copilot fetch");
        println!(
            "plan={:?} has_account={} primary={:?} secondary={:?}",
            snap.plan,
            snap.account.is_some(),
            snap.primary,
            snap.secondary
        );
    }
}
