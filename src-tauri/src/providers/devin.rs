//! Devin usage, authenticated by a pasted session token.
//!
//! Ported from `Sources/CodexBarCore/Providers/Devin/`: `app.devin.ai/api/<org>/billing/
//! quota/usage` with a Bearer token, and `DevinUsageParser` for the reply. Lane labels
//! come from `DevinProviderDescriptor` (Daily / Weekly).
//!
//! LIMITATION, deliberate: Devin has no session COOKIE. `DevinSessionImporter` reads the
//! `auth1_` token out of a Chromium **Local Storage LevelDB** file, a binary format the
//! wave 2 cookie layer does not touch and which is well outside its scope. So the
//! automatic browser path is not implemented; this provider is configured by pasting the
//! token into Settings (the same `cookie_header` field, which is already treated as a
//! secret). `is_configured` is false until that is done, and the error says so.
//!
//! Never log the token, only what was found and what is missing.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::api_token::{epoch_to_utc, parse_rfc3339};
use super::cursor::web_get;
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow};
use crate::config::Config;

const BASE: &str = "https://app.devin.ai";

const PASTE_HINT: &str = "Devin needs a session token. Open app.devin.ai, copy the Bearer token \
                          from a request's Authorization header together with your org URL, and \
                          paste both into the Devin cookie header field in Settings, for example \
                          \"Bearer auth1_xxx; https://app.devin.ai/org/acme\"";

pub struct Devin;

// ------------------------------------------------------------------ credentials

/// What the user pasted, split into the two things a request needs.
#[derive(Debug, PartialEq)]
struct Credentials {
    token: String,
    /// Already normalised to `org/<slug>` or `organizations/<internal id>`.
    organization: String,
}

/// Accepts what a browser's network tab yields: a bare token, an `Authorization:` header
/// line, and an organization given either as a slug or as an app.devin.ai URL. The parts
/// may be separated by `;`, a newline or whitespace.
fn parse_credentials(raw: &str) -> Option<Credentials> {
    let mut token = None;
    let mut organization = None;
    for part in raw
        .split(['\n', ';'])
        .flat_map(|line| line.split_whitespace())
    {
        let part = part.trim().trim_matches(|c| c == '"' || c == ',');
        if part.is_empty() {
            continue;
        }
        let lower = part.to_ascii_lowercase();
        if lower == "authorization:" || lower == "bearer" || lower == "authorization" {
            continue;
        }
        if let Some(rest) = lower.strip_prefix("authorization:") {
            // "Authorization:Bearer auth1_x" with no space after the colon, and
            // "Authorization:Bearer" with the token in the next part: no trailing space in
            // the pattern, so a bare "Bearer" remainder trims away to nothing rather than
            // being taken for the token.
            let value = &part[part.len() - rest.len()..];
            let value = value.strip_prefix("Bearer").unwrap_or(value).trim();
            if !value.is_empty() {
                token.get_or_insert_with(|| value.to_string());
            }
            continue;
        }
        if let Some(org) = organization_from(part) {
            organization.get_or_insert(org);
            continue;
        }
        if token.is_none() && part.len() > 8 {
            token = Some(part.to_string());
        }
    }
    Some(Credentials {
        token: token?,
        organization: organization?,
    })
}

/// `DevinUsageFetcher.normalizedOrganization`: a devin.ai URL, a slug, or an internal
/// `org-`/`org_` id all end up as one of the two API path prefixes.
fn organization_from(raw: &str) -> Option<String> {
    let mut value = raw.trim().to_string();
    if let Some(rest) = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    {
        let (host, path) = rest.split_once('/')?;
        if host != "devin.ai" && !host.ends_with(".devin.ai") {
            return None;
        }
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        match parts.as_slice() {
            ["org", slug, ..] => value = format!("org/{slug}"),
            ["organizations", id, ..] => value = format!("organizations/{id}"),
            _ => return None,
        }
    }
    let value = value.trim_matches('/');
    if value.is_empty() {
        return None;
    }
    if value.starts_with("org/") || value.starts_with("organizations/") {
        return Some(value.to_string());
    }
    if value.starts_with("org-") || value.starts_with("org_") {
        return Some(format!("organizations/{value}"));
    }
    None
}

