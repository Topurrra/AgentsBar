//! Manus credits, ported from CodexBar `Providers/Manus/`:
//! `ManusCookieHeader.swift`, `ManusCookieImporter.swift` and `ManusUsageFetcher.swift`.
//!
//! The browser session cookie is not sent as a `Cookie` header at all. Manus authenticates
//! its Connect RPC with the VALUE of the `session_id` cookie as a bearer token, exactly as
//! `ManusUsageFetcher.fetchCredits(sessionToken:)` does.
//!
//! Cookie domain: manus.im (covers www.manus.im and api.manus.im by suffix)
//! Cookie name: session_id
//!
//! Never log the header or the token, only cookie names and counts.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::api_token::{epoch_to_utc, loose_f64, parse_rfc3339, post_json, Auth};
use super::util::percent;
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow, Want};
use crate::config::Config;

const CREDITS_URL: &str = "https://api.manus.im/user.v1.UserService/GetAvailableCredits";
const DOMAINS: [&str; 1] = ["manus.im"];
const NAMES: [&str; 1] = ["session_id"];
const SIGNIN_HINT: &str = "Sign in at manus.im, or paste a cookie header in Settings";

/// Chrome's UA. Manus serves this RPC to its own web app, and a default reqwest UA is
/// the kind of thing an edge limiter drops.
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36";

/// At least one of these has to be in the payload, otherwise an error envelope would
/// decode into a snapshot claiming zero credits.
const CREDIT_KEYS: [&str; 8] = [
    "totalCredits",
    "freeCredits",
    "periodicCredits",
    "addonCredits",
    "refreshCredits",
    "maxRefreshCredits",
    "proMonthlyCredits",
    "eventCredits",
];

pub struct Manus;

#[async_trait]
impl Provider for Manus {
    fn id(&self) -> &'static str {
        "manus"
    }

    fn name(&self) -> &'static str {
        "Manus"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::Cookie
    }

    fn doc_url(&self) -> &'static str {
        "https://manus.im"
    }

    fn is_configured(&self, config: &Config) -> bool {
        super::has_cookies(config, self.id(), &DOMAINS, Want::All(&NAMES))
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        ctx.with_cookies(
            self.id(),
            &DOMAINS,
            Want::All(&NAMES),
            SIGNIN_HINT,
            |header| async move {
                let token = session_token(&header).ok_or_else(|| {
                    ProviderError::Auth(format!("no Manus session_id cookie. {SIGNIN_HINT}"))
                })?;

                // `post_json` maps 401 and 403 to `Auth`, so a stale session_id from one
                // browser moves the walk on to the next.
                let body: Value = post_json(
                    &ctx.http,
                    CREDITS_URL,
                    &Auth::Bearer(&token),
                    &[
                        ("Origin", "https://manus.im"),
                        ("Referer", "https://manus.im/"),
                        ("Connect-Protocol-Version", "1"),
                        ("User-Agent", USER_AGENT),
                    ],
                    &json!({}),
                )
                .await?;

                snapshot(&body, Utc::now())
            },
        )
        .await
    }
}

/// The `session_id` value out of a `Cookie` header. A user who pasted the bare token
/// (no `=`, no `;`) gets it back as is, matching `ManusCookieHeader.token(from:)`.
fn session_token(header: &str) -> Option<String> {
    let header = header.trim();
    if header.is_empty() {
        return None;
    }
    if !header.contains('=') && !header.contains(';') {
        return Some(header.to_string());
    }
    header
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case(NAMES[0]))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Manus has shipped the credits both bare and wrapped, so unwrap a known envelope key
/// when it is the one holding the credit fields.
fn credits_object(root: &Value) -> Option<&Value> {
    for key in ["data", "result", "response", "availableCredits"] {
        match root.get(key) {
            Some(inner) if has_credit_key(inner) => return Some(inner),
            _ => {}
        }
    }
    has_credit_key(root).then_some(root)
}

fn has_credit_key(value: &Value) -> bool {
    CREDIT_KEYS.iter().any(|key| value.get(key).is_some())
}

fn number(object: &Value, key: &str) -> f64 {
    loose_f64(object.get(key)).unwrap_or(0.0)
}

/// `nextRefreshTime` is an RFC 3339 string in every payload seen so far. Epoch numbers
/// are accepted too rather than silently dropping the reset time.
fn refresh_time(object: &Value) -> Option<DateTime<Utc>> {
    match object.get("nextRefreshTime")? {
        Value::String(s) => parse_rfc3339(s),
        Value::Number(n) => epoch_to_utc(n.as_f64()? as i64),
        _ => None,
    }
}

