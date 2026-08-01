//! Qwen Cloud token plan (individual) usage, authenticated by a browser session cookie.
//!
//! Ported from `Sources/CodexBarCore/Providers/QwenCloud/`: `QwenCloudUsageFetcher.swift`
//! (endpoint layout and the sec_token flow), `QwenCloudTokenPlanAPIClient.swift` (the
//! `cornerstoneParam` envelope and the form body), `QwenCloudUsageParser.swift` (the
//! response shape) and `QwenCloudCookieImporter.swift` (the domains and the ticket cookie
//! names that prove a signed in session). Plus the shared
//! `Shared/AliyunOneConsole/OneConsoleSECTokenResolver.swift` and `OneConsoleJSON.swift`.
//!
//! Qwen Cloud rides Aliyun's one-console gateway, so a request needs three things at once:
//! the session jar, a `sec_token` that only exists in the dashboard page (or in the jar,
//! or on the user-info endpoint), and the CSRF cookie echoed back as a header. The usage
//! call is required; subscription and quota-config are metadata and a failure there costs
//! a plan name, not the fetch.
//!
//! Never log the header, the sec_token or any cookie value.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use reqwest::Client;
use serde_json::{json, Map, Value};

use super::api_token::{loose_f64, parse_rfc3339, TIMEOUT};
use super::util::{web_get, web_send};
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow, Want};
use crate::config::Config;

const GATEWAY: &str = "https://home.qwencloud.com";
const DATA_GATEWAY: &str = "https://cs-data.qwencloud.com";
const DASHBOARD: &str = "https://home.qwencloud.com/billing/subscription/token-plan-individual";
const USER_INFO: &str = "https://home.qwencloud.com/tool/user/info.json";

/// The international "token plan (individual)" commodity, `QwenCloudUsageFetcher.productCode`.
const PRODUCT_CODE: &str = "sfm_tokenplansolo_public_intl";
const CONSOLE_PRODUCT: &str = "sfm_bailian";
const CONSOLE_ACTION: &str = "IntlBroadScopeAspnGateway";
const REGION: &str = "ap-southeast-1";
const LANGUAGE: &str = "en-US";

/// Gateway API names. Only `[A-Za-z0-9./]`, so they go into the query string verbatim.
const USAGE_API: &str = "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage";
const SUBSCRIPTION_API: &str = "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/subscription";
const QUOTA_CONFIG_API: &str = "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/quota-config";

/// Hosts whose cookies may authenticate Qwen Cloud.
///
/// `QwenCloudCookieImport.cookieDomains` lists nine hosts across qwencloud.com,
/// alibabacloud.com and aliyun.com, but that is the DISCOVERY set: Swift then builds the
/// header through `OneConsoleCookieHeaderBuilder.header(from:targetURL:)`, which keeps
/// only the cookies whose domain matches the request URL. Every request this module makes
/// goes to `home.qwencloud.com` or `cs-data.qwencloud.com`, so that filter leaves exactly
/// the `qwencloud.com` jar, and this is the suffix that reproduces it.
///
/// `Want::Jar` gates and sends the same set, so widening this to the passport domains
/// would ship a user's Alibaba Cloud console cookies to a host the browser itself would
/// never send them to, and would stack a second `login_aliyunid_ticket` from another
/// domain into the header. It would also mark an Alibaba-Cloud-only profile as configured
/// for a Qwen session it does not have.
const DOMAINS: &[&str] = &["qwencloud.com"];

/// `QwenCloudCookieImport.authTicketCookies`. Any ONE of these marks the jar as signed in;
/// the whole jar is then sent. Locale, account-id and CSRF cookies are deliberately not in
/// this list: a logged out profile that merely visited qwencloud.com already carries them,
/// and treating it as authenticated would re-import the same dead profile forever.
const SESSION_NAMES: &[&str] = &[
    "login_aliyunid_ticket",
    "login_qwencloud_ticket",
    "qwen_sso_ticket",
];

const SIGNIN_HINT: &str = "Qwen Cloud login required. Sign in at \
     https://home.qwencloud.com/billing/subscription/token-plan-individual, or paste a \
     cookie header in Settings";

pub struct QwenCloud;

