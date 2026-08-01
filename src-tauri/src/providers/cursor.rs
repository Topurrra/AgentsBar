//! Cursor usage, authenticated by a browser session cookie.
//!
//! Ported from `Sources/CodexBarCore/Providers/Cursor/CursorStatusProbe.swift`:
//! `/api/usage-summary` for the headline, `/api/auth/me` for identity and the user id,
//! `/api/usage?user=<id>` for the legacy request based plans. Lane labels come from
//! `CursorProviderDescriptor` (Total / Auto / API).
//!
//! Never log the header, only cookie names and counts.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::api_token::parse_rfc3339;
use super::util::{parse_json, percent, web_get};
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow, Want};
use crate::config::Config;

const BASE: &str = "https://cursor.com";

/// Hosts whose cookies may authenticate Cursor web requests (CursorCookieImporter).
const DOMAINS: &[&str] = &["cursor.com", "cursor.sh"];

/// `CursorStatusProbe.sessionCookieNames`, most common first. Any ONE of them marks the
/// jar as a signed in session; the whole jar is then sent, as CodexBar's own
/// `SessionInfo.cookieHeader` does.
const SESSION_NAMES: &[&str] = &[
    "WorkosCursorSessionToken",
    "__Secure-authjs.session-token",
    "authjs.session-token",
    "__Secure-wos-session",
    "wos-session",
    "__Secure-next-auth.session-token",
    "next-auth.session-token",
];

pub struct Cursor;

// ------------------------------------------------------------------ Cursor API models

/// `CursorUsageSummary`. Every field is optional: hobby, pro, team and enterprise
/// accounts each omit a different block.
#[derive(Debug, Default, Deserialize)]
struct UsageSummary {
    #[serde(rename = "billingCycleStart")]
    billing_cycle_start: Option<String>,
    #[serde(rename = "billingCycleEnd")]
    billing_cycle_end: Option<String>,
    #[serde(rename = "membershipType")]
    membership_type: Option<String>,
    #[serde(rename = "individualUsage")]
    individual: Option<IndividualUsage>,
    #[serde(rename = "teamUsage")]
    team: Option<TeamUsage>,
}

#[derive(Debug, Deserialize)]
struct IndividualUsage {
    plan: Option<PlanUsage>,
    #[serde(rename = "onDemand")]
    on_demand: Option<Cents>,
    /// Enterprise / team member personal cap.
    overall: Option<Cents>,
}

#[derive(Debug, Deserialize)]
struct TeamUsage {
    #[serde(rename = "onDemand")]
    on_demand: Option<Cents>,
    /// Shared team pool.
    pooled: Option<Cents>,
}

