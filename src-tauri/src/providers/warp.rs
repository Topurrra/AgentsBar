//! Warp usage, ported from CodexBar `Providers/Warp/WarpUsageFetcher.swift` and
//! `WarpSettingsReader.swift`.
//!
//! Not a cookie provider despite the wave 2 grouping: CodexBar authenticates Warp with a
//! `WARP_API_KEY` bearer token (`WarpSettingsReader.apiKeyEnvironmentKeys`) and its
//! GraphQL endpoint takes no browser session, so there is nothing to import from a
//! cookie database. The key is created at
//! <https://docs.warp.dev/reference/cli/api-keys>, so this stays `AuthKind::ApiKey`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::api_token::{api_key, has_api_key, parse_rfc3339, post_json, Auth};
use super::util::percent;
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow};
use crate::config::Config;

const API_URL: &str = "https://app.warp.dev/graphql/v2?op=GetRequestLimitInfo";
/// Warp's GraphQL endpoint sits behind an edge limiter that answers HTTP 429
/// ("Rate exceeded.") unless the User-Agent matches the official client pattern.
const USER_AGENT: &str = "Warp/1.0";
/// This port is Windows only, so the OS context is constant. Warp uses it for telemetry,
/// not for authorization.
const OS_CATEGORY: &str = "Windows";
const OS_VERSION: &str = "10.0.0";

const QUERY: &str = r"
query GetRequestLimitInfo($requestContext: RequestContext!) {
  user(requestContext: $requestContext) {
    __typename
    ... on UserOutput {
      user {
        requestLimitInfo {
          isUnlimited
          nextRefreshTime
          requestLimit
          requestsUsedSinceLastRefresh
        }
        bonusGrants {
          requestCreditsGranted
          requestCreditsRemaining
          expiration
        }
        workspaces {
          bonusGrantsInfo {
            grants {
              requestCreditsGranted
              requestCreditsRemaining
              expiration
            }
          }
        }
      }
    }
  }
}
";

pub struct Warp;

#[async_trait]
impl Provider for Warp {
    fn id(&self) -> &'static str {
        "warp"
    }

    fn name(&self) -> &'static str {
        "Warp"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn doc_url(&self) -> &'static str {
        "https://docs.warp.dev/reference/cli/api-keys"
    }

    fn env_key(&self) -> Option<&'static str> {
        Some("WARP_API_KEY")
    }

    fn is_configured(&self, config: &Config) -> bool {
        has_api_key(config, self)
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let key = api_key(&ctx.config, self)?;
        let body: Value = post_json(
            &ctx.http,
            API_URL,
            &Auth::Bearer(&key),
            &[
                ("x-warp-client-id", "warp-app"),
                ("x-warp-os-category", OS_CATEGORY),
                ("x-warp-os-name", OS_CATEGORY),
                ("x-warp-os-version", OS_VERSION),
                ("User-Agent", USER_AGENT),
            ],
            &json!({
                "query": QUERY,
                "operationName": "GetRequestLimitInfo",
                "variables": {
                    "requestContext": {
                        "clientContext": {},
                        "osContext": {
                            "category": OS_CATEGORY,
                            "name": OS_CATEGORY,
                            "version": OS_VERSION,
                        },
                    },
                },
            }),
        )
        .await?;

        snapshot(&body)
    }
}

fn int(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
    .unwrap_or(0)
}

fn flag(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => {
            matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
        }
        _ => false,
    }
}

/// GraphQL reports errors with HTTP 200, so they have to be picked out of the body.
fn graphql_error(body: &Value) -> Option<String> {
    let errors = body.get("errors")?.as_array()?;
    if errors.is_empty() {
        return None;
    }
    let messages: Vec<&str> = errors
        .iter()
        .filter_map(|e| e.as_str().or_else(|| e.get("message")?.as_str()))
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .take(3)
        .collect();
    Some(if messages.is_empty() {
        "Warp GraphQL request failed".to_string()
    } else {
        messages.join(" | ")
    })
}

#[derive(Debug, Default, PartialEq)]
struct Bonus {
    remaining: i64,
    granted: i64,
    next_expiration: Option<DateTime<Utc>>,
}

/// User level grants plus every workspace's grants, summed. The earliest expiry among
/// the grants that still hold credits is the one worth showing.
fn bonus(user: &Value) -> Bonus {
    let workspace_grants = user
        .get("workspaces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|w| w.get("bonusGrantsInfo")?.get("grants")?.as_array())
        .flatten();
    let grants = user
        .get("bonusGrants")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(workspace_grants);

    let mut out = Bonus::default();
    for grant in grants {
        let remaining = int(grant.get("requestCreditsRemaining"));
        out.remaining += remaining;
        out.granted += int(grant.get("requestCreditsGranted"));
        let expiry = grant
            .get("expiration")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339);
        if remaining > 0 {
            if let Some(expiry) = expiry {
                out.next_expiration =
                    Some(out.next_expiration.map_or(expiry, |best| best.min(expiry)));
            }
        }
    }
    out
}