/// `DevinUsageFetcher.candidatePaths`: Devin moved this route more than once, so every
/// known spelling is tried until one answers.
fn candidate_paths(organization: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut push = |path: String| {
        if !paths.contains(&path) {
            paths.push(path);
        }
    };
    let internal = organization.strip_prefix("organizations/");
    if let Some(id) = internal {
        push(format!("{id}/billing/quota/usage"));
    }
    push(format!("{organization}/billing/quota/usage"));
    if let Some(slug) = organization.strip_prefix("org/") {
        push(format!("{slug}/billing/quota/usage"));
    }
    if let Some(id) = internal {
        push(format!("organizations/{id}/billing/quota/usage"));
    }
    paths
}

// ------------------------------------------------------------------ parsing

fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Devin mixes fractions and percentages in the same payload, so anything at or below 1
/// is read as a fraction.
fn as_percent(raw: f64) -> f64 {
    let scaled = if raw <= 1.0 { raw * 100.0 } else { raw };
    scaled.clamp(0.0, 100.0)
}

fn timestamp(value: Option<&Value>) -> Option<DateTime<Utc>> {
    match value? {
        Value::String(s) => parse_rfc3339(s).or_else(|| {
            s.trim()
                .parse::<f64>()
                .ok()
                .and_then(|n| epoch_to_utc(n as i64))
        }),
        other => epoch_to_utc(number(Some(other))? as i64),
    }
}

const DIRECT_PERCENT_KEYS: &[&str] = &[
    "used_percent",
    "usedPercent",
    "usage_percent",
    "usagePercent",
    "percent_used",
    "percentUsed",
    "percent",
];
const REMAINING_PERCENT_KEYS: &[&str] = &[
    "remaining_percent",
    "remainingPercent",
    "percent_remaining",
    "percentRemaining",
];

/// `DevinUsageParser.percent(from:)`.
fn percent_of(value: &Value) -> Option<f64> {
    if let Some(direct) = number(Some(value)) {
        return Some(as_percent(direct));
    }
    let object = value.as_object()?;
    let first = |keys: &[&str]| keys.iter().find_map(|k| number(object.get(*k)));
    if let Some(used) = first(DIRECT_PERCENT_KEYS) {
        return Some(as_percent(used));
    }
    if let Some(remaining) = first(REMAINING_PERCENT_KEYS) {
        return Some(100.0 - as_percent(remaining));
    }
    let limit = first(&["limit", "quota", "total", "max", "available"]);
    if let (Some(used), Some(limit)) = (
        first(&["used", "usage", "used_count", "usedCount", "consumed"]),
        limit.filter(|l| *l > 0.0),
    ) {
        return Some(super::util::percent(used, limit));
    }
    if let (Some(remaining), Some(limit)) = (
        first(&["remaining", "left", "available"]),
        limit.filter(|l| *l > 0.0),
    ) {
        return Some(super::util::percent(limit - remaining, limit));
    }
    None
}

/// A `{ percent, resets_at }` pair pulled from one JSON node.
fn window_of(value: &Value) -> Option<(f64, Option<DateTime<Utc>>)> {
    let Some(object) = value.as_object() else {
        return percent_of(value).map(|p| (p, None));
    };
    if let Some(percent) = percent_of(value) {
        let reset = object
            .iter()
            .find(|(key, _)| key.to_lowercase().contains("reset"))
            .and_then(|(_, v)| timestamp(Some(v)));
        return Some((percent, reset));
    }
    object.values().find_map(window_of)
}

/// Depth first search for a window under a key the predicate accepts.
fn find_window(value: &Value, matches: fn(&str) -> bool) -> Option<(f64, Option<DateTime<Utc>>)> {
    if let Some(object) = value.as_object() {
        for (key, child) in object {
            if matches(key) {
                if let Some(window) = window_of(child) {
                    return Some(window);
                }
            }
        }
        return object.values().find_map(|v| find_window(v, matches));
    }
    value
        .as_array()?
        .iter()
        .find_map(|v| find_window(v, matches))
}

fn is_daily(key: &str) -> bool {
    let key = key.to_lowercase();
    !key.contains("hide") && (key.contains("daily") || key.contains("day"))
}

fn is_weekly(key: &str) -> bool {
    let key = key.to_lowercase();
    !key.contains("hide") && (key.contains("weekly") || key.contains("week"))
}

