//! Claude provider.
//!
//! Ported from CodexBar `Providers/Claude/ClaudeOAuth/*.swift` and `ClaudeConfigPaths.swift`:
//! credentials from `.credentials.json`, refresh against platform.claude.com, usage and
//! profile from the Anthropic OAuth endpoints.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow};
use crate::config::Config;

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const TOKEN_ENDPOINT: &str = "https://platform.claude.com/v1/oauth/token";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const BETA_HEADER: &str = "oauth-2025-04-20";
const USER_AGENT: &str = "claude-code/2.1.0";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Claude;

#[async_trait]
impl Provider for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn name(&self) -> &'static str {
        "Claude"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::OauthFile
    }

    fn doc_url(&self) -> &'static str {
        "https://docs.anthropic.com/en/docs/claude-code"
    }

    fn is_configured(&self, _config: &Config) -> bool {
        credentials_path().is_file()
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let mut creds = ClaudeCredentials::load()?;

        if creds.is_expired() {
            if creds.refresh_token.is_none() {
                return Err(ProviderError::Auth(
                    "Claude token expired and no refresh token, run claude login".into(),
                ));
            }
            creds.refresh(&ctx.http).await?;
            if let Err(e) = creds.save() {
                log::warn!("claude: could not persist refreshed tokens: {e}");
            }
        }

        let usage = fetch_usage(&ctx.http, &creds.access_token).await?;
        let mut snap = map_usage(&usage);
        snap.plan = creds.plan();
        // The profile call is decoration: a failure must not lose the usage numbers.
        match fetch_profile(&ctx.http, &creds.access_token).await {
            Ok(email) => snap.account = email,
            Err(e) => log::debug!("claude: profile fetch failed: {e}"),
        }
        Ok(snap)
    }
}

// MARK: credentials

fn credentials_path() -> PathBuf {
    super::util::home_subdir(".claude", Some("CLAUDE_CONFIG_DIR")).join(".credentials.json")
}

struct ClaudeCredentials {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    subscription_type: Option<String>,
    rate_limit_tier: Option<String>,
    /// Whole .credentials.json, so a rewrite keeps other keys (mcpOAuth and friends).
    raw: Value,
}

impl ClaudeCredentials {
    fn load() -> Result<Self, ProviderError> {
        let path = credentials_path();
        let text = std::fs::read_to_string(&path).map_err(|_| ProviderError::NotConfigured)?;
        let raw: Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Parse(format!(".credentials.json: {e}")))?;

        let oauth = raw.get("claudeAiOauth").ok_or_else(|| {
            ProviderError::Auth("Claude credentials hold no claudeAiOauth, run claude login".into())
        })?;
        let access_token = string_at(oauth, "accessToken")
            .ok_or_else(|| ProviderError::Auth("Claude credentials have no access token".into()))?;