// ------------------------------------------------------------------ JSON traversal
//
// `OneConsoleJSON.swift`. The gateway wraps its payload in one or two layers of
// double-stringified JSON and moves the quota object around between account types, so
// these walk the tree instead of committing to a schema.

/// Recursively replace any string that is itself JSON with the parsed value, so
/// `{"data":"{\"foo\":1}"}` reads as `{"data":{"foo":1}}`.
fn expand(value: Value) -> Value {
    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(inner) = serde_json::from_str::<Value>(trimmed) {
                    return expand(inner);
                }
            }
            Value::String(s)
        }
        Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, expand(v))).collect()),
        Value::Array(items) => Value::Array(items.into_iter().map(expand).collect()),
        other => other,
    }
}

/// The first object anywhere in the tree that carries any of `keys`.
fn find_object<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Map<String, Value>> {
    match value {
        Value::Object(map) => {
            if keys.iter().any(|k| map.contains_key(*k)) {
                return Some(map);
            }
            map.values().find_map(|v| find_object(v, keys))
        }
        Value::Array(items) => items.iter().find_map(|v| find_object(v, keys)),
        _ => None,
    }
}

/// The first value anywhere in the tree stored under `key`, matched case insensitively.
fn find_value<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
            .or_else(|| map.values().find_map(|v| find_value(v, key))),
        Value::Array(items) => items.iter().find_map(|v| find_value(v, key)),
        _ => None,
    }
}

/// The first non-empty string under any of `keys`, one key at a time so caller priority
/// survives the whole tree.
fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let found = find_value(value, key)?.as_str()?.trim();
        (!found.is_empty()).then(|| found.to_string())
    })
}

/// A 0..1 consumed ratio as percentage points. Not finite is unknown, not zero.
fn percentage_points(ratio: Option<f64>) -> Option<f64> {
    ratio
        .filter(|r| r.is_finite())
        .map(|r| r.clamp(0.0, 1.0) * 100.0)
}

/// A reset stamp: epoch seconds or milliseconds (told apart by magnitude, as
/// `OneConsoleJSON.date` does), RFC 3339, or a bare `yyyy-MM-dd[ HH:mm[:ss]]`.
///
/// A bare stamp carries no zone. It is read as UTC, which is what every other reset in
/// this app is: guessing the machine's local zone for a server-side reset would shift the
/// countdown by hours for anyone outside it.
fn json_date(value: Option<&Value>) -> Option<DateTime<Utc>> {
    if let Some(number) = loose_f64(value).filter(|n| *n > 0.0) {
        let secs = if number >= 1e12 {
            number / 1000.0
        } else {
            number
        };
        return DateTime::from_timestamp(secs as i64, 0);
    }
    let text = value?.as_str()?.trim();
    if let Some(at) = parse_rfc3339(text) {
        return Some(at);
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return Some(naive.and_utc());
        }
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .map(|d| d.and_time(chrono::NaiveTime::MIN).and_utc())
}

// ------------------------------------------------------------------ mapping
//
// `QwenCloudUsageParser.swift` plus `QwenCloudUsageSnapshot.toUsageSnapshot()`.
//
// ponytail: the legacy `AlibabaTokenPlanUsageFetcher` envelope is NOT ported. It is the
// fallback shape of a different CodexBar provider, and this one has never shipped against
// it. Add it if a real account ever answers the token plan APIs in that shape.

/// Plan tier keys, in the precedence `QwenCloudUsageParser.planCode` uses.
const PLAN_KEYS: &[&str] = &["specCode", "spec_code", "planName", "plan_name"];

fn plan_code(subscription: &str) -> Option<String> {
    let body = expand(serde_json::from_str(subscription).ok()?);
    let plan = find_object(&body, PLAN_KEYS)?;
    PLAN_KEYS.iter().find_map(|key| {
        let value = plan.get(*key)?.as_str()?.trim().to_lowercase();
        (!value.is_empty()).then_some(value)
    })
}

/// The four documented tiers get their capitalisation back; anything else keeps the code
/// the API sent, which is more honest than a guessed display name.
fn display_plan_name(code: &str) -> String {
    match code {
        "lite" => "Lite".to_string(),
        "standard" => "Standard".to_string(),
        "pro" => "Pro".to_string(),
        "max" => "Max".to_string(),
        other => other.to_string(),
    }
}

