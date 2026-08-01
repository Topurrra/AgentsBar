//! OpenCode usage, authenticated by a browser session cookie.
//!
//! Ported from `Sources/CodexBarCore/Providers/OpenCode/OpenCodeUsageFetcher.swift`.
//! opencode.ai is a TanStack Start app, so usage comes from two server function calls on
//! `/_server` identified by a build stable function id: first the workspace list, then
//! that workspace's subscription. The reply is serialized JavaScript rather than JSON on
//! the live site, and plain JSON in some deployments, so both shapes are parsed.
//! Lane labels come from `OpenCodeProviderDescriptor` (5-hour / Weekly).
//!
//! Never log the header, only cookie names and counts.

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::Value;

use super::cursor::web_get;
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow, Want};
use crate::config::Config;

const DOMAINS: &[&str] = &["opencode.ai"];
/// `OpenCodeWebCookieSupport.requestCookieNames`, which CodexBar uses to FILTER the jar
/// (`CookieHeaderNormalizer.filteredHeader(from:allowedNames:)`). Only these are sent,
/// never the rest of the site's cookies, and both travel when both exist.
const SESSION_NAMES: &[&str] = &["__Host-auth", "auth"];

const BASE: &str = "https://opencode.ai";
const SERVER: &str = "https://opencode.ai/_server";
/// Server function ids, from OpenCodeUsageFetcher. They change when OpenCode redeploys
/// its server functions, and are the first thing to re-capture if this provider breaks.
const WORKSPACES_FN: &str = "def39973159c7f0483d8793a822b8dbb10d067e12c65455fcb4608459ba0234f";
const SUBSCRIPTION_FN: &str = "7abeebee372f304e050aaaf92be863f4a86490e382f8c79db68fd94040d691b4";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
const SIGNED_OUT: &str =
    "OpenCode session cookie is invalid or expired. Sign in again at opencode.ai";

pub struct OpenCode;

/// One rolling window as OpenCode reports it.
#[derive(Debug, PartialEq)]
struct Window {
    used_percent: f64,
    reset_in_sec: i64,
}

// ------------------------------------------------------------------ parsing

/// Every `wrk_...` id in the body, in order. Covers both the serialized JavaScript form
/// (`id:"wrk_01K6..."`) and a plain JSON workspace list.
fn workspace_ids(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(hit) = text[from..].find("wrk_") {
        let start = from + hit;
        let mut end = start + 4;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        let id = &text[start..end];
        if id.len() > 4 && !out.iter().any(|seen| seen == id) {
            out.push(id.to_string());
        }
        from = end.max(start + 4);
    }
    out
}

/// A signed out reply is a 200 with a login page or a public actor in it.
fn looks_signed_out(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "login",
        "sign in",
        "auth/authorize",
        "not associated with an account",
        "actor of type \"public\"",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn first_number(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| number(object.get(*k)))
}

const PERCENT_KEYS: &[&str] = &[
    "usagePercent",
    "usedPercent",
    "percentUsed",
    "percent",
    "usage_percent",
    "used_percent",
    "utilization",
    "utilizationPercent",
    "utilization_percent",
    "usage",
];
const RESET_IN_KEYS: &[&str] = &[
    "resetInSec",
    "resetInSeconds",
    "resetSeconds",
    "reset_sec",
    "reset_in_sec",
    "resetsInSec",
    "resetsInSeconds",
    "resetIn",
    "resetSec",
];
const RESET_AT_KEYS: &[&str] = &[
    "resetAt",
    "resetsAt",
    "reset_at",
    "resets_at",
    "nextReset",
    "next_reset",
    "renewAt",
    "renew_at",
];

