//! Claude provider.
//!
//! Ported from CodexBar `Providers/Claude/ClaudeOAuth/*.swift` and `ClaudeConfigPaths.swift`:
//! credentials from `.credentials.json`, refresh against platform.claude.com, usage and
//! profile from the Anthropic OAuth endpoints.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::codex::RefreshFloor;
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow};
use crate::config::Config;

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const TOKEN_ENDPOINT: &str = "https://platform.claude.com/v1/oauth/token";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const BETA_HEADER: &str = "oauth-2025-04-20";
const USER_AGENT: &str = "claude-code/2.1.0";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Row 11. A `.credentials.json` with no `expiresAt` reads as expired forever, which
/// without this gate rotates the refresh token on every scheduler tick. See
/// [`super::codex::REFRESH_FLOOR`] for the interval.
static REFRESH_GATE: RefreshFloor = RefreshFloor::new();

/// Row 10. Anthropic rotates the refresh token on every refresh, so the token we failed
/// to write is the only live one: dropping it silently is what forced CodexBar users
/// back through `claude login` (their issues #1161 and #1239).
fn persist_failed(e: std::io::Error) -> ProviderError {
    ProviderError::Auth(format!(
        "the refreshed Claude token could not be written to .credentials.json ({e}); \
         close any running Claude CLI and retry, then run claude login if it persists"
    ))
}