/// The credit ceilings for one tier, `(five_hour, weekly)`. `None` when quota-config did
/// not answer, did not name this tier, or named it with no numbers.
fn quota_totals(quota_config: &str, code: &str) -> Option<(Option<f64>, Option<f64>)> {
    let body = expand(serde_json::from_str(quota_config).ok()?);
    let quota = find_value(&body, code)?.as_object()?;
    let five_hour = loose_f64(quota.get("five_hour").or_else(|| quota.get("fiveHour")));
    let weekly = loose_f64(quota.get("weekly"));
    (five_hour.is_some() || weekly.is_some()).then_some((five_hour, weekly))
}

/// The usage response is required; subscription and quota-config are metadata.
fn to_snapshot(
    usage: &str,
    subscription: Option<&str>,
    quota_config: Option<&str>,
) -> Result<UsageSnapshot, ProviderError> {
    let body =
        expand(serde_json::from_str(usage).map_err(|e| ProviderError::Parse(e.to_string()))?);
    let block =
        find_object(&body, &["per5HourPercentage", "per1WeekPercentage"]).ok_or_else(|| {
            ProviderError::Parse(
                "no per5HourPercentage or per1WeekPercentage in the Qwen Cloud token plan usage \
                 response"
                    .to_string(),
            )
        })?;

    let five_hour = percentage_points(loose_f64(block.get("per5HourPercentage")));
    let weekly = percentage_points(loose_f64(block.get("per1WeekPercentage")));
    if five_hour.is_none() && weekly.is_none() {
        return Err(ProviderError::Parse(
            "Qwen Cloud reported neither a 5 hour nor a weekly percentage".to_string(),
        ));
    }

    let code = subscription.and_then(plan_code);
    let totals = match (&code, quota_config) {
        (Some(code), Some(config)) => quota_totals(config, code),
        _ => None,
    };

    let mut snapshot = UsageSnapshot::new("qwen");
    snapshot.primary = five_hour.map(|used| {
        UsageWindow::new(
            "5h",
            Some(used),
            json_date(block.get("per5HourResetTime")),
            Some(5 * 60),
        )
    });
    snapshot.secondary = weekly.map(|used| {
        UsageWindow::new(
            "Weekly",
            Some(used),
            json_date(block.get("per1WeekResetTime")),
            Some(7 * 24 * 60),
        )
    });
    snapshot.plan = code.as_deref().map(display_plan_name);
    // Qwen reports a percentage, never a balance, so the only balance we can state is the
    // tier's own ceiling minus the share a lane says is gone. The weekly ceiling is the
    // plan's real budget; the 5 hour one is a burst cap inside it, which is why it is only
    // the fallback. `credits` reads as "left to spend" everywhere else in the app.
    snapshot.credits = [
        (totals.and_then(|t| t.1), weekly),
        (totals.and_then(|t| t.0), five_hour),
    ]
    .into_iter()
    .find_map(|(total, used)| {
        let total = total.filter(|t| *t > 0.0)?;
        Some(total * (100.0 - used?) / 100.0)
    });
    Ok(snapshot)
}

// ------------------------------------------------------------------ sec_token