fn parse_window(value: &Value) -> Option<Window> {
    let object = value.as_object()?;
    // A direct percent field may be a fraction or already a percentage; a computed
    // used/limit ratio is always a percentage and must not be scaled again.
    let direct = first_number(object, PERCENT_KEYS);
    let mut percent = match direct {
        Some(p) if (0.0..=1.0).contains(&p) => p * 100.0,
        Some(p) => p,
        None => {
            let used = first_number(
                object,
                &["used", "usage", "consumed", "count", "usedTokens"],
            )?;
            let limit = first_number(
                object,
                &["limit", "total", "quota", "max", "cap", "tokenLimit"],
            )
            .filter(|l| *l > 0.0)?;
            used / limit * 100.0
        }
    };
    percent = percent.clamp(0.0, 100.0);

    let reset_in_sec = first_number(object, RESET_IN_KEYS)
        .or_else(|| {
            let at = RESET_AT_KEYS.iter().find_map(|k| object.get(*k))?;
            let seconds = seconds_until(at)?;
            Some(seconds as f64)
        })
        .unwrap_or(0.0);
    Some(Window {
        used_percent: percent,
        reset_in_sec: reset_in_sec.max(0.0) as i64,
    })
}

/// Seconds from now to an ISO 8601 or epoch `resetAt`. Values outside a sane range read
/// as absent rather than as an absurd countdown.
fn seconds_until(value: &Value) -> Option<i64> {
    let at = match value {
        Value::String(s) => {
            super::api_token::parse_rfc3339(s).or_else(|| number(Some(value)).and_then(epoch))?
        }
        _ => number(Some(value)).and_then(epoch)?,
    };
    let delta = (at - Utc::now()).num_seconds();
    (delta > 0).then_some(delta)
}

fn epoch(raw: f64) -> Option<chrono::DateTime<Utc>> {
    if !raw.is_finite() || raw <= 0.0 || raw > 1e15 {
        return None;
    }
    super::api_token::epoch_to_utc(raw as i64)
}

/// The rolling and weekly windows out of a JSON payload, at the top level or one `usage`
/// wrapper down.
fn parse_json_windows(value: &Value) -> Option<(Window, Window)> {
    let object = value.as_object()?;
    if let Some(nested) = object.get("usage").and_then(parse_json_windows) {
        return Some(nested);
    }
    let pick = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| object.get(*k))
            .and_then(parse_window)
    };
    let rolling = pick(&["rollingUsage", "rolling", "rolling_usage", "rollingWindow"])?;
    let weekly = pick(&["weeklyUsage", "weekly", "weekly_usage", "weeklyWindow"])?;
    Some((rolling, weekly))
}

/// `rollingUsage:$R[42]={status:"ok",resetInSec:5944,usagePercent:17}` in the serialized
/// JavaScript reply: find the window's key, then the first `field:number` after it.
fn scan_field(text: &str, window_key: &str, field: &str) -> Option<f64> {
    let start = text.find(window_key)? + window_key.len();
    let rest = &text[start..];
    let at = rest.find(field)? + field.len();
    let tail = rest[at..].trim_start();
    let tail = tail.strip_prefix(':')?.trim_start();
    let digits: String = tail
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.parse().ok()
}

fn parse_subscription(body: &str) -> Result<(Window, Window), ProviderError> {
    if let Some(windows) = serde_json::from_str::<Value>(body)
        .ok()
        .as_ref()
        .and_then(parse_json_windows)
    {
        return Ok(windows);
    }
    let window = |key: &str| {
        Some(Window {
            used_percent: scan_field(body, key, "usagePercent")?,
            reset_in_sec: scan_field(body, key, "resetInSec")? as i64,
        })
    };
    match (window("rollingUsage"), window("weeklyUsage")) {
        (Some(rolling), Some(weekly)) => Ok((rolling, weekly)),
        _ => Err(ProviderError::Parse(
            "OpenCode subscription reply had no usage windows".into(),
        )),
    }
}

fn to_snapshot(rolling: &Window, weekly: &Window) -> UsageSnapshot {
    let now = Utc::now();
    let mut snapshot = UsageSnapshot::new("opencode");
    snapshot.primary = Some(UsageWindow::at(
        "5-hour",
        Some(rolling.used_percent),
        Some(now + Duration::seconds(rolling.reset_in_sec)),
        Some(5 * 60),
        now,
    ));
    snapshot.secondary = Some(UsageWindow::at(
        "Weekly",
        Some(weekly.used_percent),
        Some(now + Duration::seconds(weekly.reset_in_sec)),
        Some(7 * 24 * 60),
        now,
    ));
    snapshot
}