        Ok(Self {
            access_token,
            refresh_token: string_at(oauth, "refreshToken"),
            // expiresAt is epoch milliseconds.
            expires_at: oauth
                .get("expiresAt")
                .and_then(Value::as_f64)
                .and_then(|ms| DateTime::from_timestamp_millis(ms as i64)),
            subscription_type: string_at(oauth, "subscriptionType"),
            rate_limit_tier: string_at(oauth, "rateLimitTier"),
            raw,
        })
    }

    fn is_expired(&self) -> bool {
        match self.expires_at {
            None => true,
            Some(t) => Utc::now() >= t,
        }
    }

    fn plan(&self) -> Option<String> {
        branded_plan(
            self.subscription_type.as_deref(),
            self.rate_limit_tier.as_deref(),
        )
    }

    async fn refresh(&mut self, http: &reqwest::Client) -> Result<(), ProviderError> {
        let refresh_token = self.refresh_token.clone().unwrap_or_default();
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", CLIENT_ID),
        ];

        let resp = http
            .post(TOKEN_ENDPOINT)
            .timeout(REQUEST_TIMEOUT)
            .header("Accept", "application/json")
            .form(&form)
            .send()
            .await
            .map_err(|e| ProviderError::Http(format!("token refresh failed: {e}")))?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let code = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string));
            if code.as_deref() == Some("invalid_grant") {
                return Err(ProviderError::Auth(
                    "Claude refresh token rejected, run claude login".into(),
                ));
            }
            return Err(ProviderError::Auth(format!(
                "Claude token refresh failed with HTTP {}",
                status.as_u16()
            )));
        }

        let json: Value = serde_json::from_str(&text)
            .map_err(|_| ProviderError::Parse("token refresh returned invalid JSON".into()))?;
        let access = json
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| ProviderError::Parse("token refresh returned no access token".into()))?;
        self.access_token = access.to_string();
        if let Some(v) = json.get("refresh_token").and_then(Value::as_str) {
            self.refresh_token = Some(v.to_string());
        }
        if let Some(secs) = json.get("expires_in").and_then(Value::as_i64) {
            self.expires_at = Some(Utc::now() + chrono::Duration::seconds(secs));
        }
        Ok(())
    }

    /// Rewrite .credentials.json preserving every other key. Temp file + rename.
    fn save(&self) -> std::io::Result<()> {
        let mut root = self.raw.clone();
        if !root.is_object() {
            root = Value::Object(serde_json::Map::new());
        }
        let oauth = root
            .as_object_mut()
            .expect("object")
            .entry("claudeAiOauth")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if !oauth.is_object() {
            *oauth = Value::Object(serde_json::Map::new());
        }
        let oauth = oauth.as_object_mut().expect("object");
        oauth.insert(
            "accessToken".into(),
            Value::String(self.access_token.clone()),
        );
        if let Some(rt) = &self.refresh_token {
            oauth.insert("refreshToken".into(), Value::String(rt.clone()));
        }
        if let Some(at) = self.expires_at {
            oauth.insert("expiresAt".into(), Value::from(at.timestamp_millis()));
        }

        let path = credentials_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.agentbar-tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&root)?)?;
        // The staged file holds the tokens, so it must not outlive a failed rename.
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(())
    }
}

// MARK: plan naming

/// `ClaudePlan.brandedLoginMethod`: subscription type first, rate limit tier second,
/// with the Max multiplier appended when the tier carries one ("max_5x" -> "Claude Max 5x").
fn branded_plan(subscription_type: Option<&str>, rate_limit_tier: Option<&str>) -> Option<String> {
    let plan = plan_word(subscription_type).or_else(|| plan_word(rate_limit_tier))?;
    let branded = format!("Claude {plan}");
    if plan != "Max" {
        return Some(branded);
    }
    match max_multiplier(rate_limit_tier) {
        Some(m) => Some(format!("{branded} {m}")),
        None => Some(branded),
    }
}

fn plan_word(raw: Option<&str>) -> Option<&'static str> {
    let text = raw?.to_ascii_lowercase();
    ["max", "pro", "team", "enterprise", "ultra"]
        .into_iter()
        .find(|w| text.contains(w))
        .map(|w| match w {
            "max" => "Max",
            "pro" => "Pro",
            "team" => "Team",
            "enterprise" => "Enterprise",
            _ => "Ultra",
        })
}

fn max_multiplier(rate_limit_tier: Option<&str>) -> Option<String> {
    let text = rate_limit_tier?.to_ascii_lowercase();
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    let idx = words.iter().position(|w| *w == "max")?;
    let candidate = words.get(idx + 1)?;
    let digits = candidate.strip_suffix('x')?;
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
        .then(|| candidate.to_string())
}

// MARK: usage

async fn fetch_usage(http: &reqwest::Client, token: &str) -> Result<Value, ProviderError> {
    let resp = http
        .get(USAGE_URL)
        .timeout(REQUEST_TIMEOUT)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("anthropic-beta", BETA_HEADER)
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|e| ProviderError::Http(e.to_string()))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    match status.as_u16() {
        200 => serde_json::from_str(&text)
            .map_err(|e| ProviderError::Parse(format!("usage response: {e}"))),
        401 => Err(ProviderError::Auth(
            "Claude OAuth token rejected, run claude login".into(),
        )),
        429 => Err(ProviderError::Http(
            "Claude usage endpoint is rate limited, try again in a few minutes".into(),
        )),
        code => Err(ProviderError::Http(format!("Claude API error {code}"))),
    }
}

async fn fetch_profile(
    http: &reqwest::Client,
    token: &str,
) -> Result<Option<String>, ProviderError> {
    let resp = http
        .get(PROFILE_URL)
        .timeout(Duration::from_secs(15))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .send()
        .await
        .map_err(|e| ProviderError::Http(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ProviderError::Http(format!(
            "Claude profile error {}",
            resp.status().as_u16()
        )));
    }
    let json: Value = resp
        .json()
        .await
        .map_err(|e| ProviderError::Parse(e.to_string()))?;
    Ok(profile_email(&json))
}