/// `QwenCloudUsageFetcher.looksLikeLoginPage`. The dashboard answers 200 with the sign-in
/// shell when the session is dead, so the status code alone cannot tell us.
fn looks_like_login_page(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    [
        "passport.alibabacloud.com",
        "signin.aliyun.com",
        "account.alibabacloud.com/login",
        "login.qwencloud.com",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        || (lower.contains("login") && lower.contains("password") && lower.contains("sign in"))
}

/// `secToken` / `sec_token` / `csrfToken` as an inline JS constant or a JSON field, in the
/// precedence `OneConsoleSECTokenResolver.extractToken` uses. Hand written rather than
/// regex: the `regex` crate is not a dependency here and this is one scan for a literal.
fn extract_token(html: &str) -> Option<String> {
    for key in ["secToken", "sec_token", "csrfToken"] {
        let mut rest = html;
        while let Some(at) = rest.find(key) {
            rest = &rest[at + key.len()..];
            if let Some(value) = quoted_after_assignment(rest) {
                return Some(value);
            }
        }
    }
    None
}

/// The quoted string in `": \"value\""` or `" = 'value'"`, allowing the key's own closing
/// quote first so `"secToken": "x"` and `secToken = 'x'` both read.
fn quoted_after_assignment(text: &str) -> Option<String> {
    let after_key = text.trim_start();
    let after_key = after_key.strip_prefix(['"', '\'']).unwrap_or(after_key);
    let body = after_key
        .trim_start()
        .strip_prefix([':', '='])?
        .trim_start();
    let quote = body.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let body = &body[quote.len_utf8()..];
    let value = body[..body.find(quote)?].trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// One `name=value` out of a `Cookie` header. The RETURN VALUE IS A SECRET.
fn cookie_value(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|pair| {
        let (key, value) = pair.trim().split_once('=')?;
        (key.trim().eq_ignore_ascii_case(name) && !value.is_empty())
            .then(|| value.trim().to_string())
    })
}

/// Every gateway call needs a `sec_token`. Three sources, freshest first, exactly as
/// `OneConsoleSECTokenResolver.resolve` orders them: the dashboard HTML, a `sec_token`
/// cookie, then the user-info endpoint. Nothing found means the session is dead, which is
/// an `Auth` so the walk tries the next browser.
async fn sec_token(http: &Client, cookie: &str) -> Result<String, ProviderError> {
    let headers = [
        ("Accept", "text/html,application/xhtml+xml"),
        ("Cookie", cookie),
    ];
    // A transport failure here is not fatal: the cookie and user-info fallbacks may still
    // answer. An `Auth` is, and it must reach the walk unchanged. Anything else is kept,
    // because `OneConsoleSECTokenResolver.resolve` rethrows the retained dashboard failure
    // rather than reporting a missing session, and the difference is not cosmetic: an
    // `Auth` wipes the last good windows and paints the tile red, so a 502 must not be
    // spelled "sign in again".
    let mut dashboard_failure = None;
    match web_get(http, DASHBOARD, &headers, SIGNIN_HINT).await {
        Ok(html) => {
            if looks_like_login_page(&html) {
                return Err(ProviderError::Auth(SIGNIN_HINT.to_string()));
            }
            if let Some(token) = extract_token(&html) {
                return Ok(token);
            }
        }
        Err(e @ ProviderError::Auth(_)) => return Err(e),
        Err(e) => dashboard_failure = Some(e),
    }

    if let Some(token) = cookie_value(cookie, "sec_token") {
        return Ok(token);
    }

    let headers = [
        ("Accept", "application/json, text/plain, */*"),
        ("Cookie", cookie),
    ];
    if let Ok(text) = web_get(http, USER_INFO, &headers, SIGNIN_HINT).await {
        if let Ok(body) = serde_json::from_str::<Value>(&text) {
            if let Some(token) = find_string(
                &expand(body),
                &["secToken", "sec_token", "csrfToken", "token"],
            ) {
                return Ok(token);
            }
        }
    }

    Err(dashboard_failure.unwrap_or_else(|| ProviderError::Auth(SIGNIN_HINT.to_string())))
}

// ------------------------------------------------------------------ gateway client

/// A uuid SHAPED trace id for `cornerstoneParam.feTraceId`. The gateway only echoes it in
/// its own logs, so it needs to be well formed and distinct, not cryptographically random,
/// and that is not worth a `uuid` dependency.
fn trace_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let tail = nanos.rotate_left(31)
        ^ SEQ
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        nanos >> 32,
        (nanos >> 16) & 0xffff,
        nanos & 0xffff,
        (tail >> 48) & 0xffff,
        tail & 0xffff_ffff_ffff
    )
}