/// Row 11 gate. Only a credentials file with NO `expiresAt` is throttled by the shared
/// floor: that one reads as expired forever. A stated expiry that has passed is a fact,
/// and gating it would let a single dropped connection cost 15 minutes of blank tile.
fn may_refresh(expires_at: Option<DateTime<Utc>>, gate: &RefreshFloor) -> bool {
    expires_at.is_some() || gate.claim()
}

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
            // Row 11's floor exists for a file with NO expiresAt, which reads as expired
            // on every tick and would otherwise rotate the token 288 times a day. A real
            // expiry that has passed is a fact, not a guess, so it refreshes every tick.
            //
            // The gate books the ATTEMPT, so gating a known expiry would spend the floor
            // on one dropped connection: the next 15 minutes of ticks would then skip the
            // refresh and send a token the file itself calls dead, which 401s into an
            // `Auth` error, and scheduler::merge clears every window on `Auth`. One lost
            // packet would blank the tile for 15 minutes and tell the user to run
            // `claude login`. Letting a transport failure surface as `Http` keeps the last
            // good numbers and retries on the next tick instead.
            if may_refresh(creds.expires_at, &REFRESH_GATE) {
                creds.refresh(&ctx.http).await?;
                creds.save().map_err(persist_failed)?;
            }
        }

        let usage = fetch_usage(&ctx.http, &creds.access_token).await?;
        let mut snap = map_usage(&usage);
        snap.plan = creds.plan();
        // The profile call is decoration: a failure must not lose the usage numbers.
        match fetch_profile(&ctx.http, &creds.access_token).await {
            // Row 35. The email is the only identity this API hands us, so it is both the
            // display string and the history key. It is not sent anywhere new: `account`
            // already carried it.
            Ok(email) => {
                snap.account_key = email.clone();
                snap.account = email;
            }
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
        let retry_after = super::retry_after_of(&resp);
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
            // Only a refusal is an auth problem. A 429 or a 5xx is the token service
            // having a moment, and `Auth` would clear every window (row 2) and tell the
            // user to run `claude login` over a blank tile.
            let msg = format!("Claude token refresh failed with HTTP {}", status.as_u16());
            return Err(match status.as_u16() {
                400 | 401 | 403 => ProviderError::Auth(msg),
                429 => ProviderError::RateLimited { retry_after },
                _ => ProviderError::Http(msg),
            });
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

    fn save(&self) -> std::io::Result<()> {
        self.save_to(&credentials_path())
    }

    /// Rewrite .credentials.json preserving every other key. Temp file + rename.
    ///
    /// The file is re-read HERE rather than reused from `load()`: `self.raw` predates
    /// the token round trip, and the Claude CLI may have rewritten the file (including
    /// its own rotated token, or an mcpOAuth entry) while we were on the network.
    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let mut root = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| self.raw.clone());
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

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.agentbar-tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&root)?)?;
        // The staged file holds the tokens, so it must not outlive a failed rename.
        if let Err(e) = std::fs::rename(&tmp, path) {
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
    // Row 23. Off the response before the body consumes it: a 429 that named its own wait
    // is the only thing that can shorten or lengthen the backoff honestly.
    let retry_after = super::retry_after_of(&resp);
    let text = resp.text().await.unwrap_or_default();
    match status.as_u16() {
        200 => serde_json::from_str(&text)
            .map_err(|e| ProviderError::Parse(format!("usage response: {e}"))),
        401 => Err(ProviderError::Auth(
            "Claude OAuth token rejected, run claude login".into(),
        )),
        // A rate limit, not a generic transport failure: the scheduler honours the header,
        // the tile prints the rate limited copy, and the retry ladder leaves it alone.
        429 => Err(ProviderError::RateLimited { retry_after }),
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
        return Err(super::util::http_error(&resp, || {
            format!("Claude profile error {}", resp.status().as_u16())
        }));
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
    Some(UsageWindow::new(
        label,
        Some(used),
        raw.get("resets_at")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s.trim()).ok())
            .map(|d| d.with_timezone(&Utc)),
        Some(minutes),
    ))
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
              "five_hour": {"utilization": 37.5, "resets_at": "2033-05-18T14:30:00Z"},
              "seven_day": {"utilization": 120, "resets_at": "2033-05-20T00:00:00.000Z"},
              "seven_day_opus": {"utilization": 4}
            }"#,
        )
        .unwrap();
        let snap = map_usage(&body);
        let p = snap.primary.unwrap();
        assert_eq!(
            (p.label.as_str(), p.used_percent, p.window_minutes),
            ("5h", Some(37.5), Some(300))
        );
        assert!(p.resets_at.is_some());
        let s = snap.secondary.unwrap();
        assert_eq!(s.used_percent, Some(100.0)); // clamped
        assert!(s.resets_at.is_some()); // fractional seconds parse too
        let t = snap.tertiary.unwrap();
        assert_eq!((t.label.as_str(), t.resets_at), ("Opus weekly", None));
    }

    /// Proof that the mapper builds through `UsageWindow::new`: a reset that has
    /// already happened is dropped rather than shown as an expired countdown.
    #[test]
    fn an_elapsed_reset_does_not_survive_the_constructor() {
        let snap = map_usage(&serde_json::json!({
            "five_hour": {"utilization": 10, "resets_at": "2020-01-01T00:00:00Z"}
        }));
        assert_eq!(snap.primary.unwrap().resets_at, None);
    }

    #[test]
    fn missing_windows_stay_none() {
        let snap = map_usage(&serde_json::json!({"five_hour": {"resets_at": "bogus"}}));
        assert!(snap.primary.is_none() && snap.secondary.is_none() && snap.tertiary.is_none());
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("agentbar-claude-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn creds_with(raw: Value) -> ClaudeCredentials {
        ClaudeCredentials {
            access_token: "new-access".into(),
            refresh_token: Some("new-refresh".into()),
            expires_at: DateTime::from_timestamp_millis(1_900_000_000_000),
            subscription_type: None,
            rate_limit_tier: None,
            raw,
        }
    }

    /// Row 10. `save` reads .credentials.json again instead of replaying the copy taken
    /// before the network call, so an mcpOAuth entry the CLI wrote in between survives.
    #[test]
    fn save_rereads_the_file_and_keeps_other_keys() {
        let path = scratch("save-merge").join(".credentials.json");
        std::fs::write(
            &path,
            r#"{"mcpOAuth": {"written_after_we_loaded": true},
                "claudeAiOauth": {"accessToken": "old", "subscriptionType": "max", "scopes": ["a"]}}"#,
        )
        .unwrap();

        creds_with(serde_json::json!({"claudeAiOauth": {"accessToken": "stale"}}))
            .save_to(&path)
            .unwrap();

        let back: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            back["mcpOAuth"]["written_after_we_loaded"], true,
            "a key the CLI added during our request window was clobbered"
        );
        let oauth = &back["claudeAiOauth"];
        assert_eq!(oauth["accessToken"], "new-access");
        assert_eq!(oauth["refreshToken"], "new-refresh");
        assert_eq!(oauth["subscriptionType"], "max");
        assert_eq!(oauth["scopes"][0], "a");
        assert_eq!(oauth["expiresAt"], 1_900_000_000_000i64);
        assert!(!path.with_extension("json.agentbar-tmp").exists());
    }

    /// Row 10. A lost write fails the refresh loudly: Anthropic rotated the refresh
    /// token already, so silence here costs the user a `claude login`.
    #[test]
    fn a_failed_write_becomes_a_loud_auth_error() {
        let blocker = scratch("save-fail").join("not-a-directory");
        std::fs::write(&blocker, "").unwrap();
        let path = blocker.join(".credentials.json");

        let err = creds_with(Value::Null).save_to(&path).unwrap_err();
        match persist_failed(err) {
            ProviderError::Auth(msg) => {
                assert!(msg.contains("claude login"), "{msg}");
                assert!(
                    !msg.contains("new-access") && !msg.contains("new-refresh"),
                    "{msg}"
                );
            }
            other => panic!("a lost credential write must fail the refresh: {other}"),
        }
        assert!(!path.exists());
    }

    /// Row 11. A credentials file with no `expiresAt` reads as expired forever; the
    /// shared floor is what stops that from rotating the token every tick.
    #[test]
    fn a_missing_expiry_still_reads_as_expired_but_the_floor_gates_it() {
        let creds = ClaudeCredentials {
            expires_at: None,
            ..creds_with(Value::Null)
        };
        assert!(creds.is_expired());
        let gate = RefreshFloor::new();
        assert!(may_refresh(None, &gate));
        assert!(
            !may_refresh(None, &gate),
            "a second tick must not rotate the token again"
        );
    }

    /// The other half of row 11: a STATED expiry that has passed is not the case the
    /// floor was written for. Gating it would spend the floor on one dropped connection,
    /// and the next 15 minutes of ticks would send a token the file calls dead, which
    /// 401s into an `Auth` error that clears the tile.
    #[test]
    fn a_known_expiry_is_refreshed_every_tick_not_once_per_floor() {
        let gate = RefreshFloor::new();
        let expiry = DateTime::from_timestamp_millis(1_000_000_000_000);
        for _ in 0..3 {
            assert!(may_refresh(expiry, &gate));
        }
        // ... and doing so never books the floor away from the no-expiry case.
        assert!(may_refresh(None, &gate));
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
                            "claude {lane}: {} used {:?}% window {:?}m resets {:?}",
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