fn snapshot(root: &Value, now: DateTime<Utc>) -> Result<UsageSnapshot, ProviderError> {
    let object = credits_object(root)
        .ok_or_else(|| ProviderError::Parse("Manus response has no credits fields".into()))?;

    let pro_monthly = number(object, "proMonthlyCredits");
    let periodic = number(object, "periodicCredits");
    let refresh = number(object, "refreshCredits");
    let max_refresh = number(object, "maxRefreshCredits");

    let mut snap = UsageSnapshot::new("manus");
    snap.fetched_at = now;
    snap.credits = Some(number(object, "totalCredits"));
    if pro_monthly > 0.0 {
        snap.primary = Some(UsageWindow::at(
            "Monthly credits",
            Some(percent(pro_monthly - periodic, pro_monthly)),
            None,
            None,
            now,
        ));
    }
    if max_refresh > 0.0 {
        snap.secondary = Some(UsageWindow::at(
            "Daily refresh",
            Some(percent(max_refresh - refresh, max_refresh)),
            refresh_time(object),
            None,
            now,
        ));
    }
    snap.plan = object
        .get("refreshInterval")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(snap)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic payloads only. Never a real cookie or token.
    fn parse(json: &str) -> Result<UsageSnapshot, ProviderError> {
        snapshot(&serde_json::from_str(json).unwrap(), Utc::now())
    }

    #[test]
    fn session_token_reads_the_named_cookie_or_a_bare_token() {
        assert_eq!(session_token("session_id=abc").as_deref(), Some("abc"));
        assert_eq!(
            session_token("other=1; session_id=abc; more=2").as_deref(),
            Some("abc")
        );
        assert_eq!(session_token("SESSION_ID=abc").as_deref(), Some("abc"));
        // A pasted bare token has no cookie syntax at all.
        assert_eq!(session_token("  abc  ").as_deref(), Some("abc"));
        assert_eq!(session_token("other=1; more=2"), None);
        assert_eq!(session_token("session_id=  "), None);
        assert_eq!(session_token(""), None);
    }

    #[test]
    fn both_lanes_map_from_a_full_payload() {
        let snap = parse(
            r#"{"totalCredits":9300,"freeCredits":300,"periodicCredits":1500,
                "addonCredits":0,"refreshCredits":100,"maxRefreshCredits":300,
                "proMonthlyCredits":6000,"eventCredits":0,
                "nextRefreshTime":"2026-08-01T12:00:00Z","refreshInterval":"daily"}"#,
        )
        .unwrap();
        let primary = snap.primary.unwrap();
        assert_eq!(primary.label, "Monthly credits");
        // 6000 granted, 1500 left: 75% used.
        assert_eq!(primary.used_percent, Some(75.0));
        let secondary = snap.secondary.unwrap();
        // 300 max refresh, 100 left.
        assert!((secondary.used_percent.unwrap() - 200.0 / 3.0).abs() < 1e-9);
        assert_eq!(
            secondary.resets_at.map(|d| d.timestamp()),
            Some(1_785_585_600)
        );
        assert_eq!(snap.credits, Some(9300.0));
        assert_eq!(snap.plan.as_deref(), Some("daily"));
    }

    #[test]
    fn envelopes_and_quoted_numbers_are_unwrapped() {
        for key in ["data", "result", "response", "availableCredits"] {
            let snap = parse(&format!(
                r#"{{"{key}":{{"totalCredits":"250","proMonthlyCredits":"1000",
                    "periodicCredits":"250"}}}}"#
            ))
            .unwrap();
            assert_eq!(snap.credits, Some(250.0));
            assert_eq!(snap.primary.unwrap().used_percent, Some(75.0));
        }
    }

    #[test]
    fn a_payload_without_credit_fields_is_an_error_not_zero_credits() {
        assert!(parse(r#"{"code":16,"message":"unauthenticated"}"#).is_err());
        assert!(parse(r#"{"data":{"code":16}}"#).is_err());
    }

    #[test]
    fn free_accounts_have_no_monthly_lane() {
        let snap = parse(
            r#"{"totalCredits":300,"freeCredits":300,"proMonthlyCredits":0,
                "refreshCredits":300,"maxRefreshCredits":300}"#,
        )
        .unwrap();
        assert!(snap.primary.is_none());
        assert_eq!(snap.secondary.unwrap().used_percent, Some(0.0));
        assert_eq!(snap.credits, Some(300.0));
    }

    /// Live check against this machine's real Manus session, if one exists.
    /// Prints percentages and counts only, never a cookie value or a token.
    /// cargo test -p agentbar manus_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs a real manus.im browser session"]
    async fn manus_live() {
        let ctx = FetchContext {
            http: reqwest::Client::new(),
            config: Config::default(),
        };
        match Manus.fetch(&ctx).await {
            Ok(snap) => println!(
                "manus: credits={:?} primary={:?} secondary={:?}",
                snap.credits, snap.primary, snap.secondary
            ),
            Err(e) => println!("manus: {e}"),
        }
    }
}