/// Cursor reports money in cents throughout.
#[derive(Debug, Deserialize)]
struct Cents {
    used: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct PlanUsage {
    used: Option<i64>,
    limit: Option<i64>,
    #[serde(rename = "autoPercentUsed")]
    auto_percent: Option<f64>,
    #[serde(rename = "apiPercentUsed")]
    api_percent: Option<f64>,
    #[serde(rename = "totalPercentUsed")]
    total_percent: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct UserInfo {
    email: Option<String>,
    sub: Option<String>,
}

/// `/api/usage?user=<id>`, only meaningful on the legacy request based plans.
#[derive(Debug, Default, Deserialize)]
struct RequestUsage {
    #[serde(rename = "gpt-4")]
    gpt4: Option<ModelUsage>,
}

#[derive(Debug, Deserialize)]
struct ModelUsage {
    #[serde(rename = "numRequests")]
    num_requests: Option<i64>,
    #[serde(rename = "numRequestsTotal")]
    num_requests_total: Option<i64>,
    /// Non-null is what marks the account as request based rather than token based.
    #[serde(rename = "maxRequestUsage")]
    max_request_usage: Option<i64>,
}

// ------------------------------------------------------------------ mapping

impl Cents {
    fn usd(value: Option<i64>) -> Option<f64> {
        value.map(|v| v as f64 / 100.0)
    }

    fn used_usd(&self) -> f64 {
        Self::usd(self.used).unwrap_or(0.0)
    }

    fn limit_usd(&self) -> Option<f64> {
        Self::usd(self.limit).filter(|l| *l > 0.0)
    }

    fn ratio_percent(&self) -> Option<f64> {
        let limit = self.limit.filter(|l| *l > 0)?;
        Some(percent(self.used.unwrap_or(0) as f64, limit as f64))
    }
}

impl UsageSummary {
    fn plan(&self) -> Option<&PlanUsage> {
        self.individual.as_ref()?.plan.as_ref()
    }

    /// The "Total" headline, in the precedence CursorStatusProbe.parseUsageSummary uses:
    /// reported total, then the average of the two lanes, then either lane, then the plan
    /// ratio, then the personal enterprise cap, then the shared team pool.
    fn total_percent(&self) -> f64 {
        let plan = self.plan();
        let auto = plan.and_then(|p| p.auto_percent);
        let api = plan.and_then(|p| p.api_percent);
        if let Some(total) = plan.and_then(|p| p.total_percent) {
            return total;
        }
        match (auto, api) {
            (Some(a), Some(b)) => return (a + b) / 2.0,
            (None, Some(b)) => return b,
            (Some(a), None) => return a,
            (None, None) => {}
        }
        if let Some(pct) = plan.and_then(|p| {
            Cents {
                used: p.used,
                limit: p.limit,
            }
            .ratio_percent()
        }) {
            return pct;
        }
        if let Some(pct) = self
            .individual
            .as_ref()
            .and_then(|i| i.overall.as_ref())
            .and_then(Cents::ratio_percent)
        {
            return pct;
        }
        self.team
            .as_ref()
            .and_then(|t| t.pooled.as_ref())
            .and_then(Cents::ratio_percent)
            .unwrap_or(0.0)
    }

    /// Personal on-demand budget when there is one, otherwise the shared team budget.
    fn on_demand(&self) -> Option<&Cents> {
        let personal = self.individual.as_ref().and_then(|i| i.on_demand.as_ref());
        if personal.and_then(Cents::limit_usd).is_some() {
            return personal;
        }
        let team = self.team.as_ref().and_then(|t| t.on_demand.as_ref());
        if team.and_then(Cents::limit_usd).is_some() {
            return team;
        }
        personal
    }
}

/// "pro" reads as "Cursor Pro", anything unknown keeps its own capitalisation.
fn membership_label(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut chars = trimmed.chars();
    let head = chars
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    format!("Cursor {head}{}", chars.as_str().to_lowercase())
}

fn window(
    label: &str,
    used_percent: f64,
    resets_at: Option<DateTime<Utc>>,
    minutes: Option<u64>,
) -> UsageWindow {
    UsageWindow::new(label, Some(used_percent), resets_at, minutes)
}

fn to_snapshot(summary: &UsageSummary, user: &UserInfo, requests: &RequestUsage) -> UsageSnapshot {
    let start = summary
        .billing_cycle_start
        .as_deref()
        .and_then(parse_rfc3339);
    let end = summary.billing_cycle_end.as_deref().and_then(parse_rfc3339);
    // The billing cycle is the window for every lane Cursor reports.
    let minutes = match (start, end) {
        (Some(s), Some(e)) => u64::try_from((e - s).num_minutes()).ok().filter(|m| *m > 0),
        _ => None,
    };

    // A legacy request quota replaces the headline and hides the token-based lanes,
    // whose percentages mean nothing next to a request count.
    let legacy = requests.gpt4.as_ref().and_then(|g| {
        let limit = g.max_request_usage.filter(|l| *l > 0)?;
        let used = g.num_requests_total.or(g.num_requests).unwrap_or(0);
        Some(percent(used as f64, limit as f64))
    });

    let mut snapshot = UsageSnapshot::new("cursor");
    snapshot.primary = Some(window(
        "Total",
        legacy.unwrap_or_else(|| summary.total_percent()),
        end,
        minutes,
    ));
    if legacy.is_none() {
        let plan = summary.plan();
        snapshot.secondary = plan
            .and_then(|p| p.auto_percent)
            .map(|p| window("Auto", p, end, minutes));
        snapshot.tertiary = plan
            .and_then(|p| p.api_percent)
            .map(|p| window("API", p, end, minutes));
    }
    // Credits reads as a remaining balance everywhere else in the app, so only a capped
    // on-demand budget produces one. An uncapped budget has no balance to report.
    snapshot.credits = summary
        .on_demand()
        .and_then(|od| od.limit_usd().map(|limit| (limit - od.used_usd()).max(0.0)));
    snapshot.plan = summary
        .membership_type
        .as_deref()
        .map(membership_label)
        .filter(|p| !p.is_empty());
    snapshot.account = user
        .email
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(str::to_string);
    snapshot
}

// ------------------------------------------------------------------ provider

const SIGNIN_HINT: &str =
    "Cursor session expired. Sign in again at cursor.com, or paste a cookie header in Settings";

#[async_trait]
impl Provider for Cursor {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn name(&self) -> &'static str {
        "Cursor"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::Cookie
    }

    fn doc_url(&self) -> &'static str {
        "https://cursor.com/dashboard?tab=usage"
    }

