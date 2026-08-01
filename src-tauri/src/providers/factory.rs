//! Factory (Droid) usage, authenticated by a browser session cookie.
//!
//! Ported from `Sources/CodexBarCore/Providers/Factory/FactoryStatusProbe.swift`.
//!
//! Two shapes of account exist and both are handled:
//! - accounts on token rate limits billing, whose `api.factory.ai/api/billing/limits`
//!   carries rolling 5h / weekly / monthly pools,
//! - everyone else, whose `/api/organization/subscription/usage` carries standard and
//!   premium token counters for the billing period.
//!
//! Two ways in are handled as well: a `factory.ai` session cookie, or, when the browser
//! only holds the WorkOS AuthKit session, the WorkOS refresh grant at
//! `api.workos.com/user_management/authenticate` which returns a bearer token.
//!
//! Never log the header or the bearer token, only cookie names and counts.

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use super::api_token::{epoch_to_utc, parse_rfc3339};
use super::cursor::{parse_json, web_get, web_post};
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow, Want};
use crate::config::Config;

const APP: &str = "https://app.factory.ai";
const API: &str = "https://api.factory.ai";
const WORKOS_AUTH: &str = "https://api.workos.com/user_management/authenticate";

const DOMAINS: &[&str] = &["factory.ai"];
/// `FactoryStatusProbe.sessionCookieNames` plus the auth.js session names it also accepts.
const SESSION_NAMES: &[&str] = &[
    "session",
    "wos-session",
    "__Secure-authjs.session-token",
    "authjs.session-token",
    "__Secure-next-auth.session-token",
    "next-auth.session-token",
];

const WORKOS_DOMAINS: &[&str] = &["workos.com"];
/// The AuthKit session names that mark a workos.com jar as signed in. The whole jar then
/// travels, which is what CodexBar does and what WorkOS expects.
const WORKOS_NAMES: &[&str] = &["wos-session", "__Secure-wos-session"];
/// Factory's two WorkOS client ids, tried in order (FactoryStatusProbe.workosClientIDs).
const WORKOS_CLIENT_IDS: [&str; 2] = [
    "client_01HXRMBQ9BJ3E7QSTQ9X2PHVB7",
    "client_01HNM792M5G5G1A2THWPXKFMXB",
];

const SIGNIN_HINT: &str =
    "Factory session expired. Sign in again at app.factory.ai, or paste a cookie header in Settings";

pub struct Factory;

// ------------------------------------------------------------------ API models

#[derive(Debug, Default, Deserialize)]
struct AuthResponse {
    organization: Option<Organization>,
    #[serde(rename = "userProfile")]
    user_profile: Option<UserProfile>,
}

#[derive(Debug, Deserialize)]
struct Organization {
    name: Option<String>,
    subscription: Option<Subscription>,
}

#[derive(Debug, Deserialize)]
struct Subscription {
    #[serde(rename = "factoryTier")]
    factory_tier: Option<String>,
    #[serde(rename = "orbSubscription")]
    orb: Option<OrbSubscription>,
}

#[derive(Debug, Deserialize)]
struct OrbSubscription {
    plan: Option<Plan>,
}

#[derive(Debug, Deserialize)]
struct Plan {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UserProfile {
    id: Option<String>,
    email: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct UsageResponse {
    usage: Option<UsageData>,
}

#[derive(Debug, Deserialize)]
struct UsageData {
    #[serde(rename = "endDate")]
    end_date: Option<i64>,
    standard: Option<TokenUsage>,
    premium: Option<TokenUsage>,
}

#[derive(Debug, Deserialize)]
struct TokenUsage {
    #[serde(rename = "userTokens")]
    user_tokens: Option<i64>,
    #[serde(rename = "totalAllowance")]
    total_allowance: Option<i64>,
    #[serde(rename = "usedRatio")]
    used_ratio: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct BillingLimits {
    #[serde(rename = "usesTokenRateLimitsBilling", default)]
    uses_token_rate_limits: bool,
    limits: Option<TokenRateLimits>,
    #[serde(rename = "extraUsageBalanceCents")]
    extra_usage_balance_cents: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TokenRateLimits {
    standard: LimitPool,
}

#[derive(Debug, Deserialize)]
struct LimitPool {
    #[serde(rename = "fiveHour")]
    five_hour: BillingWindow,
    weekly: BillingWindow,
    monthly: BillingWindow,
}

#[derive(Debug, Default, Deserialize)]
struct BillingWindow {
    #[serde(rename = "usedPercent", default)]
    used_percent: f64,
    /// Epoch seconds, epoch milliseconds or ISO 8601 depending on the account.
    #[serde(rename = "windowEnd")]
    window_end: Option<Value>,
    #[serde(rename = "secondsRemaining")]
    seconds_remaining: Option<f64>,
}

/// A response's `windowEnd` is any of ISO 8601, epoch seconds or epoch milliseconds.
fn flexible_date(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(s) => {
            parse_rfc3339(s).or_else(|| s.trim().parse::<f64>().ok().and_then(epoch))
        }
        Value::Number(n) => epoch(n.as_f64()?),
        _ => None,
    }
}

fn epoch(raw: f64) -> Option<DateTime<Utc>> {
    (raw.is_finite() && raw > 0.0)
        .then(|| epoch_to_utc(raw as i64))
        .flatten()
}

impl BillingWindow {
    fn resets_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if let Some(seconds) = self.seconds_remaining.filter(|s| *s > 0.0) {
            return Some(now + Duration::seconds(seconds as i64));
        }
        self.window_end
            .as_ref()
            .and_then(flexible_date)
            .filter(|end| *end > now)
    }

