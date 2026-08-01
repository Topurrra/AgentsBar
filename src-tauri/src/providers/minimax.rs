//! Ported from Sources/CodexBarCore/Providers/MiniMax/MiniMaxUsageFetcher.swift,
//! API-token path. Tries the token-plan endpoint first and the legacy coding-plan
//! endpoint second, on `api.minimax.io` and then (only when the global host rejects
//! the token) `api.minimaxi.com`, which is what the Swift app does for accounts that
//! predate the region split.

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::Value;

use super::api_token::{api_key, epoch_to_utc, get_json, loose_f64, Auth};
use super::util::percent;
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow};
use crate::config::Config;

const HOSTS: [&str; 2] = ["https://api.minimax.io", "https://api.minimaxi.com"];
const PATHS: [&str; 2] = [
    "v1/token_plan/remains",
    "v1/api/openplatform/coding_plan/remains",
];

pub struct Minimax;

#[derive(Debug, Deserialize)]
struct Envelope {
    base_resp: Option<BaseResp>,
    #[serde(flatten)]
    rest: Value,
}

#[derive(Debug, Deserialize)]
struct BaseResp {
    status_code: Option<i64>,
    status_msg: Option<String>,
}

fn num(item: &Value, key: &str) -> Option<f64> {
    loose_f64(item.get(key))
}

/// One quota lane of a `model_remains` entry. `*_usage_count` is REMAINING quota,
/// not spend, which is why used is derived as total minus it.
struct Lane {
    total: &'static str,
    remaining: &'static str,
    remaining_percent: &'static str,
    start: &'static str,
    end: &'static str,
    remains_time: &'static str,
}

const INTERVAL: Lane = Lane {
    total: "current_interval_total_count",
    remaining: "current_interval_usage_count",
    remaining_percent: "current_interval_remaining_percent",
    start: "start_time",
    end: "end_time",
    remains_time: "remains_time",
};

const WEEKLY: Lane = Lane {
    total: "current_weekly_total_count",
    remaining: "current_weekly_usage_count",
    remaining_percent: "current_weekly_remaining_percent",
    start: "weekly_start_time",
    end: "weekly_end_time",
    remains_time: "weekly_remains_time",
};

fn lane_window(item: &Value, lane: &Lane, now: DateTime<Utc>) -> Option<UsageWindow> {
    let used_percent = match num(item, lane.remaining_percent) {
        Some(remaining_percent) => 100.0 - remaining_percent,
        None => {
            let total = num(item, lane.total).filter(|t| *t > 0.0)?;
            let remaining = num(item, lane.remaining)?;
            percent((total - remaining).max(0.0), total)
        }
    };

    let start = num(item, lane.start).and_then(|v| epoch_to_utc(v as i64));
    let end = num(item, lane.end).and_then(|v| epoch_to_utc(v as i64));
    let window_minutes = match (start, end) {
        (Some(start), Some(end)) => {
            let minutes = (end - start).num_minutes();
            (minutes > 0).then_some(minutes as u64)
        }
        _ => None,
    };

    // The window end is authoritative while it is still in the future; a stale end
    // falls back to the remaining-time countdown.
    let resets_at = match end {
        Some(end) if end > now => Some(end),
        _ => num(item, lane.remains_time)
            .filter(|r| *r > 0.0)
            .map(|raw| {
                let seconds = if raw > 1_000_000.0 { raw / 1000.0 } else { raw };
                now + Duration::seconds(seconds as i64)
            }),
    };

    Some(UsageWindow::at(
        window_label(lane, window_minutes),
        Some(used_percent),
        resets_at,
        window_minutes,
        now,
    ))
}

fn window_label(lane: &Lane, minutes: Option<u64>) -> String {
    if lane.total == WEEKLY.total {
        return "Weekly".to_string();
    }
    match minutes {
        Some(m) if m % (24 * 60) == 0 => format!("{}d", m / (24 * 60)),
        Some(m) if m % 60 == 0 => format!("{}h", m / 60),
        Some(m) => format!("{m}m"),
        None => "Session".to_string(),
    }
}