    fn is_configured(&self, config: &Config) -> bool {
        super::has_cookies(config, self.id(), DOMAINS, Want::Jar(SESSION_NAMES))
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        ctx.with_cookies(
            self.id(),
            DOMAINS,
            Want::Jar(SESSION_NAMES),
            SIGNIN_HINT,
            |cookie| async move {
                let headers = [("Accept", "application/json"), ("Cookie", cookie.as_str())];

                // `web_get` maps 401 and 403 to `Auth`, so a logged out browser hands the
                // walk on to the next one here, on the first call.
                let summary: UsageSummary = parse_json(
                    &web_get(
                        &ctx.http,
                        &format!("{BASE}/api/usage-summary"),
                        &headers,
                        SIGNIN_HINT,
                    )
                    .await?,
                )?;

                // Identity is a bonus, and the user id it carries only matters to legacy
                // plans.
                let user: UserInfo = web_get(
                    &ctx.http,
                    &format!("{BASE}/api/auth/me"),
                    &headers,
                    SIGNIN_HINT,
                )
                .await
                .ok()
                .and_then(|text| parse_json(&text).ok())
                .unwrap_or_default();

                let mut requests = RequestUsage::default();
                if let Some(id) = user.sub.as_deref().filter(|s| !s.is_empty()) {
                    // Not every plan has this endpoint, so a failure here is not a fetch
                    // failure.
                    let url = format!("{BASE}/api/usage?user={}", urlencode(id));
                    if let Ok(text) = web_get(&ctx.http, &url, &headers, SIGNIN_HINT).await {
                        requests = parse_json(&text).unwrap_or_default();
                    }
                }

                Ok(to_snapshot(&summary, &user, &requests))
            },
        )
        .await
    }
}