    /// Factory leaves stale numbers behind after a short rolling window expires. Its own
    /// web UI reads that state as reset, so a window with a spent `windowEnd` and no
    /// countdown reports zero rather than the stale figure.
    fn used_percent(&self, now: DateTime<Utc>) -> f64 {
        if self.resets_at(now).is_none()
            && self.window_end.is_some()
            && self.seconds_remaining.is_none()
        {
            return 0.0;
        }
        self.used_percent
    }

    fn to_window(&self, label: &str, minutes: Option<u64>, now: DateTime<Utc>) -> UsageWindow {
        UsageWindow::at(
            label,
            Some(self.used_percent(now)),
            self.resets_at(now),
            minutes,
            now,
        )
    }
}

// ------------------------------------------------------------------ mapping

/// Allowances above a trillion tokens mean "unlimited" rather than a real ceiling.
const UNLIMITED_THRESHOLD: i64 = 1_000_000_000_000;

/// `FactoryStatusSnapshot.calculateUsagePercent`: the API ratio wins when it is
/// trustworthy, otherwise the raw counters do.
fn usage_percent(used: i64, allowance: i64, ratio: Option<f64>) -> f64 {
    let allowance_reliable = allowance > 0 && allowance <= UNLIMITED_THRESHOLD;
    if let Some(ratio) = ratio.filter(|r| r.is_finite()) {
        // A zero ratio next to real usage and a real allowance is the API lagging.
        let stale_zero = ratio == 0.0 && used > 0 && allowance_reliable;
        if !stale_zero {
            if (-0.001..=1.001).contains(&ratio) {
                return (ratio * 100.0).clamp(0.0, 100.0);
            }
            if !allowance_reliable && (-0.1..=100.1).contains(&ratio) {
                return ratio.clamp(0.0, 100.0);
            }
        }
    }
    if allowance > UNLIMITED_THRESHOLD {
        // Unlimited plans get a pseudo gauge against a 100M token reference.
        return (used as f64 / 100_000_000.0 * 100.0).min(100.0);
    }
    if allowance <= 0 {
        return 0.0;
    }
    super::util::percent(used as f64, allowance as f64)
}

/// "Factory Team - Enterprise", skipping a plan name that only repeats the tier.
fn plan_label(auth: &AuthResponse) -> Option<String> {
    let subscription = auth.organization.as_ref()?.subscription.as_ref();
    let mut parts = Vec::new();
    if let Some(tier) = subscription
        .and_then(|s| s.factory_tier.as_deref())
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        let mut chars = tier.chars();
        let head = chars
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        parts.push(format!("Factory {head}{}", chars.as_str().to_lowercase()));
    }
    if let Some(plan) = subscription
        .and_then(|s| s.orb.as_ref())
        .and_then(|o| o.plan.as_ref())
        .and_then(|p| p.name.as_deref())
        .map(str::trim)
        .filter(|p| !p.is_empty() && !p.to_lowercase().contains("factory"))
    {
        parts.push(plan.to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" - "))
}

fn account_label(auth: &AuthResponse) -> Option<String> {
    auth.user_profile
        .as_ref()
        .and_then(|u| u.email.as_deref())
        .or_else(|| auth.organization.as_ref().and_then(|o| o.name.as_deref()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn rate_limit_snapshot(
    auth: &AuthResponse,
    limits: &BillingLimits,
    pools: &TokenRateLimits,
) -> UsageSnapshot {
    let now = Utc::now();
    let mut snapshot = UsageSnapshot::new("factory");
    snapshot.primary = Some(pools.standard.five_hour.to_window("5h", Some(5 * 60), now));
    snapshot.secondary = Some(
        pools
            .standard
            .weekly
            .to_window("7-day", Some(7 * 24 * 60), now),
    );
    snapshot.tertiary = Some(pools.standard.monthly.to_window("Monthly", None, now));
    snapshot.credits = limits
        .extra_usage_balance_cents
        .map(|cents| cents as f64 / 100.0);
    snapshot.plan = plan_label(auth);
    snapshot.account = account_label(auth);
    snapshot
}

fn usage_snapshot(auth: &AuthResponse, usage: &UsageResponse) -> UsageSnapshot {
    let now = Utc::now();
    let data = usage.usage.as_ref();
    let resets_at = data.and_then(|d| d.end_date).and_then(epoch_to_utc);
    // A missing pool counts as an empty one rather than as a missing lane.
    let lane = |pool: Option<&TokenUsage>, label: &str| {
        UsageWindow::at(
            label,
            Some(usage_percent(
                pool.and_then(|p| p.user_tokens).unwrap_or(0),
                pool.and_then(|p| p.total_allowance).unwrap_or(0),
                pool.and_then(|p| p.used_ratio),
            )),
            resets_at,
            // The billing period has no fixed duration, only an end.
            None,
            now,
        )
    };

    let mut snapshot = UsageSnapshot::new("factory");
    snapshot.primary = Some(lane(data.and_then(|d| d.standard.as_ref()), "Standard"));
    snapshot.secondary = Some(lane(data.and_then(|d| d.premium.as_ref()), "Premium"));
    snapshot.plan = plan_label(auth);
    snapshot.account = account_label(auth);
    snapshot
}

// ------------------------------------------------------------------ provider

/// The header set every Factory API call carries; `x-factory-client` is required.
fn factory_headers<'a>(auth: &'a str, auth_value: &'a str) -> [(&'a str, &'a str); 6] {
    [
        ("Accept", "application/json"),
        ("Content-Type", "application/json"),
        ("Origin", APP),
        ("Referer", "https://app.factory.ai/"),
        ("x-factory-client", "web-app"),
        (auth, auth_value),
    ]
}

async fn fetch_with(
    ctx: &FetchContext,
    base: &str,
    header: (&str, &str),
) -> Result<UsageSnapshot, ProviderError> {
    let headers = factory_headers(header.0, header.1);
    let auth: AuthResponse = parse_json(
        &web_get(
            &ctx.http,
            &format!("{base}/api/app/auth/me"),
            &headers,
            SIGNIN_HINT,
        )
        .await?,
    )?;

    // Accounts on the newer rolling pools report them here; everyone else 404s or
    // returns a flag that is false, in which case the period counters are the truth.
    let limits: Option<BillingLimits> = web_get(
        &ctx.http,
        &format!("{API}/api/billing/limits"),
        &headers,
        SIGNIN_HINT,
    )
    .await
    .ok()
    .and_then(|body| parse_json(&body).ok());
    if let Some(limits) = limits.filter(|l| l.uses_token_rate_limits) {
        if let Some(pools) = limits.limits.as_ref() {
            return Ok(rate_limit_snapshot(&auth, &limits, pools));
        }
    }

    let mut url = format!("{base}/api/organization/subscription/usage?useCache=true");
    if let Some(id) = auth
        .user_profile
        .as_ref()
        .and_then(|u| u.id.as_deref())
        .filter(|id| !id.trim().is_empty())
    {
        url.push_str(&format!("&userId={id}"));
    }
    let usage: UsageResponse = parse_json(&web_get(&ctx.http, &url, &headers, SIGNIN_HINT).await?)?;
    Ok(usage_snapshot(&auth, &usage))
}

/// Exchanges the browser's WorkOS AuthKit session for a Factory bearer token. The refresh
/// token itself never leaves WorkOS: `useCookie` tells it to read the session cookie.
async fn workos_bearer(ctx: &FetchContext, cookie: &str) -> Result<String, ProviderError> {
    let mut last = ProviderError::Auth("WorkOS auth failed".into());
    for client_id in WORKOS_CLIENT_IDS {
        let body = json!({
            "client_id": client_id,
            "grant_type": "refresh_token",
            "useCookie": true,
        });
        match web_post(
            &ctx.http,
            WORKOS_AUTH,
            &[("Accept", "application/json"), ("Cookie", cookie)],
            &body,
            SIGNIN_HINT,
        )
        .await
        {
            Ok(text) => match serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("access_token")?.as_str().map(str::to_string))
            {
                Some(token) => return Ok(token),
                None => last = ProviderError::Parse("WorkOS returned no access token".into()),
            },
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[async_trait]
impl Provider for Factory {
    fn id(&self) -> &'static str {
        "factory"
    }

    fn name(&self) -> &'static str {
        "Factory"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::Cookie
    }

    fn doc_url(&self) -> &'static str {
        "https://app.factory.ai/settings/billing"
    }

    fn is_configured(&self, config: &Config) -> bool {
        super::has_cookies(config, self.id(), DOMAINS, Want::Jar(SESSION_NAMES))
            || super::has_cookies(config, self.id(), WORKOS_DOMAINS, Want::Jar(WORKOS_NAMES))
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        // Same gate as before: only the ABSENCE of a Factory cookie falls through to
        // WorkOS. A cookie that exists but fails is still a Factory failure, and now the
        // walk gets to try every browser that holds one before giving up.
        if super::has_cookies(&ctx.config, self.id(), DOMAINS, Want::Jar(SESSION_NAMES)) {
            return ctx
                .with_cookies(
                    self.id(),
                    DOMAINS,
                    Want::Jar(SESSION_NAMES),
                    SIGNIN_HINT,
                    |cookie| async move {
                        // app.factory.ai serves the session cookie; api.factory.ai answers
                        // the same routes and is the one that works for some org configs.
                        let first = fetch_with(ctx, APP, ("Cookie", cookie.as_str())).await;
                        if first.is_ok() {
                            return first;
                        }
                        if let Ok(snapshot) =
                            fetch_with(ctx, API, ("Cookie", cookie.as_str())).await
                        {
                            return Ok(snapshot);
                        }
                        first
                    },
                )
                .await;
        }

        // No Factory cookie at all: the browser may still hold the WorkOS session that
        // Factory signs in through. That one stays on the single-candidate shim, because
        // the WorkOS cookie is exchanged for a bearer token rather than sent as a session.
        let workos = ctx
            .cookie_header(self.id(), WORKOS_DOMAINS, Want::Jar(WORKOS_NAMES))
            .map_err(|e| {
                ProviderError::Auth(format!(
                    "no Factory session cookie found: {e}. {SIGNIN_HINT}"
                ))
            })?;
        let bearer = format!("Bearer {}", workos_bearer(ctx, &workos).await?);
        let first = fetch_with(ctx, API, ("Authorization", bearer.as_str())).await;
        if first.is_ok() {
            return first;
        }
        fetch_with(ctx, APP, ("Authorization", bearer.as_str()))
            .await
            .or(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shapes from Tests/CodexBarTests/FactoryStatusProbeFetchTests.swift.
    const AUTH: &str = r#"{
        "organization": {
            "id": "org_1", "name": "Acme",
            "subscription": {
                "factoryTier": "team",
                "orbSubscription": {"plan": {"name": "Team", "id": "plan_1"}, "status": "active"}
            }
        },
        "userProfile": {"id": "user-1", "email": "user@example.com"}
    }"#;
    const RATE_LIMITS: &str = r#"{
        "usesTokenRateLimitsBilling": true,
        "extraUsageBalanceCents": 2500,
        "overagePreference": null,
        "extraUsageAllowed": false,
        "tokenRateLimitsRolloutEligible": true,
        "limits": {
            "standard": {
                "fiveHour": {"usedPercent": 12, "secondsRemaining": 3600},
                "weekly": {"usedPercent": 34, "secondsRemaining": 7200},
                "monthly": {"usedPercent": 56, "secondsRemaining": 10800}
            }
        }
    }"#;

    fn auth() -> AuthResponse {
        serde_json::from_str(AUTH).unwrap()
    }

    #[test]
    fn maps_the_token_rate_limit_pools() {
        let limits: BillingLimits = serde_json::from_str(RATE_LIMITS).unwrap();
        let snap = rate_limit_snapshot(&auth(), &limits, limits.limits.as_ref().unwrap());

        let primary = snap.primary.unwrap();
        assert_eq!(primary.label, "5h");
        assert_eq!(primary.used_percent, Some(12.0));
        assert_eq!(primary.window_minutes, Some(300));
        // secondsRemaining drives the countdown.
        assert!(primary.resets_at.unwrap() > Utc::now());
        assert_eq!(snap.secondary.unwrap().used_percent, Some(34.0));
        let tertiary = snap.tertiary.unwrap();
        assert_eq!(tertiary.label, "Monthly");
        assert_eq!(tertiary.window_minutes, None);
        assert_eq!(snap.credits, Some(25.0));
        assert_eq!(snap.plan.as_deref(), Some("Factory Team - Team"));
        assert_eq!(snap.account.as_deref(), Some("user@example.com"));
    }

    #[test]
    fn an_expired_rolling_window_reads_as_reset_not_as_stale_usage() {
        let stale: BillingWindow =
            serde_json::from_str(r#"{"usedPercent": 88, "windowEnd": 1000000000}"#).unwrap();
        let now = Utc::now();
        assert_eq!(stale.used_percent(now), 0.0);
        assert_eq!(stale.resets_at(now), None);

        // A future windowEnd keeps its number, in any of the three encodings.
        for encoded in [
            r#"{"usedPercent": 88, "windowEnd": 4102444800}"#,
            r#"{"usedPercent": 88, "windowEnd": 4102444800000}"#,
            r#"{"usedPercent": 88, "windowEnd": "2100-01-01T00:00:00Z"}"#,
        ] {
            let window: BillingWindow = serde_json::from_str(encoded).unwrap();
            assert_eq!(window.used_percent(now), 88.0, "{encoded}");
            assert!(window.resets_at(now).is_some(), "{encoded}");
        }
    }

    #[test]
    fn maps_the_period_token_counters() {
        let usage: UsageResponse = serde_json::from_str(
            r#"{"usage": {"endDate": 2000000000547,
                "standard": {"userTokens": 100, "totalAllowance": 1000},
                "premium": {"userTokens": 30, "totalAllowance": 200}},
                "userId": "user-1"}"#,
        )
        .unwrap();
        let snap = usage_snapshot(&auth(), &usage);
        let primary = snap.primary.unwrap();
        assert_eq!(primary.label, "Standard");
        assert_eq!(primary.used_percent, Some(10.0));
        assert_eq!(primary.window_minutes, None);
        assert_eq!(primary.resets_at.unwrap().timestamp(), 2_000_000_000);
        assert_eq!(snap.secondary.unwrap().used_percent, Some(15.0));
    }

    #[test]
    fn missing_pools_read_as_empty_lanes() {
        let snap = usage_snapshot(&auth(), &UsageResponse::default());
        assert_eq!(snap.primary.unwrap().used_percent, Some(0.0));
        assert_eq!(snap.secondary.unwrap().used_percent, Some(0.0));
    }

    #[test]
    fn the_api_ratio_wins_only_when_it_is_trustworthy() {
        // Plain ratio scale.
        assert_eq!(usage_percent(0, 0, Some(0.42)), 42.0);
        // A zero ratio next to real usage is stale, so the counters take over.
        assert_eq!(usage_percent(500, 1000, Some(0.0)), 50.0);
        // Percent scale is only trusted when the allowance is not.
        assert_eq!(usage_percent(0, 0, Some(42.0)), 42.0);
        assert_eq!(usage_percent(500, 1000, Some(42.0)), 50.0);
        // No allowance and no ratio is zero, never a divide by zero.
        assert_eq!(usage_percent(500, 0, None), 0.0);
        // A sentinel allowance means unlimited: gauge against 100M tokens.
        assert_eq!(usage_percent(50_000_000, i64::MAX, None), 50.0);
        assert_eq!(usage_percent(500_000_000, i64::MAX, None), 100.0);
        // Over quota still clamps.
        assert_eq!(usage_percent(2000, 1000, None), 100.0);
    }

    #[test]
    fn plan_and_account_labels() {
        assert_eq!(plan_label(&auth()).as_deref(), Some("Factory Team - Team"));
        // A plan name that only repeats the brand is dropped.
        let echoing: AuthResponse = serde_json::from_str(
            r#"{"organization":{"subscription":{"factoryTier":"pro",
                "orbSubscription":{"plan":{"name":"Factory Pro"}}}}}"#,
        )
        .unwrap();
        assert_eq!(plan_label(&echoing).as_deref(), Some("Factory Pro"));
        assert_eq!(plan_label(&AuthResponse::default()), None);
        // Organisation name backs a missing email.
        let org_only: AuthResponse =
            serde_json::from_str(r#"{"organization":{"name":"Acme"}}"#).unwrap();
        assert_eq!(account_label(&org_only).as_deref(), Some("Acme"));
    }
}