/// `QwenCloudTokenPlanAPIClient.makeRequest`: the `cornerstoneParam` envelope inside a
/// `params` JSON field, form encoded, with the CSRF cookie echoed back as a header.
async fn call_api(
    http: &Client,
    cookie: &str,
    token: &str,
    api: &str,
    data: &[(&str, &str)],
) -> Result<String, ProviderError> {
    let mut payload = Map::new();
    for (key, value) in data {
        payload.insert((*key).to_string(), Value::String((*value).to_string()));
    }
    let mut cornerstone = json!({
        "feTraceId": trace_id(),
        "feURL": DASHBOARD,
        "protocol": "V2",
        "console": "ONE_CONSOLE",
        "productCode": "p_efm",
        "domain": "home.qwencloud.com",
        "consoleSite": "QWENCLOUD",
        "userNickName": "",
        "userPrincipalName": "",
        "xsp_lang": LANGUAGE,
    });
    if let Some(anonymous) = cookie_value(cookie, "cna") {
        cornerstone["X-Anonymous-Id"] = Value::String(anonymous);
    }
    payload.insert("cornerstoneParam".to_string(), cornerstone);
    let params = json!({ "Api": api, "V": "1.0", "Data": Value::Object(payload) }).to_string();

    // `api` is one of our own constants, `[A-Za-z0-9./]` only, so it needs no escaping.
    let url =
        format!("{DATA_GATEWAY}/data/api.json?action={CONSOLE_ACTION}&product={CONSOLE_PRODUCT}&api={api}&_v=undefined");
    let mut req = http
        .post(url)
        .timeout(TIMEOUT)
        .header("Accept", "application/json, text/plain, */*")
        .header("Cookie", cookie)
        .header("Origin", GATEWAY)
        .header("Referer", DASHBOARD)
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&[
            ("product", CONSOLE_PRODUCT),
            ("action", CONSOLE_ACTION),
            ("sec_token", token),
            ("region", REGION),
            ("language", LANGUAGE),
            ("params", &params),
        ]);
    if let Some(csrf) =
        cookie_value(cookie, "login_aliyunid_csrf").or_else(|| cookie_value(cookie, "csrf"))
    {
        req = req
            .header("x-xsrf-token", &csrf)
            .header("x-csrf-token", &csrf);
    }
    web_send(req, SIGNIN_HINT).await
}

// ------------------------------------------------------------------ provider