fn profile_email(json: &Value) -> Option<String> {
    let from = |v: &Value| {
        ["emailAddress", "email_address", "email"]
            .iter()
            .find_map(|k| string_at(v, k))
    };
    json.get("account").and_then(from).or_else(|| from(json))
}

fn map_usage(body: &Value) -> UsageSnapshot {
    let mut snap = UsageSnapshot::new("claude");
    snap.primary = window(body, "five_hour", "5h", 300);
    snap.secondary = window(body, "seven_day", "Weekly", 10080);
    snap.tertiary = window(body, "seven_day_opus", "Opus weekly", 10080);
    snap
}

/// A usage window: `utilization` is already a 0..100 percentage, `resets_at` an ISO-8601 string.
fn window(body: &Value, key: &str, label: &str, minutes: u64) -> Option<UsageWindow> {
    let raw = body.get(key)?;
    let used = raw.get("utilization").and_then(Value::as_f64)?;
    Some(UsageWindow {
        label: label.to_string(),
        used_percent: used.clamp(0.0, 100.0),
        resets_at: raw
            .get("resets_at")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s.trim()).ok())
            .map(|d| d.with_timezone(&Utc)),
        window_minutes: Some(minutes),
    })
}

fn string_at(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_five_hour_seven_day_and_opus() {
        let body: Value = serde_json::from_str(
            r#"{
              "five_hour": {"utilization": 37.5, "resets_at": "2026-08-01T14:30:00Z"},
              "seven_day": {"utilization": 120, "resets_at": "2026-08-05T00:00:00.000Z"},
              "seven_day_opus": {"utilization": 4}
            }"#,
        )
        .unwrap();
        let snap = map_usage(&body);
        let p = snap.primary.unwrap();
        assert_eq!(
            (p.label.as_str(), p.used_percent, p.window_minutes),
            ("5h", 37.5, Some(300))
        );
        assert!(p.resets_at.is_some());
        let s = snap.secondary.unwrap();
        assert_eq!(s.used_percent, 100.0); // clamped
        assert!(s.resets_at.is_some()); // fractional seconds parse too
        let t = snap.tertiary.unwrap();
        assert_eq!((t.label.as_str(), t.resets_at), ("Opus weekly", None));
    }

    #[test]
    fn missing_windows_stay_none() {
        let snap = map_usage(&serde_json::json!({"five_hour": {"resets_at": "bogus"}}));
        assert!(snap.primary.is_none() && snap.secondary.is_none() && snap.tertiary.is_none());
    }

    #[test]
    fn plan_naming() {
        assert_eq!(
            branded_plan(Some("max"), Some("max_5x")).as_deref(),
            Some("Claude Max 5x")
        );
        assert_eq!(
            branded_plan(None, Some("claude_pro")).as_deref(),
            Some("Claude Pro")
        );
        assert_eq!(
            branded_plan(Some("max"), None).as_deref(),
            Some("Claude Max")
        );
        assert_eq!(branded_plan(None, Some("something")), None);
    }

    #[test]
    fn profile_email_reads_nested_and_flat() {
        assert_eq!(
            profile_email(&serde_json::json!({"account": {"email_address": "a@b.c"}})).as_deref(),
            Some("a@b.c")
        );
        assert_eq!(
            profile_email(&serde_json::json!({"emailAddress": "d@e.f"})).as_deref(),
            Some("d@e.f")
        );
    }

    /// Real-credential smoke test. Run with:
    /// cargo test -p agentbar claude_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs real ~/.claude/.credentials.json"]
    async fn claude_live_smoke() {
        let ctx = FetchContext {
            http: reqwest::Client::new(),
            config: Config::default(),
        };
        match Claude.fetch(&ctx).await {
            Ok(s) => {
                for (lane, w) in [
                    ("primary", &s.primary),
                    ("secondary", &s.secondary),
                    ("tertiary", &s.tertiary),
                ] {
                    if let Some(w) = w {
                        println!(
                            "claude {lane}: {} used {:.1}% window {:?}m resets {:?}",
                            w.label, w.used_percent, w.window_minutes, w.resets_at
                        );
                    }
                }
                println!(
                    "claude plan={:?} account_present={}",
                    s.plan,
                    s.account.is_some()
                );
            }
            Err(e) => println!("claude fetch failed: {e}"),
        }
    }
}
