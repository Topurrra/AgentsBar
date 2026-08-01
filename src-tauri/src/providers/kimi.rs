//! Ported from Sources/CodexBarCore/Providers/Kimi/KimiUsageFetcher.swift, Code API
//! path (`GET https://api.kimi.com/coding/v1/usages` with the coding API key).
//! The kimi.com console path in the Swift app needs a browser `kimi-auth` session,
//! so it is out of scope here.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use super::api_token::{api_key, get_json, has_api_key, loose_f64, parse_rfc3339, Auth};
use super::util::percent;
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow};
use crate::config::Config;

const DEFAULT_BASE: &str = "https://api.kimi.com";
const WEEKLY_WINDOW_MINUTES: u64 = 7 * 24 * 60;

pub struct Kimi;

#[derive(Debug, Deserialize)]
struct UsageResponse {
    usage: Detail,
    #[serde(default)]
    limits: Option<Vec<RateLimit>>,
}

#[derive(Debug, Deserialize)]
struct RateLimit {
    window: Window,
    detail: Detail,
}

#[derive(Debug, Deserialize)]
struct Window {
    #[serde(default)]
    duration: i64,
    #[serde(rename = "timeUnit", default)]
    time_unit: String,
}

impl Window {
    fn minutes(&self) -> Option<u64> {
        if self.duration <= 0 {
            return None;
        }
        let multiplier = match self.time_unit.as_str() {
            "TIME_UNIT_MINUTE" => 1,
            "TIME_UNIT_HOUR" => 60,
            "TIME_UNIT_DAY" => 24 * 60,
            _ => return None,
        };
        Some(self.duration as u64 * multiplier)
    }
}

/// Counters arrive quoted on some builds and bare on others.
#[derive(Debug, Deserialize)]
struct Detail {
    limit: Value,
    used: Option<Value>,
    remaining: Option<Value>,
    #[serde(
        rename = "resetTime",
        alias = "resetAt",
        alias = "reset_time",
        alias = "reset_at"
    )]
    reset_time: Option<String>,
}

/// used, limit, reliable. `used` is authoritative and may exceed the limit during
/// overage; `remaining` only counts when it describes a valid balance. A limit with
/// no usable counter still gauges at 0% but must not claim a window duration.
fn counts(detail: &Detail) -> Option<(f64, f64, bool)> {
    let limit = loose_f64(Some(&detail.limit)).filter(|l| *l > 0.0)?;
    if let Some(used) = loose_f64(detail.used.as_ref()).filter(|u| *u >= 0.0) {
        return Some((used, limit, true));
    }
    if let Some(remaining) =
        loose_f64(detail.remaining.as_ref()).filter(|r| *r >= 0.0 && *r <= limit)
    {
        return Some((limit - remaining, limit, true));
    }
    Some((0.0, limit, false))
}

fn window(detail: &Detail, label: &str, minutes: Option<u64>) -> Option<UsageWindow> {
    let (used, limit, reliable) = counts(detail)?;
    Some(UsageWindow::new(
        label,
        Some(percent(used, limit)),
        detail.reset_time.as_deref().and_then(parse_rfc3339),
        if reliable { minutes } else { None },
    ))
}

fn rate_label(minutes: Option<u64>) -> String {
    match minutes {
        Some(m) if m % 60 == 0 => format!("{}h", m / 60),
        Some(m) => format!("{m}m"),
        None => "Session".to_string(),
    }
}

fn to_snapshot(response: &UsageResponse) -> UsageSnapshot {
    let mut snapshot = UsageSnapshot::new("kimi");
    snapshot.primary = window(&response.usage, "Weekly", Some(WEEKLY_WINDOW_MINUTES));
    if let Some(rate) = response.limits.as_ref().and_then(|l| l.first()) {
        let minutes = rate.window.minutes();
        snapshot.secondary = window(&rate.detail, &rate_label(minutes), minutes);
    }
    snapshot
}

fn base_url() -> String {
    std::env::var("KIMI_CODE_BASE_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| v.starts_with("https://"))
        .unwrap_or_else(|| DEFAULT_BASE.to_string())
}

#[async_trait]
impl Provider for Kimi {
    fn id(&self) -> &'static str {
        "kimi"
    }

    fn name(&self) -> &'static str {
        "Kimi"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::Token
    }

    fn doc_url(&self) -> &'static str {
        "https://platform.moonshot.ai"
    }

    fn env_key(&self) -> Option<&'static str> {
        Some("KIMI_CODE_API_KEY")
    }

    fn is_configured(&self, config: &Config) -> bool {
        has_api_key(config, self)
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let key = api_key(&ctx.config, self)?;
        let url = format!("{}/coding/v1/usages", base_url());
        let response: UsageResponse = get_json(&ctx.http, &url, &Auth::Bearer(&key), &[]).await?;
        Ok(to_snapshot(&response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Payload from Tests/CodexBarTests/KimiProviderTests.swift.
    const SAMPLE: &str = r#"{
      "usage": {
        "limit": "2048",
        "used": "375",
        "remaining": "1673",
        "resetTime": "2033-01-09T15:23:13.373329235Z"
      },
      "limits": [
        {
          "window": {"duration": 300, "timeUnit": "TIME_UNIT_MINUTE"},
          "detail": {
            "limit": "200",
            "used": "19",
            "remaining": "181",
            "resetTime": "2033-01-06T15:05:24.374187075Z"
          }
        }
      ]
    }"#;

    #[test]
    fn parses_weekly_and_rate_windows() {
        let parsed: UsageResponse = serde_json::from_str(SAMPLE).unwrap();
        let snapshot = to_snapshot(&parsed);
        let primary = snapshot.primary.unwrap();
        assert!((primary.used_percent.unwrap() - 18.310_546_875).abs() < 0.001);
        assert_eq!(primary.window_minutes, Some(WEEKLY_WINDOW_MINUTES));
        assert!(primary.resets_at.is_some());

        let secondary = snapshot.secondary.unwrap();
        assert!((secondary.used_percent.unwrap() - 9.5).abs() < 0.001);
        assert_eq!(secondary.window_minutes, Some(300));
        assert_eq!(secondary.label, "5h");
    }

    #[test]
    fn missing_limits_leave_only_the_weekly_window() {
        let parsed: UsageResponse =
            serde_json::from_str(r#"{"usage":{"limit":2048,"used":512},"limits":null}"#).unwrap();
        let snapshot = to_snapshot(&parsed);
        assert_eq!(snapshot.primary.unwrap().used_percent, Some(25.0));
        assert!(snapshot.secondary.is_none());
    }

    #[test]
    fn unusable_counters_gauge_at_zero_without_a_window() {
        let parsed: UsageResponse =
            serde_json::from_str(r#"{"usage":{"limit":"100","remaining":"250"}}"#).unwrap();
        let primary = to_snapshot(&parsed).primary.unwrap();
        assert_eq!(primary.used_percent, Some(0.0));
        assert_eq!(primary.window_minutes, None);
    }
}
