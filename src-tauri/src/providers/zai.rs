//! Ported from Sources/CodexBarCore/Providers/Zai/ZaiUsageStats.swift (quota limit
//! endpoint). Region: global `api.z.ai` by default. BigModel CN users point
//! `Z_AI_API_HOST` at `https://open.bigmodel.cn`, or `Z_AI_QUOTA_URL` at a full URL,
//! the same overrides the Swift app reads.

use async_trait::async_trait;
use serde::Deserialize;

use super::api_token::{api_key, epoch_to_utc, get_json, has_api_key, Auth};
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow};
use crate::config::Config;

const QUOTA_PATH: &str = "api/monitor/usage/quota/limit";
const GLOBAL_HOST: &str = "https://api.z.ai";

pub struct Zai;

#[derive(Debug, Deserialize)]
struct QuotaResponse {
    #[serde(default)]
    code: i64,
    msg: Option<String>,
    data: Option<QuotaData>,
    #[serde(default)]
    success: bool,
}

#[derive(Debug, Deserialize)]
struct QuotaData {
    #[serde(default)]
    limits: Vec<LimitRaw>,
    #[serde(alias = "plan", alias = "plan_type", alias = "packageName")]
    #[serde(rename = "planName")]
    plan_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LimitRaw {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    unit: i64,
    #[serde(default)]
    number: i64,
    /// The quota ceiling, despite the name.
    usage: Option<i64>,
    #[serde(rename = "currentValue")]
    current_value: Option<i64>,
    remaining: Option<i64>,
    #[serde(default)]
    percentage: f64,
    #[serde(rename = "nextResetTime")]
    next_reset_time: Option<i64>,
}

impl LimitRaw {
    fn is_tokens(&self) -> bool {
        self.kind == "TOKENS_LIMIT"
    }

    fn window_minutes(&self) -> Option<u64> {
        if self.number <= 0 {
            return None;
        }
        let multiplier = match self.unit {
            5 => 1,           // minutes
            3 => 60,          // hours
            1 => 24 * 60,     // days
            6 => 7 * 24 * 60, // weeks
            _ => return None,
        };
        Some(self.number as u64 * multiplier)
    }

    /// z.ai sometimes omits the quota counters, so a computed percent is preferred
    /// but the raw `percentage` (which can fall outside 0...100) is the fallback.
    fn used_percent(&self) -> f64 {
        self.computed_used_percent().unwrap_or(self.percentage)
    }

    fn computed_used_percent(&self) -> Option<f64> {
        let limit = self.usage.filter(|l| *l > 0)?;
        let used_raw = match (self.remaining, self.current_value) {
            (Some(remaining), Some(current)) => Some((limit - remaining).max(current)),
            (Some(remaining), None) => Some(limit - remaining),
            (None, Some(current)) => Some(current),
            (None, None) => None,
        }?;
        let used = used_raw.clamp(0, limit);
        Some(super::util::percent(used as f64, limit as f64))
    }

    fn label(&self) -> String {
        if !self.is_tokens() {
            return "Monthly".to_string();
        }
        let suffix = match self.unit {
            5 => "m",
            3 => "h",
            1 => "d",
            6 => "w",
            _ => return "Tokens".to_string(),
        };
        if self.number <= 0 {
            return "Tokens".to_string();
        }
        format!("{}{suffix}", self.number)
    }

    fn to_window(&self) -> UsageWindow {
        UsageWindow::new(
            self.label(),
            Some(self.used_percent()),
            self.next_reset_time.and_then(epoch_to_utc),
            // A TIME_LIMIT entry counts calls per calendar month; only token
            // windows carry a real duration.
            if self.is_tokens() {
                self.window_minutes()
            } else {
                None
            },
        )
    }
}

fn to_snapshot(response: QuotaResponse) -> Result<UsageSnapshot, ProviderError> {
    if !response.success || response.code != 200 {
        let message = response
            .msg
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| format!("z.ai quota API returned code {}", response.code));
        return Err(ProviderError::Http(message));
    }
    let data = response
        .data
        .ok_or_else(|| ProviderError::Parse("missing data".into()))?;

    let mut tokens: Vec<&LimitRaw> = data.limits.iter().filter(|l| l.is_tokens()).collect();
    let time_limit = data.limits.iter().find(|l| l.kind == "TIME_LIMIT");

    // Two token windows means shortest is the session gauge, longest the plan quota.
    tokens.sort_by_key(|l| l.window_minutes().unwrap_or(u64::MAX));
    let (token_limit, session_limit) = match tokens.len() {
        0 => (None, None),
        1 => (Some(tokens[0]), None),
        _ => (tokens.last().copied(), Some(tokens[0])),
    };