// ------------------------------------------------------------------ provider

fn server_url(function_id: &str, args: Option<&str>) -> String {
    match args {
        Some(args) => format!("{SERVER}?id={function_id}&args={}", urlencode(args)),
        None => format!("{SERVER}?id={function_id}"),
    }
}

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

#[async_trait]
impl Provider for OpenCode {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn name(&self) -> &'static str {
        "OpenCode"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::Cookie
    }

    fn doc_url(&self) -> &'static str {
        "https://opencode.ai"
    }

    fn is_configured(&self, config: &Config) -> bool {
        super::has_cookies(config, self.id(), DOMAINS, Want::Any(SESSION_NAMES))
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        ctx.with_cookies(
            self.id(),
            DOMAINS,
            Want::Any(SESSION_NAMES),
            "Sign in at opencode.ai, or paste a cookie header in Settings",
            |cookie| async move { fetch_with(ctx, &cookie).await },
        )
        .await
    }
}

async fn fetch_with(ctx: &FetchContext, cookie: &str) -> Result<UsageSnapshot, ProviderError> {
    // TanStack Start routes server functions by id, and echoes it back in a header.
    let instance = format!("server-fn:{}", Utc::now().timestamp_micros());
    let common = |referer: &'static str, function_id: &'static str| {
        [
            (
                "Accept",
                "text/javascript, application/json;q=0.9, */*;q=0.8",
            ),
            ("Origin", BASE),
            ("Referer", referer),
            ("User-Agent", USER_AGENT),
            ("X-Server-Id", function_id),
        ]
    };

    let workspaces = web_get(
        &ctx.http,
        &server_url(WORKSPACES_FN, None),
        &[
            common(BASE, WORKSPACES_FN).as_slice(),
            &[("X-Server-Instance", instance.as_str()), ("Cookie", cookie)],
        ]
        .concat(),
        SIGNED_OUT,
    )
    .await?;
    // A signed out session answers 200 with a login page, so this `Auth` is what lets
    // the walk move on to the next browser.
    if looks_signed_out(&workspaces) {
        return Err(ProviderError::Auth(SIGNED_OUT.to_string()));
    }
    let workspace = workspace_ids(&workspaces)
        .into_iter()
        .next()
        .ok_or_else(|| ProviderError::Parse("OpenCode returned no workspace id".into()))?;

    let referer = format!("{BASE}/workspace/{workspace}/billing");
    let body = web_get(
        &ctx.http,
        &server_url(SUBSCRIPTION_FN, Some(&format!("[\"{workspace}\"]"))),
        &[
            common(BASE, SUBSCRIPTION_FN).as_slice(),
            &[
                ("Referer", referer.as_str()),
                ("X-Server-Instance", instance.as_str()),
                ("Cookie", cookie),
            ],
        ]
        .concat(),
        SIGNED_OUT,
    )
    .await?;
    if looks_signed_out(&body) {
        return Err(ProviderError::Auth(SIGNED_OUT.to_string()));
    }
    if body.trim().eq_ignore_ascii_case("null") {
        return Err(ProviderError::Http(format!(
            "no OpenCode subscription usage for workspace {workspace}. That workspace \
                 has no subscription quota data"
        )));
    }
    let (rolling, weekly) = parse_subscription(&body)?;
    Ok(to_snapshot(&rolling, &weekly))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialized JavaScript exactly as OpenCodeUsageParserTests captures it.
    const JS_WORKSPACES: &str = concat!(
        r#";0x00000089;((self.$R=self.$R||{})["codexbar"]=[],"#,
        r#"($R=>$R[0]=[$R[1]={id:"wrk_01K6AR1ZET89H8NB691FQ2C2VB",name:"Default",slug:null}])"#,
        r#"($R["codexbar"]))"#,
    );
    const JS_SUBSCRIPTION: &str = concat!(
        r#"$R[16]($R[30],$R[41]={rollingUsage:$R[42]={status:"ok",resetInSec:5944,usagePercent:17},"#,
        r#"weeklyUsage:$R[43]={status:"ok",resetInSec:278201,usagePercent:75}});"#,
    );

    #[test]
    fn finds_the_workspace_id_in_serialized_javascript() {
        assert_eq!(
            workspace_ids(JS_WORKSPACES),
            ["wrk_01K6AR1ZET89H8NB691FQ2C2VB"]
        );
        // A JSON workspace list yields the same ids, deduplicated and in order.
        assert_eq!(
            workspace_ids(r#"[{"id":"wrk_aaa"},{"id":"wrk_bbb"},{"id":"wrk_aaa"}]"#),
            ["wrk_aaa", "wrk_bbb"]
        );
        assert!(workspace_ids("no ids here").is_empty());
    }

    #[test]
    fn parses_the_serialized_javascript_subscription() {
        let (rolling, weekly) = parse_subscription(JS_SUBSCRIPTION).unwrap();
        assert_eq!(rolling.used_percent, 17.0);
        assert_eq!(rolling.reset_in_sec, 5944);
        assert_eq!(weekly.used_percent, 75.0);
        assert_eq!(weekly.reset_in_sec, 278_201);

        let snap = to_snapshot(&rolling, &weekly);
        let primary = snap.primary.unwrap();
        assert_eq!(primary.label, "5-hour");
        assert_eq!(primary.window_minutes, Some(300));
        assert_eq!(snap.secondary.unwrap().window_minutes, Some(7 * 24 * 60));
    }

    #[test]
    fn parses_json_payloads_including_the_usage_wrapper_and_fractions() {
        let (rolling, weekly) = parse_subscription(
            r#"{"usage":{"rollingUsage":{"usagePercent":0.25,"resetInSec":3600},
                "weeklyUsage":{"usagePercent":75,"resetInSec":7200}}}"#,
        )
        .unwrap();
        // A direct percent at or below 1 is a fraction.
        assert_eq!(rolling.used_percent, 25.0);
        assert_eq!(rolling.reset_in_sec, 3600);
        assert_eq!(weekly.used_percent, 75.0);
    }

    #[test]
    fn computes_a_percentage_from_used_and_limit() {
        let (rolling, weekly) = parse_subscription(
            r#"{"rollingUsage":{"used":25,"limit":100,"resetInSec":600},
                "weeklyUsage":{"used":50,"limit":200,"resetInSec":3600}}"#,
        )
        .unwrap();
        // A computed ratio is already a percentage: 25 of 100 is 25%, not 2500%.
        assert_eq!(rolling.used_percent, 25.0);
        assert_eq!(weekly.used_percent, 25.0);
    }

    #[test]
    fn a_reset_timestamp_outside_range_reads_as_no_countdown() {
        let (rolling, _) = parse_subscription(
            r#"{"rollingUsage":{"usagePercent":17,"resetAt":"1e309"},
                "weeklyUsage":{"usagePercent":75,"resetInSec":7200}}"#,
        )
        .unwrap();
        assert_eq!(rolling.reset_in_sec, 0);
    }

    #[test]
    fn missing_windows_are_a_parse_error_and_signed_out_bodies_are_detected() {
        assert!(parse_subscription(r#"{"rollingUsage":{"usagePercent":17}}"#).is_err());
        assert!(parse_subscription("null").is_err());
        assert!(looks_signed_out(
            "<html><a href=\"/auth/authorize\">Sign in</a></html>"
        ));
        assert!(!looks_signed_out(JS_SUBSCRIPTION));
    }

    #[test]
    fn server_urls_carry_the_function_id_and_encoded_args() {
        assert_eq!(
            server_url("abc", Some(r#"["wrk_1"]"#)),
            "https://opencode.ai/_server?id=abc&args=%5B%22wrk_1%22%5D"
        );
        assert_eq!(
            server_url("abc", None),
            "https://opencode.ai/_server?id=abc"
        );
    }
}