fn find_plan(value: &Value) -> Option<String> {
    if let Some(object) = value.as_object() {
        for key in [
            "plan_name",
            "planName",
            "plan",
            "tier",
            "subscription_tier",
            "subscriptionTier",
        ] {
            if let Some(name) = object.get(key).and_then(Value::as_str).map(str::trim) {
                if !name.is_empty() {
                    // `team_pro` reads as `Team Pro`.
                    return Some(
                        name.split(['_', '-'])
                            .map(|part| {
                                let mut chars = part.chars();
                                match chars.next() {
                                    Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
                                    None => String::new(),
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" "),
                    );
                }
            }
        }
        return object.values().find_map(find_plan);
    }
    value.as_array()?.iter().find_map(find_plan)
}

fn to_snapshot(body: &Value, organization: &str) -> Result<UsageSnapshot, ProviderError> {
    // The current API reports flat percentage fields; older shapes nest them, so the
    // search by key name is the fallback.
    let object = body.as_object();
    let current = |percent_key: &str, reset_key: &str| {
        let object = object?;
        let percent = as_percent(number(object.get(percent_key))?);
        Some((percent, timestamp(object.get(reset_key))))
    };
    let daily =
        current("daily_percentage", "daily_reset_at").or_else(|| find_window(body, is_daily));
    let weekly =
        current("weekly_percentage", "weekly_reset_at").or_else(|| find_window(body, is_weekly));
    if daily.is_none() && weekly.is_none() {
        return Err(ProviderError::Parse(
            "Devin reply had no daily or weekly quota window".into(),
        ));
    }

    let mut snapshot = UsageSnapshot::new("devin");
    snapshot.primary = daily.map(|(used, resets_at)| UsageWindow {
        label: "Daily".to_string(),
        used_percent: used,
        resets_at,
        window_minutes: Some(24 * 60),
    });
    snapshot.secondary = weekly.map(|(used, resets_at)| UsageWindow {
        label: "Weekly".to_string(),
        used_percent: used,
        resets_at,
        window_minutes: Some(7 * 24 * 60),
    });
    snapshot.credits = object.and_then(|o| {
        number(o.get("overage_balance"))
            .or_else(|| number(o.get("overage_balance_cents")).map(|c| c / 100.0))
            .filter(|v| v.is_finite() && *v >= 0.0)
    });
    snapshot.plan = find_plan(body);
    snapshot.account = Some(
        organization
            .trim_start_matches("organizations/")
            .trim_start_matches("org/")
            .to_string(),
    );
    Ok(snapshot)
}

// ------------------------------------------------------------------ provider

#[async_trait]
impl Provider for Devin {
    fn id(&self) -> &'static str {
        "devin"
    }

    fn name(&self) -> &'static str {
        "Devin"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::Cookie
    }

    fn doc_url(&self) -> &'static str {
        "https://app.devin.ai"
    }

    fn is_configured(&self, config: &Config) -> bool {
        config.cookie_source(self.id()) != "off"
            && config
                .cookie_header(self.id())
                .and_then(parse_credentials)
                .is_some()
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        if ctx.config.cookie_source(self.id()) == "off" {
            return Err(ProviderError::NotConfigured);
        }
        let credentials = ctx
            .config
            .cookie_header(self.id())
            .and_then(parse_credentials)
            .ok_or_else(|| ProviderError::Auth(PASTE_HINT.to_string()))?;

        let bearer = format!("Bearer {}", credentials.token);
        let internal = credentials
            .organization
            .strip_prefix("organizations/")
            .unwrap_or_default();
        let mut headers = vec![
            ("Accept", "application/json"),
            ("Authorization", bearer.as_str()),
        ];
        if !internal.is_empty() {
            headers.push(("x-cog-org-id", internal));
        }

        let mut last = ProviderError::Http("no Devin quota endpoint answered".into());
        for path in candidate_paths(&credentials.organization) {
            match web_get(
                &ctx.http,
                &format!("{BASE}/api/{path}"),
                &headers,
                "Devin session token is invalid or expired. Paste a fresh one in Settings",
            )
            .await
            {
                Ok(body) => {
                    let value: Value = serde_json::from_str(&body)
                        .map_err(|e| ProviderError::Parse(e.to_string()))?;
                    return to_snapshot(&value, &credentials.organization);
                }
                // An expired token fails the same way on every path, so stop early.
                Err(e @ ProviderError::Auth(_)) => return Err(e),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(raw: &str) -> Option<Credentials> {
        parse_credentials(raw)
    }

    #[test]
    fn accepts_the_shapes_a_browser_network_tab_yields() {
        let expected = Credentials {
            token: "auth1_abcdefghijklmnop".to_string(),
            organization: "org/acme".to_string(),
        };
        assert_eq!(
            creds("auth1_abcdefghijklmnop; https://app.devin.ai/org/acme"),
            Some(expected)
        );
        assert_eq!(
            creds("Authorization: Bearer auth1_abcdefghijklmnop\norg/acme")
                .unwrap()
                .token,
            "auth1_abcdefghijklmnop"
        );
        // No space after the colon, with and without one before the token.
        assert_eq!(
            creds("Authorization:Bearer auth1_abcdefghijklmnop; org/acme")
                .unwrap()
                .token,
            "auth1_abcdefghijklmnop"
        );
        assert_eq!(
            creds("Authorization:Bearerauth1_abcdefghijklmnop org/acme")
                .unwrap()
                .token,
            "auth1_abcdefghijklmnop"
        );
        // An internal org id routes to the other path prefix.
        assert_eq!(
            creds("auth1_abcdefghijklmnop org_12345")
                .unwrap()
                .organization,
            "organizations/org_12345"
        );
        // Either half alone is not enough to make a request.
        assert_eq!(creds("auth1_abcdefghijklmnop"), None);
        assert_eq!(creds("org/acme"), None);
        assert_eq!(creds(""), None);
    }

    #[test]
    fn organization_normalisation_rejects_foreign_urls() {
        assert_eq!(
            organization_from("https://app.devin.ai/org/acme/settings").as_deref(),
            Some("org/acme")
        );
        assert_eq!(
            organization_from("https://app.devin.ai/organizations/org-9").as_deref(),
            Some("organizations/org-9")
        );
        assert_eq!(organization_from("https://evil.example.com/org/acme"), None);
        assert_eq!(organization_from("just-a-slug"), None);
    }

    #[test]
    fn every_known_spelling_of_the_quota_route_is_tried_once() {
        assert_eq!(
            candidate_paths("org/acme"),
            ["org/acme/billing/quota/usage", "acme/billing/quota/usage"]
        );
        assert_eq!(
            candidate_paths("organizations/org_1"),
            [
                "org_1/billing/quota/usage",
                "organizations/org_1/billing/quota/usage"
            ]
        );
    }

    #[test]
    fn maps_the_flat_percentage_payload() {
        let body: Value = serde_json::from_str(
            r#"{"daily_percentage": 0.42, "daily_reset_at": "2026-02-01T00:00:00Z",
                "weekly_percentage": 61, "weekly_reset_at": 1768507567547,
                "overage_balance_cents": 2500, "plan_name": "team_pro"}"#,
        )
        .unwrap();
        let snap = to_snapshot(&body, "org/acme").unwrap();

        let primary = snap.primary.unwrap();
        assert_eq!(primary.label, "Daily");
        // A fraction below 1 is a fraction, not 0.42%.
        assert_eq!(primary.used_percent, 42.0);
        assert_eq!(primary.window_minutes, Some(24 * 60));
        assert_eq!(primary.resets_at.unwrap().timestamp(), 1_769_904_000);

        let secondary = snap.secondary.unwrap();
        assert_eq!(secondary.used_percent, 61.0);
        assert_eq!(secondary.resets_at.unwrap().timestamp(), 1_768_507_567);

        assert_eq!(snap.credits, Some(25.0));
        assert_eq!(snap.plan.as_deref(), Some("Team Pro"));
        assert_eq!(snap.account.as_deref(), Some("acme"));
    }

    #[test]
    fn falls_back_to_nested_windows_and_computed_ratios() {
        let body: Value = serde_json::from_str(
            r#"{"quota": {"daily": {"used": 25, "limit": 100, "reset_at": 1768507567},
                          "weekly": {"remaining_percent": 30}}}"#,
        )
        .unwrap();
        let snap = to_snapshot(&body, "organizations/org_1").unwrap();
        assert_eq!(snap.primary.unwrap().used_percent, 25.0);
        assert_eq!(snap.secondary.unwrap().used_percent, 70.0);
        assert_eq!(snap.account.as_deref(), Some("org_1"));
    }

    #[test]
    fn a_payload_with_no_window_at_all_is_a_parse_error() {
        let body: Value = serde_json::from_str(r#"{"something": "else"}"#).unwrap();
        assert!(to_snapshot(&body, "org/acme").is_err());
    }

    #[test]
    fn percentages_clamp_and_survive_string_numbers() {
        let body: Value =
            serde_json::from_str(r#"{"daily_percentage": "140", "weekly_percentage": -5}"#)
                .unwrap();
        let snap = to_snapshot(&body, "org/acme").unwrap();
        assert_eq!(snap.primary.unwrap().used_percent, 100.0);
        assert_eq!(snap.secondary.unwrap().used_percent, 0.0);
    }
}
