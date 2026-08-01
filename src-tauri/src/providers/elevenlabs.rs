//! Ported from Sources/CodexBarCore/Providers/ElevenLabs/ElevenLabsUsageFetcher.swift.
//! `GET https://api.elevenlabs.io/v1/user/subscription`, authenticated with `xi-api-key`.

use async_trait::async_trait;
use serde::Deserialize;

use super::api_token::{api_key, epoch_to_utc, get_json, has_api_key, Auth};
use super::util::percent;
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow};
use crate::config::Config;

const SUBSCRIPTION_URL: &str = "https://api.elevenlabs.io/v1/user/subscription";

pub struct ElevenLabs;

#[derive(Debug, Deserialize)]
struct Subscription {
    tier: Option<String>,
    status: Option<String>,
    #[serde(default)]
    character_count: i64,
    #[serde(default)]
    character_limit: i64,
    voice_slots_used: Option<i64>,
    voice_limit: Option<i64>,
    professional_voice_slots_used: Option<i64>,
    professional_voice_limit: Option<i64>,
    next_character_count_reset_unix: Option<i64>,
}

fn slot_window(label: &str, used: Option<i64>, limit: Option<i64>) -> Option<UsageWindow> {
    let (used, limit) = (used?, limit?);
    if limit <= 0 {
        return None;
    }
    Some(UsageWindow::new(
        label,
        Some(percent(used as f64, limit as f64)),
        None,
        None,
    ))
}

/// "creator" -> "Creator", "pro_voice" -> "Pro Voice", plus the status when it is
/// anything other than active. Falls back to the bare status.
fn plan_label(sub: &Subscription) -> Option<String> {
    let status = sub
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let tier = sub.tier.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let Some(tier) = tier else {
        return status.map(str::to_string);
    };
    let pretty = tier
        .replace('_', " ")
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    match status {
        Some(status) if !status.eq_ignore_ascii_case("active") => {
            Some(format!("{pretty} ({status})"))
        }
        _ => Some(pretty),
    }
}

fn to_snapshot(sub: &Subscription) -> UsageSnapshot {
    let mut snapshot = UsageSnapshot::new("elevenlabs");
    if sub.character_limit > 0 {
        snapshot.primary = Some(UsageWindow::new(
            "Credits",
            Some(percent(
                sub.character_count as f64,
                sub.character_limit as f64,
            )),
            sub.next_character_count_reset_unix.and_then(epoch_to_utc),
            None,
        ));
    }
    snapshot.secondary = slot_window("Voice slots", sub.voice_slots_used, sub.voice_limit);
    snapshot.tertiary = slot_window(
        "Pro voices",
        sub.professional_voice_slots_used,
        sub.professional_voice_limit,
    );
    snapshot.credits = Some((sub.character_limit - sub.character_count).max(0) as f64);
    snapshot.plan = plan_label(sub);
    snapshot
}

#[async_trait]
impl Provider for ElevenLabs {
    fn id(&self) -> &'static str {
        "elevenlabs"
    }

    fn name(&self) -> &'static str {
        "ElevenLabs"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn doc_url(&self) -> &'static str {
        "https://elevenlabs.io/app/settings/api-keys"
    }

    fn env_key(&self) -> Option<&'static str> {
        Some("ELEVENLABS_API_KEY")
    }

    fn is_configured(&self, config: &Config) -> bool {
        has_api_key(config, self)
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let key = api_key(&ctx.config, self)?;
        let sub: Subscription = get_json(
            &ctx.http,
            SUBSCRIPTION_URL,
            &Auth::Header("xi-api-key", &key),
            &[],
        )
        .await?;
        Ok(to_snapshot(&sub))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Payload from Tests/CodexBarTests/ElevenLabsUsageFetcherTests.swift.
    const SAMPLE: &str = r#"{
      "tier": "creator",
      "character_count": 25000,
      "character_limit": 100000,
      "voice_slots_used": 2,
      "voice_limit": 10,
      "professional_voice_slots_used": 1,
      "professional_voice_limit": 2,
      "current_overage": {"amount": "0", "currency": "usd"},
      "status": "active",
      "next_character_count_reset_unix": 2000000000
    }"#;

    #[test]
    fn parses_subscription_into_windows() {
        let sub: Subscription = serde_json::from_str(SAMPLE).unwrap();
        let snapshot = to_snapshot(&sub);
        let primary = snapshot.primary.unwrap();
        assert_eq!(primary.used_percent, Some(25.0));
        assert_eq!(primary.resets_at.unwrap().timestamp(), 2_000_000_000);
        assert_eq!(snapshot.secondary.unwrap().used_percent, Some(20.0));
        assert_eq!(snapshot.tertiary.unwrap().used_percent, Some(50.0));
        assert_eq!(snapshot.credits, Some(75_000.0));
        assert_eq!(snapshot.plan.as_deref(), Some("Creator"));
    }

    #[test]
    fn minimal_payload_keeps_only_the_credit_window() {
        let sub: Subscription = serde_json::from_str(
            r#"{"tier":"starter","character_count":1000,"character_limit":10000,"status":"past_due"}"#,
        )
        .unwrap();
        let snapshot = to_snapshot(&sub);
        assert_eq!(snapshot.primary.unwrap().used_percent, Some(10.0));
        assert!(snapshot.secondary.is_none());
        assert_eq!(snapshot.plan.as_deref(), Some("Starter (past_due)"));
    }

    #[test]
    fn overage_does_not_push_percent_past_full() {
        let sub: Subscription =
            serde_json::from_str(r#"{"character_count":150,"character_limit":100}"#).unwrap();
        let snapshot = to_snapshot(&sub);
        assert_eq!(snapshot.primary.unwrap().used_percent, Some(100.0));
        assert_eq!(snapshot.credits, Some(0.0));
    }
}