fn plan_name(root: &Value) -> Option<String> {
    [
        "current_subscribe_title",
        "plan_name",
        "combo_title",
        "current_plan_title",
    ]
    .iter()
    .filter_map(|key| root.get(*key).and_then(Value::as_str))
    .map(str::trim)
    .find(|name| !name.is_empty())
    .map(str::to_string)
    .or_else(|| {
        root.get("current_combo_card")
            .and_then(|c| c.get("title"))
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn credits(root: &Value) -> Option<f64> {
    [
        "points_balance",
        "point_balance",
        "credits_balance",
        "credit_balance",
        "balance",
    ]
    .iter()
    .find_map(|key| loose_f64(root.get(*key)))
    .filter(|v| *v >= 0.0)
}

fn to_snapshot(body: Value, now: DateTime<Utc>) -> Result<UsageSnapshot, ProviderError> {
    let envelope: Envelope =
        serde_json::from_value(body).map_err(|e| ProviderError::Parse(e.to_string()))?;
    // Some responses nest everything under `data`, some do not.
    let root = envelope
        .rest
        .get("data")
        .filter(|d| d.is_object())
        .unwrap_or(&envelope.rest);

    let base = root
        .get("base_resp")
        .and_then(|b| serde_json::from_value::<BaseResp>(b.clone()).ok())
        .or(envelope.base_resp);
    if let Some(status) = base.as_ref().and_then(|b| b.status_code) {
        if status != 0 {
            let message = base
                .and_then(|b| b.status_msg)
                .unwrap_or_else(|| format!("status_code {status}"));
            let lower = message.to_lowercase();
            if status == 1004
                || lower.contains("cookie")
                || lower.contains("log in")
                || lower.contains("login")
                || lower.contains("invalid api key")
            {
                return Err(ProviderError::Auth(message));
            }
            return Err(ProviderError::Http(message));
        }
    }

    let first = root
        .get("model_remains")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or_else(|| ProviderError::Parse("missing coding plan data".into()))?;

    let mut snapshot = UsageSnapshot::new("minimax");
    snapshot.primary = lane_window(first, &INTERVAL, now);
    snapshot.secondary = lane_window(first, &WEEKLY, now);
    snapshot.plan = plan_name(root);
    snapshot.credits = credits(root);
    Ok(snapshot)
}

#[async_trait]
impl Provider for Minimax {
    fn id(&self) -> &'static str {
        "minimax"
    }

    fn name(&self) -> &'static str {
        "MiniMax"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn doc_url(&self) -> &'static str {
        "https://www.minimax.io/platform"
    }

    fn is_configured(&self, config: &Config) -> bool {
        config.api_key(self.id()).is_some()
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let key = api_key(&ctx.config, self.id())?;
        let auth = Auth::Bearer(key);
        let headers = [("MM-API-Source", "AgentBar")];
        let mut last = ProviderError::NotConfigured;

        for host in HOSTS {
            for path in PATHS {
                let url = format!("{host}/{path}");
                match get_json::<Value>(&ctx.http, &url, &auth, &headers).await {
                    Ok(body) => match to_snapshot(body, Utc::now()) {
                        Ok(snapshot) => return Ok(snapshot),
                        Err(e) => last = e,
                    },
                    Err(e) => last = e,
                }
            }
            // Only a rejected token justifies asking the other region's host.
            if !matches!(last, ProviderError::Auth(_)) {
                break;
            }
        }
        Err(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Payload from Tests/CodexBarTests/MiniMaxAPITokenFetchTests.swift:
    // start 1700000000000, end start + 5h, remaining 250 of 1000.
    const SAMPLE: &str = r#"{
      "base_resp": { "status_code": 0 },
      "current_subscribe_title": "Max",
      "model_remains": [
        {
          "current_interval_total_count": 1000,
          "current_interval_usage_count": 250,
          "start_time": 1700000000000,
          "end_time": 1700018000000,
          "remains_time": 240000
        }
      ]
    }"#;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).unwrap()
    }

    fn parse(body: &str, now: i64) -> Result<UsageSnapshot, ProviderError> {
        to_snapshot(serde_json::from_str(body).unwrap(), at(now))
    }

    #[test]
    fn maps_the_interval_lane() {
        let snapshot = parse(SAMPLE, 1_700_000_000).unwrap();
        let primary = snapshot.primary.unwrap();
        // usage_count is remaining quota, so 250 left of 1000 is 75% used.
        assert_eq!(primary.used_percent, Some(75.0));
        assert_eq!(primary.window_minutes, Some(300));
        assert_eq!(primary.label, "5h");
        assert_eq!(primary.resets_at.unwrap().timestamp(), 1_700_018_000);
        assert!(snapshot.secondary.is_none());
        assert_eq!(snapshot.plan.as_deref(), Some("Max"));
    }

    #[test]
    fn remaining_percent_wins_over_the_counters() {
        let body = r#"{"data":{"current_subscribe_title":"Pro","points_balance":"42.5",
          "model_remains":[{"current_interval_remaining_percent":30,
          "current_weekly_total_count":100,"current_weekly_usage_count":40}]}}"#;
        let snapshot = parse(body, 1_700_000_000).unwrap();
        assert_eq!(snapshot.primary.unwrap().used_percent, Some(70.0));
        let weekly = snapshot.secondary.unwrap();
        assert_eq!(weekly.used_percent, Some(60.0));
        assert_eq!(weekly.label, "Weekly");
        assert_eq!(snapshot.credits, Some(42.5));
    }

    #[test]
    fn rejected_token_envelope_is_an_auth_error() {
        let body = r#"{"base_resp":{"status_code":1004,"status_msg":"invalid api key"}}"#;
        assert!(matches!(
            parse(body, 1_700_000_000),
            Err(ProviderError::Auth(_))
        ));
    }

    #[test]
    fn stale_window_end_falls_back_to_the_countdown() {
        let snapshot = parse(SAMPLE, 1_700_100_000).unwrap();
        // remains_time under a million counts as seconds (Swift uses the same cutoff).
        assert_eq!(
            snapshot.primary.unwrap().resets_at.unwrap().timestamp(),
            1_700_340_000
        );
    }
}