fn snapshot(body: &Value) -> Result<UsageSnapshot, ProviderError> {
    if let Some(message) = graphql_error(body) {
        return Err(ProviderError::Auth(message));
    }
    let user = body
        .get("data")
        .and_then(|d| d.get("user"))
        .ok_or_else(|| ProviderError::Parse("Warp response has no data.user".into()))?;
    let inner = user.get("user").ok_or_else(|| {
        match user.get("__typename").and_then(Value::as_str).unwrap_or("") {
            "" | "UserOutput" => {
                ProviderError::Parse("Warp response has no requestLimitInfo".into())
            }
            other => ProviderError::Auth(format!("Warp returned user type {other}")),
        }
    })?;
    let limit_info = inner
        .get("requestLimitInfo")
        .ok_or_else(|| ProviderError::Parse("Warp response has no requestLimitInfo".into()))?;

    let unlimited = flag(limit_info.get("isUnlimited"));
    let limit = int(limit_info.get("requestLimit"));
    let used = int(limit_info.get("requestsUsedSinceLastRefresh"));
    let resets_at = limit_info
        .get("nextRefreshTime")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339);

    let mut snap = UsageSnapshot::new("warp");
    snap.primary = Some(UsageWindow::new(
        "Credits",
        Some(if unlimited {
            0.0
        } else {
            percent(used as f64, limit as f64)
        }),
        if unlimited { None } else { resets_at },
        None,
    ));
    snap.plan = Some(if unlimited {
        "Unlimited".to_string()
    } else {
        format!("{used}/{limit} credits")
    });

    let bonus = bonus(inner);
    if bonus.granted > 0 || bonus.remaining > 0 {
        snap.secondary = Some(UsageWindow::new(
            "Add-on credits",
            Some(if bonus.granted > 0 {
                percent(
                    (bonus.granted - bonus.remaining) as f64,
                    bonus.granted as f64,
                )
            } else {
                0.0
            }),
            bonus.next_expiration,
            None,
        ));
        snap.credits = Some(bonus.remaining as f64);
    }
    Ok(snap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<UsageSnapshot, ProviderError> {
        snapshot(&serde_json::from_str(json).unwrap())
    }

    #[test]
    fn request_limit_maps_to_the_primary_lane() {
        let snap = parse(
            r#"{"data":{"user":{"__typename":"UserOutput","user":{"requestLimitInfo":{
                "isUnlimited":false,"requestLimit":2500,"requestsUsedSinceLastRefresh":500,
                "nextRefreshTime":"2026-08-15T00:00:00Z"}}}}}"#,
        )
        .unwrap();
        let primary = snap.primary.unwrap();
        assert_eq!(primary.used_percent, Some(20.0));
        assert_eq!(
            primary.resets_at.map(|d| d.timestamp()),
            Some(1_786_752_000)
        );
        assert_eq!(snap.plan.as_deref(), Some("500/2500 credits"));
        assert!(snap.secondary.is_none());
    }

    #[test]
    fn unlimited_plans_read_as_zero_used_with_no_reset() {
        let snap = parse(
            r#"{"data":{"user":{"user":{"requestLimitInfo":{"isUnlimited":true,
                "requestLimit":0,"requestsUsedSinceLastRefresh":0,
                "nextRefreshTime":"2026-08-15T00:00:00Z"}}}}}"#,
        )
        .unwrap();
        let primary = snap.primary.unwrap();
        assert_eq!(primary.used_percent, Some(0.0));
        assert!(primary.resets_at.is_none());
        assert_eq!(snap.plan.as_deref(), Some("Unlimited"));
    }

    #[test]
    fn bonus_grants_are_summed_across_user_and_workspaces() {
        let snap = parse(
            r#"{"data":{"user":{"user":{
                "requestLimitInfo":{"isUnlimited":false,"requestLimit":"100",
                    "requestsUsedSinceLastRefresh":"25"},
                "bonusGrants":[
                    {"requestCreditsGranted":100,"requestCreditsRemaining":40,
                     "expiration":"2026-12-01T00:00:00Z"},
                    {"requestCreditsGranted":50,"requestCreditsRemaining":0,
                     "expiration":"2026-09-01T00:00:00Z"}],
                "workspaces":[{"bonusGrantsInfo":{"grants":[
                    {"requestCreditsGranted":50,"requestCreditsRemaining":10,
                     "expiration":"2026-10-01T00:00:00Z"}]}}]}}}}"#,
        )
        .unwrap();
        // Quoted numbers still count.
        assert_eq!(snap.primary.unwrap().used_percent, Some(25.0));
        let secondary = snap.secondary.unwrap();
        // 200 granted, 50 remaining.
        assert_eq!(secondary.used_percent, Some(75.0));
        assert_eq!(snap.credits, Some(50.0));
        // The spent grant expiring in September must not win over the two live ones.
        assert_eq!(
            secondary.resets_at.map(|d| d.timestamp()),
            Some(1_790_812_800)
        );
    }

    #[test]
    fn graphql_errors_surface_instead_of_an_empty_snapshot() {
        let err = parse(r#"{"errors":[{"message":"Unauthorized"}],"data":null}"#).unwrap_err();
        assert!(matches!(err, ProviderError::Auth(m) if m == "Unauthorized"));
        assert!(parse(r#"{"data":{"user":{"__typename":"UserOutput"}}}"#).is_err());
        assert!(matches!(
            parse(r#"{"data":{"user":{"__typename":"UserErrorOutput"}}}"#).unwrap_err(),
            ProviderError::Auth(_)
        ));
    }

    /// cargo test -p agentbar warp_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs a real Warp API key in the AgentBar config"]
    async fn warp_live() {
        let ctx = FetchContext {
            http: reqwest::Client::new(),
            config: Config::load(),
        };
        match Warp.fetch(&ctx).await {
            Ok(snap) => println!(
                "warp: plan={:?} primary={:?} secondary={:?}",
                snap.plan, snap.primary, snap.secondary
            ),
            Err(e) => println!("warp: {e}"),
        }
    }
}