/// Cursor user ids look like `auth0|1234`, and `|` is not legal in a query string.
fn urlencode(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shape from Tests/CodexBarTests/CursorStatusProbeTests.swift.
    const PRO: &str = r#"{
        "billingCycleStart": "2033-01-01T00:00:00.000Z",
        "billingCycleEnd": "2033-02-01T00:00:00.000Z",
        "membershipType": "pro",
        "individualUsage": {
            "plan": {
                "enabled": true, "used": 1500, "limit": 5000, "remaining": 3500,
                "totalPercentUsed": 30.0, "autoPercentUsed": 20.0, "apiPercentUsed": 40.0
            },
            "onDemand": {"enabled": true, "used": 500, "limit": 10000, "remaining": 9500}
        },
        "teamUsage": {
            "onDemand": {"enabled": true, "used": 2000, "limit": 50000, "remaining": 48000}
        }
    }"#;

    fn snapshot_of(summary_json: &str, requests_json: &str) -> UsageSnapshot {
        let summary: UsageSummary = serde_json::from_str(summary_json).unwrap();
        let requests: RequestUsage = serde_json::from_str(requests_json).unwrap();
        let user: UserInfo =
            serde_json::from_str(r#"{"email":"user@example.com","sub":"auth0|12345"}"#).unwrap();
        to_snapshot(&summary, &user, &requests)
    }

    #[test]
    fn maps_a_pro_plan() {
        let snap = snapshot_of(PRO, "{}");
        let primary = snap.primary.unwrap();
        assert_eq!(primary.label, "Total");
        assert_eq!(primary.used_percent, Some(30.0));
        assert_eq!(primary.resets_at.unwrap().timestamp(), 1_990_828_800);
        // A 31 day billing cycle.
        assert_eq!(primary.window_minutes, Some(31 * 24 * 60));
        assert_eq!(snap.secondary.unwrap().label, "Auto");
        assert_eq!(snap.tertiary.unwrap().used_percent, Some(40.0));
        assert_eq!(snap.plan.as_deref(), Some("Cursor Pro"));
        assert_eq!(snap.account.as_deref(), Some("user@example.com"));
        // Personal on-demand: $100.00 cap, $5.00 spent.
        assert_eq!(snap.credits, Some(95.0));
    }

    #[test]
    fn averages_the_two_lanes_when_no_total_is_reported() {
        let snap = snapshot_of(
            r#"{"individualUsage":{"plan":{"autoPercentUsed":20,"apiPercentUsed":40}}}"#,
            "{}",
        );
        assert_eq!(snap.primary.unwrap().used_percent, Some(30.0));
    }

    #[test]
    fn falls_back_through_plan_then_personal_cap_then_pool() {
        // Plan ratio: 4900 of 50000 cents.
        let snap = snapshot_of(
            r#"{"individualUsage":{"plan":{"used":4900,"limit":50000}}}"#,
            "{}",
        );
        assert_eq!(snap.primary.unwrap().used_percent, Some(9.8));

        // Enterprise personal cap when no plan block exists at all.
        let snap = snapshot_of(
            r#"{"individualUsage":{"overall":{"used":7384,"limit":10000}}}"#,
            "{}",
        );
        assert!((snap.primary.unwrap().used_percent.unwrap() - 73.84).abs() < 1e-9);

        // Shared pool as the last resort.
        let snap = snapshot_of(r#"{"teamUsage":{"pooled":{"used":50,"limit":200}}}"#, "{}");
        assert_eq!(snap.primary.unwrap().used_percent, Some(25.0));

        // Nothing at all reads as zero rather than an error.
        let snap = snapshot_of("{}", "{}");
        assert_eq!(snap.primary.unwrap().used_percent, Some(0.0));
        assert_eq!(snap.credits, None);
    }

    #[test]
    fn legacy_request_quota_takes_over_the_headline_and_hides_the_token_lanes() {
        let snap = snapshot_of(
            PRO,
            r#"{"gpt-4":{"numRequests":120,"numRequestsTotal":250,"maxRequestUsage":500}}"#,
        );
        // numRequestsTotal wins over numRequests.
        assert_eq!(snap.primary.unwrap().used_percent, Some(50.0));
        assert!(snap.secondary.is_none());
        assert!(snap.tertiary.is_none());
    }

    #[test]
    fn a_token_based_plan_ignores_the_usage_endpoint() {
        // maxRequestUsage absent means the account is not request based.
        let snap = snapshot_of(PRO, r#"{"gpt-4":{"numRequests":120}}"#);
        assert_eq!(snap.primary.unwrap().used_percent, Some(30.0));
        assert!(snap.secondary.is_some());
    }

    #[test]
    fn percentages_are_clamped_and_team_budget_backs_the_personal_one() {
        let snap = snapshot_of(
            r#"{"individualUsage":{"plan":{"totalPercentUsed":140},"onDemand":{"used":0}},
                "teamUsage":{"onDemand":{"used":2000,"limit":50000}}}"#,
            "{}",
        );
        assert_eq!(snap.primary.unwrap().used_percent, Some(100.0));
        // No personal cap, so the team budget is the one with a balance.
        assert_eq!(snap.credits, Some(480.0));
    }

    #[test]
    fn membership_labels_and_url_encoding() {
        assert_eq!(membership_label("pro"), "Cursor Pro");
        assert_eq!(membership_label("ENTERPRISE"), "Cursor Enterprise");
        assert_eq!(membership_label("  "), "");
        assert_eq!(urlencode("auth0|12345"), "auth0%7C12345");
    }

    /// Live probe for every provider in this batch against this machine's real browsers
    /// and the real APIs. Prints provider names, lane labels, percentages, plan and reset
    /// only. Never a cookie value, never a token, never an account id.
    ///
    /// Run with: cargo test -- --ignored --nocapture live_sessions
    #[tokio::test]
    #[ignore = "hits the real browser profiles and the real provider APIs"]
    async fn live_sessions_on_this_machine() {
        let config = crate::config::Config::load();
        let ctx = FetchContext {
            http: reqwest::Client::new(),
            config,
        };
        for provider in super::super::all_providers()
            .into_iter()
            .filter(|p| ["cursor", "factory", "devin", "t3chat", "opencode"].contains(&p.id()))
        {
            if !provider.is_configured(&ctx.config) {
                println!("{}: no session on this machine, skipped", provider.name());
                continue;
            }
            match provider.fetch(&ctx).await {
                Ok(snap) => {
                    for lane in [&snap.primary, &snap.secondary, &snap.tertiary]
                        .into_iter()
                        .flatten()
                    {
                        println!(
                            "{}: {} {:?}% used, resets {:?}, window {:?}",
                            provider.name(),
                            lane.label,
                            lane.used_percent,
                            lane.resets_at,
                            lane.window_minutes
                        );
                        assert!(lane.used_percent.is_none_or(|p| (0.0..=100.0).contains(&p)));
                    }
                    println!(
                        "{}: plan {:?}, credits {:?}",
                        provider.name(),
                        snap.plan,
                        snap.credits
                    );
                    assert!(
                        snap.primary.is_some(),
                        "{}: no primary lane",
                        provider.name()
                    );
                }
                Err(e) => println!("{}: FAILED: {e}", provider.name()),
            }
        }
    }
}