#[async_trait]
impl Provider for QwenCloud {
    fn id(&self) -> &'static str {
        "qwen"
    }

    fn name(&self) -> &'static str {
        "Qwen Cloud"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::Cookie
    }

    fn doc_url(&self) -> &'static str {
        DASHBOARD
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
                let token = sec_token(&ctx.http, &cookie).await?;
                let usage = call_api(&ctx.http, &cookie, &token, USAGE_API, &[]).await?;
                // Metadata. A failure here costs a plan name and a credit balance, not the
                // fetch, so it must not walk to another browser either.
                let subscription = call_api(
                    &ctx.http,
                    &cookie,
                    &token,
                    SUBSCRIPTION_API,
                    &[("commodityCode", PRODUCT_CODE)],
                )
                .await
                .ok();
                let quota_config = call_api(&ctx.http, &cookie, &token, QUOTA_CONFIG_API, &[])
                    .await
                    .ok();
                to_snapshot(&usage, subscription.as_deref(), quota_config.as_deref())
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gateway shape from Tests/CodexBarTests/QwenCloudProviderTests.swift, including
    /// the double-stringified `DataV2.data` envelope the real console sends.
    const NESTED_USAGE: &str = r#"{
        "data": {
            "DataV2": {
                "data": "{\"code\":0,\"data\":{\"per5HourPercentage\":0.03,\"per5HourResetTime\":1700003600000,\"per1WeekPercentage\":0.01,\"per1WeekResetTime\":1700086400000},\"success\":true}"
            }
        },
        "httpStatusCode": 200
    }"#;

    const FLAT_USAGE: &str = r#"{
        "data": {
            "per5HourPercentage": 0.03,
            "per5HourResetTime": 1700003600000,
            "per1WeekPercentage": 0.01,
            "per1WeekResetTime": 1700086400000
        }
    }"#;

    const SUBSCRIPTION: &str = r#"{"data":{"specCode":"standard","status":"VALID"}}"#;

    const QUOTA_CONFIG: &str = r#"{
        "data": {
            "lite": { "five_hour": 1000, "weekly": 10000 },
            "standard": { "five_hour": 5000, "weekly": 50000 },
            "pro": { "five_hour": 10000, "weekly": 100000 }
        }
    }"#;

    /// The reset stamps in the fixtures are from 2023, so a `UsageWindow` would correctly
    /// drop them as elapsed. Read the raw block instead when asserting on them.
    fn resets(usage: &str) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
        let body = expand(serde_json::from_str(usage).unwrap());
        let block = find_object(&body, &["per5HourPercentage"]).unwrap();
        (
            json_date(block.get("per5HourResetTime")),
            json_date(block.get("per1WeekResetTime")),
        )
    }

    #[test]
    fn maps_the_five_hour_and_weekly_lanes_through_the_embedded_envelope() {
        let snap = to_snapshot(NESTED_USAGE, None, None).unwrap();
        let primary = snap.primary.unwrap();
        assert_eq!(primary.label, "5h");
        // A 0..1 consumed ratio, not a percentage: 0.03 is 3 percent used.
        assert_eq!(primary.used_percent, Some(3.0));
        assert_eq!(primary.window_minutes, Some(300));
        let secondary = snap.secondary.unwrap();
        assert_eq!(secondary.label, "Weekly");
        assert_eq!(secondary.used_percent, Some(1.0));
        assert_eq!(secondary.window_minutes, Some(7 * 24 * 60));
        // Epoch milliseconds, told apart from seconds by magnitude.
        let (five, weekly) = resets(NESTED_USAGE);
        assert_eq!(five.unwrap().timestamp(), 1_700_003_600);
        assert_eq!(weekly.unwrap().timestamp(), 1_700_086_400);
        // No subscription call, so no plan and no balance to state.
        assert_eq!(snap.plan, None);
        assert_eq!(snap.credits, None);
    }

    #[test]
    fn the_plan_tier_names_the_credit_ceiling() {
        let snap = to_snapshot(FLAT_USAGE, Some(SUBSCRIPTION), Some(QUOTA_CONFIG)).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Standard"));
        // The weekly ceiling is the plan's budget: 50000 with 1 percent gone.
        assert_eq!(snap.credits, Some(49_500.0));
    }

    #[test]
    fn quota_config_without_a_matching_tier_states_no_balance() {
        let unknown = r#"{"data":{"specCode":"enterprise"}}"#;
        let snap = to_snapshot(FLAT_USAGE, Some(unknown), Some(QUOTA_CONFIG)).unwrap();
        // An unmapped code is shown as the API spelled it rather than guessed at.
        assert_eq!(snap.plan.as_deref(), Some("enterprise"));
        assert_eq!(snap.credits, None);
        // Metadata that never answered at all is the same story.
        let snap = to_snapshot(FLAT_USAGE, None, Some(QUOTA_CONFIG)).unwrap();
        assert_eq!(snap.credits, None);
    }

    #[test]
    fn the_five_hour_ceiling_backs_the_weekly_one() {
        let five_only = r#"{"data":{"per5HourPercentage":0.5}}"#;
        let snap = to_snapshot(five_only, Some(SUBSCRIPTION), Some(QUOTA_CONFIG)).unwrap();
        assert!(snap.secondary.is_none());
        assert_eq!(snap.credits, Some(2_500.0));
    }

    #[test]
    fn ratios_are_clamped_and_a_missing_lane_is_unknown_not_zero() {
        let snap = to_snapshot(r#"{"per5HourPercentage":1.4}"#, None, None).unwrap();
        assert_eq!(snap.primary.unwrap().used_percent, Some(100.0));
        assert!(snap.secondary.is_none());
        let snap = to_snapshot(r#"{"per1WeekPercentage":0}"#, None, None).unwrap();
        assert!(snap.primary.is_none());
        assert_eq!(snap.secondary.unwrap().used_percent, Some(0.0));
    }

    #[test]
    fn a_response_with_no_usable_percentage_is_a_parse_error() {
        for body in [
            r#"{"data":{"status":"VALID"}}"#,
            r#"{"per5HourPercentage":"soon"}"#,
            "not json",
        ] {
            assert!(
                matches!(to_snapshot(body, None, None), Err(ProviderError::Parse(_))),
                "{body}"
            );
        }
    }

    #[test]
    fn reset_stamps_read_as_seconds_milliseconds_and_text() {
        let at = |raw: &str| json_date(Some(&serde_json::from_str::<Value>(raw).unwrap()));
        assert_eq!(at("1700003600").unwrap().timestamp(), 1_700_003_600);
        assert_eq!(at("1700003600000").unwrap().timestamp(), 1_700_003_600);
        assert_eq!(at(r#""1700003600""#).unwrap().timestamp(), 1_700_003_600);
        assert_eq!(
            at(r#""2033-01-01T00:00:00Z""#).unwrap().timestamp(),
            1_988_150_400
        );
        // No zone in the text means UTC, never the machine's local zone.
        assert_eq!(
            at(r#""2033-01-01 00:00:00""#).unwrap().timestamp(),
            1_988_150_400
        );
        assert_eq!(
            at(r#""2033-01-01 00:00""#).unwrap().timestamp(),
            1_988_150_400
        );
        assert_eq!(at(r#""2033-01-01""#).unwrap().timestamp(), 1_988_150_400);
        assert_eq!(at("0"), None);
        assert_eq!(at(r#""whenever""#), None);
        assert_eq!(json_date(None), None);
    }

    /// The sec_token lives in the dashboard HTML in half a dozen spellings, and a dead
    /// session gets the sign-in shell back with a 200.
    #[test]
    fn the_sec_token_is_found_in_every_spelling_and_the_login_shell_is_recognised() {
        for html in [
            r#"<script>window.ALIYUN_CONSOLE_CONFIG={"secToken":"tok"};</script>"#,
            r#"<script>{"sec_token":"tok"}</script>"#,
            r#"<script>sec_token = "tok";</script>"#,
            r#"<script>secToken:'tok'</script>"#,
            r#"<script>csrfToken = "tok";</script>"#,
        ] {
            assert_eq!(extract_token(html).as_deref(), Some("tok"), "{html}");
        }
        // A mention with no value must not stop the scan finding the real one.
        assert_eq!(
            extract_token(r#"<div data-secToken></div><script>secToken="tok"</script>"#).as_deref(),
            Some("tok")
        );
        assert_eq!(extract_token("<html>nothing here</html>"), None);
        assert_eq!(extract_token(r#"secToken = ""#), None);

        assert!(looks_like_login_page(
            r#"<a href="https://signin.aliyun.com/login">Sign in</a>"#
        ));
        assert!(looks_like_login_page(
            "<form>Login with your Password to Sign In</form>"
        ));
        assert!(!looks_like_login_page(
            "<html>Token Plan (Individual)</html>"
        ));
    }

    #[test]
    fn cookie_values_are_read_by_name_case_insensitively() {
        let header = "cna=abc; Login_AliyunID_Csrf=xyz; sec_token=; other=1";
        assert_eq!(cookie_value(header, "cna").as_deref(), Some("abc"));
        assert_eq!(
            cookie_value(header, "login_aliyunid_csrf").as_deref(),
            Some("xyz")
        );
        // An empty value is not a value.
        assert_eq!(cookie_value(header, "sec_token"), None);
        assert_eq!(cookie_value(header, "missing"), None);
    }

    #[test]
    fn the_trace_id_is_uuid_shaped_and_distinct() {
        let one = trace_id();
        assert_eq!(one.len(), 36);
        assert_eq!(
            one.split('-').map(str::len).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(one.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
        assert_ne!(one, trace_id());
    }

    /// Live probe against this machine's real browsers and the real Qwen Cloud gateway.
    /// Prints lane labels, percentages, plan and reset only. Never a cookie value, never
    /// the sec_token.
    ///
    /// Run with: cargo test -- --ignored --nocapture qwen_live
    #[tokio::test]
    #[ignore = "hits the real browser profiles and the real Qwen Cloud API"]
    async fn qwen_live() {
        let ctx = FetchContext {
            http: reqwest::Client::new(),
            config: crate::config::Config::load(),
        };
        let provider = QwenCloud;
        if !provider.is_configured(&ctx.config) {
            println!("Qwen Cloud: no session on this machine, skipped");
            return;
        }
        match provider.fetch(&ctx).await {
            Ok(snap) => {
                for lane in [&snap.primary, &snap.secondary].into_iter().flatten() {
                    println!(
                        "Qwen Cloud: {} {:?}% used, resets {:?}, window {:?}",
                        lane.label, lane.used_percent, lane.resets_at, lane.window_minutes
                    );
                    assert!(lane.used_percent.is_none_or(|p| (0.0..=100.0).contains(&p)));
                }
                println!(
                    "Qwen Cloud: plan {:?}, credits {:?}",
                    snap.plan, snap.credits
                );
                assert!(
                    snap.primary.is_some() || snap.secondary.is_some(),
                    "no lane at all"
                );
            }
            Err(e) => println!("Qwen Cloud: FAILED: {e}"),
        }
    }
}
