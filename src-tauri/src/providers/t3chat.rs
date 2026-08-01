//! T3 Chat usage, authenticated by a browser session cookie.
//!
//! Ported from `Sources/CodexBarCore/Providers/T3Chat/`: the `getCustomerData` tRPC call
//! on `t3.chat`, whose reply is a JSON lines stream with the customer object nested at an
//! unstable depth. Lane labels come from `T3ChatProviderDescriptor` (Base / Overage).
//!
//! Never log the header, only cookie names and counts.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use super::api_token::epoch_to_utc;
use super::util::web_get;
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow, Want};
use crate::config::Config;

const DOMAINS: &[&str] = &["t3.chat"];

/// CodexBar sends every t3.chat cookie and lets the API judge it. So does this port: the
/// whole jar travels, including the Vercel clearance cookies the bot mitigation reads.
/// The name below only decides whether the jar looks signed in at all, which keeps a
/// merely anonymous visit from showing up as a configured provider that always errors.
/// If t3.chat renames it, this list is the one thing to update.
const SESSION_NAMES: &[&str] = &["session"];

const BASE: &str = "https://t3.chat";
const REFERER: &str = "https://t3.chat/settings/customization";
/// Captured from T3 Chat's own getCustomerData request shape (T3ChatUsageFetcher.input).
const INPUT: &str =
    r#"{"0":{"json":{"sessionId":null},"meta":{"values":{"sessionId":["undefined"]}}}}"#;
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";

pub struct T3Chat;

#[derive(Debug, Default, Deserialize)]
struct CustomerData {
    #[serde(rename = "subTier")]
    sub_tier: Option<String>,
    subscription: Option<Subscription>,
    #[serde(rename = "usageFourHourPercentage")]
    four_hour_percent: Option<f64>,
    #[serde(rename = "usageMonthPercentage")]
    month_percent: Option<f64>,
    #[serde(rename = "usagePeriodPercentage")]
    period_percent: Option<f64>,
    #[serde(rename = "usageFourHourNextResetAt")]
    four_hour_reset: Option<f64>,
    #[serde(rename = "usageWindowNextResetAt")]
    window_reset: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct Subscription {
    #[serde(rename = "productName")]
    product_name: Option<String>,
    #[serde(rename = "currentPeriodEnd")]
    current_period_end: Option<f64>,
}

/// `product-name` reads as `Product Name`.
fn plan_name(data: &CustomerData) -> Option<String> {
    let raw = data
        .subscription
        .as_ref()
        .and_then(|s| s.product_name.as_deref())
        .or(data.sub_tier.as_deref())?
        .trim();
    if raw.is_empty() {
        return None;
    }
    Some(
        raw.split('-')
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// T3 Chat sends JavaScript epoch milliseconds, while some subscription fields are
/// seconds. [`epoch_to_utc`] already tells the two apart.
fn timestamp(raw: Option<f64>) -> Option<DateTime<Utc>> {
    let value = raw.filter(|v| v.is_finite() && *v > 0.0)?;
    epoch_to_utc(value as i64)
}

fn to_snapshot(data: &CustomerData) -> UsageSnapshot {
    let mut snapshot = UsageSnapshot::new("t3chat");
    snapshot.primary = Some(UsageWindow::new(
        "Base",
        data.four_hour_percent,
        timestamp(data.four_hour_reset).or_else(|| timestamp(data.window_reset)),
        Some(4 * 60),
    ));
    snapshot.secondary = Some(UsageWindow::new(
        "Overage",
        data.month_percent.or(data.period_percent),
        // billingNextResetAt tracks the usage window, not the overage period, so an
        // unknown subscription period end stays unknown rather than borrowing it.
        timestamp(
            data.subscription
                .as_ref()
                .and_then(|s| s.current_period_end),
        ),
        None,
    ));
    snapshot.plan = plan_name(data);
    snapshot
}

/// The customer object sits at an unstable depth inside the tRPC envelope, so it is
/// found by its own field names rather than by path.
fn find_customer(value: &Value) -> Option<&Value> {
    if let Some(object) = value.as_object() {
        let looks_right = object.contains_key("usageFourHourPercentage")
            || object.contains_key("usageMonthPercentage")
            || (object.contains_key("subscription") && object.contains_key("usageBand"));
        if looks_right {
            return Some(value);
        }
        return object.values().find_map(find_customer);
    }
    value.as_array()?.iter().find_map(find_customer)
}

fn parse_jsonl(body: &str) -> Result<CustomerData, ProviderError> {
    for line in body.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(found) = find_customer(&value) {
            return serde_json::from_value(found.clone())
                .map_err(|e| ProviderError::Parse(e.to_string()));
        }
    }
    Err(ProviderError::Parse(
        "T3 Chat response had no customer data object".into(),
    ))
}

/// Percent encoding for the tRPC `input` query parameter.
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
impl Provider for T3Chat {
    fn id(&self) -> &'static str {
        "t3chat"
    }

    fn name(&self) -> &'static str {
        "T3 Chat"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::Cookie
    }