    let mut snapshot = UsageSnapshot::new("zai");
    snapshot.primary = token_limit.or(time_limit).map(LimitRaw::to_window);
    snapshot.secondary = match (token_limit, time_limit) {
        (Some(_), Some(time)) => Some(time.to_window()),
        _ => None,
    };
    snapshot.tertiary = session_limit.map(LimitRaw::to_window);
    snapshot.plan = data
        .plan_name
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    Ok(snapshot)
}

fn quota_url() -> String {
    if let Some(url) = std::env::var("Z_AI_QUOTA_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| v.starts_with("https://"))
    {
        return url;
    }
    let host = std::env::var("Z_AI_API_HOST")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| v.starts_with("https://"))
        .unwrap_or_else(|| GLOBAL_HOST.to_string());
    format!("{host}/{QUOTA_PATH}")
}

#[async_trait]
impl Provider for Zai {
    fn id(&self) -> &'static str {
        "zai"
    }

    fn name(&self) -> &'static str {
        "Z.ai"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn doc_url(&self) -> &'static str {
        "https://docs.z.ai"
    }

    fn env_key(&self) -> Option<&'static str> {
        Some("Z_AI_API_KEY")
    }

    fn is_configured(&self, config: &Config) -> bool {
        has_api_key(config, self)
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let key = api_key(&ctx.config, self)?;
        let response: QuotaResponse =
            get_json(&ctx.http, &quota_url(), &Auth::Bearer(&key), &[]).await?;
        to_snapshot(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Payload shape from Tests/CodexBarTests/ZaiProviderTests.swift.
    const SAMPLE: &str = r#"{
      "code": 200,
      "msg": "Operation successful",
      "data": {
        "limits": [
          {
            "type": "TIME_LIMIT",
            "unit": 5,
            "number": 1,
            "usage": 100,
            "currentValue": 102,
            "remaining": 0,
            "percentage": 100,
            "usageDetails": [{"modelCode": "search-prime", "usage": 95}]
          },
          {
            "type": "TOKENS_LIMIT",
            "unit": 3,
            "number": 5,
            "usage": 40000000,
            "currentValue": 13628365,
            "remaining": 26371635,
            "percentage": 34,
            "nextResetTime": 2000000000000
          }
        ],
        "planName": "Pro"
      },
      "success": true
    }"#;

    #[test]
    fn maps_token_and_time_limits() {
        let parsed: QuotaResponse = serde_json::from_str(SAMPLE).unwrap();
        let snapshot = to_snapshot(parsed).unwrap();

        let primary = snapshot.primary.unwrap();
        assert_eq!(primary.label, "5h");
        assert!((primary.used_percent.unwrap() - 34.070_912_5).abs() < 0.001);
        assert_eq!(primary.window_minutes, Some(300));
        assert_eq!(primary.resets_at.unwrap().timestamp(), 2_000_000_000);

        let secondary = snapshot.secondary.unwrap();
        assert_eq!(secondary.label, "Monthly");
        // currentValue over the ceiling clamps to full, never past it.
        assert_eq!(secondary.used_percent, Some(100.0));
        assert_eq!(secondary.window_minutes, None);

        assert!(snapshot.tertiary.is_none());
        assert_eq!(snapshot.plan.as_deref(), Some("Pro"));
    }

    #[test]
    fn falls_back_to_the_raw_percentage_when_counters_are_missing() {
        let parsed: QuotaResponse = serde_json::from_str(
            r#"{"code":200,"data":{"limits":[
                {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":34}
            ]},"success":true}"#,
        )
        .unwrap();
        let snapshot = to_snapshot(parsed).unwrap();
        assert_eq!(snapshot.primary.unwrap().used_percent, Some(34.0));
    }

    #[test]
    fn two_token_windows_split_into_primary_and_tertiary() {
        let parsed: QuotaResponse = serde_json::from_str(
            r#"{"code":200,"data":{"limits":[
                {"type":"TOKENS_LIMIT","unit":3,"number":5,"usage":100,"currentValue":10,"percentage":10},
                {"type":"TOKENS_LIMIT","unit":1,"number":7,"usage":100,"currentValue":40,"percentage":40}
            ]},"success":true}"#,
        )
        .unwrap();
        let snapshot = to_snapshot(parsed).unwrap();
        assert_eq!(snapshot.primary.unwrap().label, "7d");
        assert_eq!(snapshot.tertiary.unwrap().label, "5h");
    }

    #[test]
    fn unsuccessful_envelope_is_an_error() {
        let parsed: QuotaResponse =
            serde_json::from_str(r#"{"code":401,"msg":"bad token","success":false}"#).unwrap();
        assert!(to_snapshot(parsed).is_err());
    }
}