    fn doc_url(&self) -> &'static str {
        "https://t3.chat/settings/customization"
    }

    fn is_configured(&self, config: &Config) -> bool {
        super::has_cookies(config, self.id(), DOMAINS, Want::Jar(SESSION_NAMES))
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        ctx.with_cookies(
            self.id(),
            DOMAINS,
            Want::Jar(SESSION_NAMES),
            "Sign in at t3.chat, or paste a cookie header in Settings",
            |cookie| async move { fetch_with(ctx, &cookie).await },
        )
        .await
    }
}

async fn fetch_with(ctx: &FetchContext, cookie: &str) -> Result<UsageSnapshot, ProviderError> {
    // T3 Chat sits behind Vercel's bot mitigation, which reads the browser fingerprint
    // headers as well as the cookie, so the whole captured set travels with the call.
    let headers = [
        ("Accept", "*/*"),
        ("Accept-Language", "en-US,en;q=0.9"),
        ("Cache-Control", "no-cache"),
        ("Pragma", "no-cache"),
        ("Priority", "u=4"),
        ("Referer", REFERER),
        ("Origin", BASE),
        ("Sec-Fetch-Dest", "empty"),
        ("Sec-Fetch-Mode", "cors"),
        ("Sec-Fetch-Site", "same-origin"),
        ("trpc-accept", "application/jsonl"),
        ("x-trpc-batch", "true"),
        ("x-trpc-source", "web-client"),
        ("User-Agent", USER_AGENT),
        ("Cookie", cookie),
    ];
    let url = format!(
        "{BASE}/api/trpc/getCustomerData?batch=1&input={}",
        urlencode(INPUT)
    );
    // `web_get` maps 401 and 403 to `Auth`, which is what lets the next browser be tried.
    let body = web_get(
        &ctx.http,
        &url,
        &headers,
        "T3 Chat session cookie is invalid or expired. Sign in again at t3.chat",
    )
    .await?;
    Ok(to_snapshot(&parse_jsonl(&body)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Envelope shape from T3ChatUsageFetcherTests: the customer object arrives wrapped
    /// in tRPC's result/data layers, on its own line.
    const BODY: &str = concat!(
        r#"{"json":[[0,0,0,[0]]]}"#,
        "\n",
        r#"{"json":[[0,0,[{"result":{"data":{"subTier":"pro-plus","usageBand":"high","#,
        r#""usageFourHourPercentage":42.5,"usageMonthPercentage":88,"#,
        r#""usageFourHourNextResetAt":2000000000547,"#,
        r#""subscription":{"productName":"pro-plus","currentPeriodEnd":2000900000547}}}}]]]}"#,
    );

    #[test]
    fn finds_the_customer_object_and_maps_both_lanes() {
        let data = parse_jsonl(BODY).unwrap();
        let snap = to_snapshot(&data);

        let primary = snap.primary.unwrap();
        assert_eq!(primary.label, "Base");
        assert_eq!(primary.used_percent, Some(42.5));
        assert_eq!(primary.window_minutes, Some(240));
        assert_eq!(primary.resets_at.unwrap().timestamp(), 2_000_000_000);

        let secondary = snap.secondary.unwrap();
        assert_eq!(secondary.label, "Overage");
        assert_eq!(secondary.used_percent, Some(88.0));
        assert_eq!(secondary.window_minutes, None);
        assert_eq!(secondary.resets_at.unwrap().timestamp(), 2_000_900_000);

        assert_eq!(snap.plan.as_deref(), Some("Pro Plus"));
    }

    #[test]
    fn falls_back_to_the_period_percentage_and_leaves_unknown_resets_unset() {
        let data =
            parse_jsonl(r#"{"usageFourHourPercentage":10,"usagePeriodPercentage":60}"#).unwrap();
        let snap = to_snapshot(&data);
        assert_eq!(snap.secondary.unwrap().used_percent, Some(60.0));
        assert!(snap.primary.unwrap().resets_at.is_none());
        assert_eq!(snap.plan, None);
    }

    #[test]
    fn window_reset_backs_the_four_hour_reset_and_percentages_clamp() {
        let data =
            parse_jsonl(r#"{"usageMonthPercentage":140,"usageWindowNextResetAt":2000000000547}"#)
                .unwrap();
        let snap = to_snapshot(&data);
        // The payload carries no four-hour percentage, so the base lane is UNKNOWN.
        // It used to read as a confident 0 percent used.
        assert_eq!(snap.primary.as_ref().unwrap().used_percent, None);
        assert_eq!(
            snap.primary.unwrap().resets_at.unwrap().timestamp(),
            2_000_000_000
        );
        assert_eq!(snap.secondary.unwrap().used_percent, Some(100.0));
    }

    #[test]
    fn a_response_without_a_customer_object_is_a_parse_error() {
        assert!(parse_jsonl(r#"{"json":[[0,0,0,[0]]]}"#).is_err());
        assert!(parse_jsonl("not json at all").is_err());
    }

    #[test]
    fn input_is_percent_encoded() {
        let encoded = urlencode(INPUT);
        assert!(!encoded.contains('{'));
        assert!(encoded.starts_with("%7B%220%22%3A"));
    }
}
