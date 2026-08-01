//! Codex (ChatGPT) provider.
//!
//! Ported from CodexBar `Providers/Codex/CodexOAuth/*.swift`:
//! credentials from `auth.json`, 8-day refresh rule via auth.openai.com, usage from the
//! ChatGPT backend `wham/usage` endpoint.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow};
use crate::config::Config;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";
const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api";
/// CodexBar refreshes when the stored tokens are older than 8 days.
const REFRESH_AFTER_SECS: i64 = 8 * 24 * 60 * 60;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Row 11. No matter what a credential file says, one provider gets at most one token
/// rotation per this interval. A file with no `last_refresh` (hand edited, written by an
/// older CLI, written by a third party login helper) otherwise reads as "refresh now" on
/// every scheduler tick: 288 rotations a day against the auth service.
pub(super) const REFRESH_FLOOR: Duration = Duration::from_secs(15 * 60);

/// Per-provider gate for [`REFRESH_FLOOR`]. Records ATTEMPTS, not successes: a refresh
/// that fails must be throttled exactly like one that works, otherwise a broken token
/// spins the loop hardest.
///
/// ponytail: one `Mutex<Option<Instant>>` per provider static, no registry map. It lives
/// in this file because `providers/util.rs` belongs to another agent this wave; move it
/// there if a third OAuth-file provider lands.
pub(super) struct RefreshFloor(Mutex<Option<Instant>>);

impl RefreshFloor {
    pub(super) const fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// True when a refresh may run now, and books this attempt. False means one ran less
    /// than [`REFRESH_FLOOR`] ago.
    pub(super) fn claim(&self) -> bool {
        self.claim_at(Instant::now())
    }

    fn claim_at(&self, now: Instant) -> bool {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some_and(|t| now.duration_since(t) < REFRESH_FLOOR) {
            return false;
        }
        *slot = Some(now);
        true
    }
}

static REFRESH_GATE: RefreshFloor = RefreshFloor::new();

/// Row 10. The token endpoint has already invalidated the old refresh token by the time
/// `save` runs, so the copy we failed to write is the only valid one left. Continuing
/// would hand the next tick a dead token and the user a CLI re-login.
fn persist_failed(e: std::io::Error) -> ProviderError {
    ProviderError::Auth(format!(
        "the refreshed Codex token could not be written to auth.json ({e}); \
         close any running Codex CLI and retry, then run codex login if it persists"
    ))
}

pub struct Codex;

#[async_trait]
impl Provider for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn name(&self) -> &'static str {
        "Codex"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::OauthFile
    }

    fn doc_url(&self) -> &'static str {
        "https://developers.openai.com/codex"
    }

    fn is_configured(&self, _config: &Config) -> bool {
        auth_path().is_file()
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let mut auth = CodexAuth::load()?;

        // The gate is claimed before the network call, so a refresh that fails is
        // throttled too. When it denies, we go on with the token we have: the server is
        // the authority on whether it still works, and a 401 refreshes reactively.
        if auth.needs_refresh() && !auth.refresh_token.is_empty() && REFRESH_GATE.claim() {
            auth.refresh(&ctx.http).await?;
            auth.save().map_err(persist_failed)?;
        }

        let body = fetch_usage(&ctx.http, &auth).await?;
        Ok(map_usage(&body, &auth))
    }
}

// MARK: credentials

fn codex_home() -> PathBuf {
    super::util::home_subdir(".codex", Some("CODEX_HOME"))
}

fn auth_path() -> PathBuf {
    codex_home().join("auth.json")
}

struct CodexAuth {
    access_token: String,
    refresh_token: String,
    id_token: Option<String>,
    account_id: Option<String>,
    last_refresh: Option<DateTime<Utc>>,
    /// Whole auth.json, so a rewrite keeps fields we do not know about.
    raw: Value,
}

impl CodexAuth {
    fn load() -> Result<Self, ProviderError> {
        let path = auth_path();
        let text = std::fs::read_to_string(&path).map_err(|_| ProviderError::NotConfigured)?;
        let raw: Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Parse(format!("auth.json: {e}")))?;

        // An API-key login has no OAuth tokens; CodexBar prefers it when present.
        if let Some(key) = raw.get("OPENAI_API_KEY").and_then(Value::as_str) {
            if !key.trim().is_empty() {
                return Ok(Self {
                    access_token: key.trim().to_string(),
                    refresh_token: String::new(),
                    id_token: None,
                    account_id: None,
                    last_refresh: None,
                    raw,
                });
            }
        }

        let tokens = raw.get("tokens").ok_or_else(|| {
            ProviderError::Auth("auth.json has no tokens, run codex login".into())
        })?;
        let access_token = str_field(tokens, "access_token", "accessToken")
            .ok_or_else(|| ProviderError::Auth("auth.json has no access token".into()))?;

        Ok(Self {
            access_token,
            refresh_token: str_field(tokens, "refresh_token", "refreshToken").unwrap_or_default(),
            id_token: str_field(tokens, "id_token", "idToken"),
            account_id: str_field(tokens, "account_id", "accountId"),
            last_refresh: raw
                .get("last_refresh")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339),
            raw,
        })
    }

    fn needs_refresh(&self) -> bool {
        match self.last_refresh {
            None => true,
            Some(t) => (Utc::now() - t).num_seconds() > REFRESH_AFTER_SECS,
        }
    }

    /// id_token claims (no signature check, same as the Swift side).
    fn claims(&self) -> Option<Value> {
        decode_jwt_claims(self.id_token.as_deref()?)
    }

    fn account_id(&self) -> Option<String> {
        if let Some(id) = self.account_id.as_ref().filter(|s| !s.trim().is_empty()) {
            return Some(id.trim().to_string());
        }
        let claims = self.claims()?;
        claim_str(&claims, "https://api.openai.com/auth", "chatgpt_account_id")
    }

    fn email(&self) -> Option<String> {
        let claims = self.claims()?;
        claims
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                claims
                    .get("https://api.openai.com/profile")
                    .and_then(|p| p.get("email"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn plan_from_claims(&self) -> Option<String> {
        let claims = self.claims()?;
        claim_str(&claims, "https://api.openai.com/auth", "chatgpt_plan_type")
    }

    async fn refresh(&mut self, http: &reqwest::Client) -> Result<(), ProviderError> {
        let body = serde_json::json!({
            "client_id": CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": self.refresh_token,
            "scope": "openid profile email",
        });

        let resp = http
            .post(TOKEN_ENDPOINT)
            .timeout(REQUEST_TIMEOUT)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Http(format!("token refresh failed: {e}")))?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ProviderError::Auth(refresh_error_message(
                status.as_u16(),
                &text,
            )));
        }

        let json: Value = serde_json::from_str(&text)
            .map_err(|_| ProviderError::Parse("token refresh returned invalid JSON".into()))?;
        if let Some(v) = json.get("access_token").and_then(Value::as_str) {
            self.access_token = v.to_string();
        }
        if let Some(v) = json.get("refresh_token").and_then(Value::as_str) {
            self.refresh_token = v.to_string();
        }
        if let Some(v) = json.get("id_token").and_then(Value::as_str) {
            self.id_token = Some(v.to_string());
        }
        self.last_refresh = Some(Utc::now());
        Ok(())
    }

    fn save(&self) -> std::io::Result<()> {
        self.save_to(&auth_path())
    }

    /// Rewrite auth.json preserving unknown keys. Temp file + rename, like the CLI.
    ///
    /// The file is re-read HERE rather than reused from `load()`: `self.raw` is a
    /// snapshot from before the token round trip, and `codex login` may have rewritten
    /// auth.json in between. Writing the stale copy back restores a refresh token the
    /// server has already burned. Mirrors `CodexOAuthCredentials.save` in CodexBar.
    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let mut root = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| self.raw.clone());
        if !root.is_object() {
            root = Value::Object(serde_json::Map::new());
        }
        let obj = root.as_object_mut().expect("object");

        // Merge into the existing tokens object instead of replacing it: keys the CLI
        // owns in there and we do not model must survive a refresh.
        let mut tokens = match obj.get("tokens") {
            Some(Value::Object(existing)) => existing.clone(),
            _ => serde_json::Map::new(),
        };
        tokens.insert(
            "access_token".into(),
            Value::String(self.access_token.clone()),
        );
        tokens.insert(
            "refresh_token".into(),
            Value::String(self.refresh_token.clone()),
        );
        if let Some(id) = &self.id_token {
            tokens.insert("id_token".into(), Value::String(id.clone()));
        }
        if let Some(id) = &self.account_id {
            tokens.insert("account_id".into(), Value::String(id.clone()));
        }
        obj.insert("tokens".into(), Value::Object(tokens));
        obj.insert(
            "last_refresh".into(),
            Value::String(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        );

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.agentsbar-tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&root)?)?;
        // The staged file holds the tokens, so it must not outlive a failed rename.
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(())
    }
}

fn refresh_error_message(status: u16, body: &str) -> String {
    let code = serde_json::from_str::<Value>(body).ok().and_then(|v| {
        v.get("error")
            .and_then(|e| e.get("code").and_then(Value::as_str).map(str::to_string))
            .or_else(|| v.get("error").and_then(Value::as_str).map(str::to_string))
            .or_else(|| v.get("code").and_then(Value::as_str).map(str::to_string))
    });
    match code.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("refresh_token_expired") => "refresh token expired, run codex login".into(),
        Some("refresh_token_reused") => "refresh token already used, run codex login".into(),
        Some("invalid_grant") | Some("refresh_token_invalidated") => {
            "refresh token revoked, run codex login".into()
        }
        _ if status == 401 => "refresh token expired, run codex login".into(),
        _ => format!("token refresh failed with HTTP {status}"),
    }
}

// MARK: usage

async fn fetch_usage(http: &reqwest::Client, auth: &CodexAuth) -> Result<Value, ProviderError> {
    let mut req = http
        .get(usage_url())
        .timeout(REQUEST_TIMEOUT)
        .header("Authorization", format!("Bearer {}", auth.access_token))
        .header("User-Agent", "AgentsBar")
        .header("Accept", "application/json");
    if let Some(account) = auth.account_id() {
        req = req.header("ChatGPT-Account-Id", account);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| ProviderError::Http(e.to_string()))?;
    let status = resp.status();
    // Row 23. Off the response before the body consumes it, so a 429 that named its own
    // wait gets that wait instead of the flat default.
    let retry_after = super::retry_after_of(&resp);
    let text = resp.text().await.unwrap_or_default();

    match status.as_u16() {
        200..=299 => serde_json::from_str(&text)
            .map_err(|e| ProviderError::Parse(format!("usage response: {e}"))),
        401 | 403 => Err(ProviderError::Auth(
            "Codex token expired or invalid, run codex login".into(),
        )),
        // A rate limit, not a generic transport failure: the scheduler honours the header,
        // the tile prints the rate limited copy, and the retry ladder leaves it alone.
        429 => Err(ProviderError::RateLimited { retry_after }),
        code => Err(ProviderError::Http(format!("Codex API error {code}"))),
    }
}

/// `chatgpt_base_url` from config.toml wins, otherwise the public backend.
fn usage_url() -> String {
    let base = std::fs::read_to_string(codex_home().join("config.toml"))
        .ok()
        .and_then(|c| parse_base_url(&c))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let base = normalize_base_url(&base);
    let path = if base.contains("/backend-api") {
        "/wham/usage"
    } else {
        "/api/codex/usage"
    };
    format!("{base}{path}")
}

fn parse_base_url(config: &str) -> Option<String> {
    for line in config.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        // Blank lines, comments and `[section]` headers have no `=`: skip them, do not
        // stop scanning (CodexOAuthUsageFetcher.swift does `continue` here too).
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "chatgpt_base_url" {
            continue;
        }
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'').trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn normalize_base_url(value: &str) -> String {
    let mut trimmed = value.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        trimmed = DEFAULT_BASE_URL.to_string();
    }
    if (trimmed.starts_with("https://chatgpt.com")
        || trimmed.starts_with("https://chat.openai.com"))
        && !trimmed.contains("/backend-api")
    {
        trimmed.push_str("/backend-api");
    }
    trimmed
}

#[derive(Deserialize)]
struct WindowSnapshot {
    /// Absent means unknown, not zero: a missing percentage used to render as an
    /// empty, confidently green bar.
    #[serde(default)]
    used_percent: Option<f64>,
    #[serde(default)]
    reset_at: i64,
    #[serde(default)]
    limit_window_seconds: i64,
}

impl WindowSnapshot {
    fn minutes(&self) -> Option<u64> {
        (self.limit_window_seconds > 0).then_some((self.limit_window_seconds / 60) as u64)
    }

    fn into_window(self, label: String) -> UsageWindow {
        UsageWindow::new(
            label,
            self.used_percent,
            (self.reset_at > 0)
                .then(|| DateTime::from_timestamp(self.reset_at, 0))
                .flatten(),
            self.minutes(),
        )
    }
}

/// Decoded per field so one malformed window cannot discard its siblings.
fn window_at(parent: Option<&Value>, key: &str) -> Option<WindowSnapshot> {
    serde_json::from_value(parent?.get(key)?.clone()).ok()
}

fn label_for(minutes: Option<u64>) -> String {
    match minutes {
        Some(300) => "5h".into(),
        Some(10080) => "Weekly".into(),
        Some(m) if m % 1440 == 0 => format!("{}d", m / 1440),
        Some(m) if m % 60 == 0 => format!("{}h", m / 60),
        Some(m) => format!("{m}m"),
        None => "Session".into(),
    }
}

fn map_usage(body: &Value, auth: &CodexAuth) -> UsageSnapshot {
    let rate_limit = body.get("rate_limit");
    let primary = window_at(rate_limit, "primary_window");
    let secondary = window_at(rate_limit, "secondary_window");

    // The API occasionally swaps the two lanes; classify by window length like CodexBar.
    let (primary, secondary) = match (primary, secondary) {
        (Some(p), Some(s)) if p.minutes() == Some(10080) && s.minutes() != Some(10080) => {
            (Some(s), Some(p))
        }
        (Some(p), None) if p.minutes() == Some(10080) => (None, Some(p)),
        (None, Some(s)) if s.minutes() != Some(10080) => (Some(s), None),
        pair => pair,
    };

    // The label comes from what the API actually reported. A window with no
    // limit_window_seconds is "Session", never an invented "5h" or "Weekly": a monthly
    // quota labelled "5h" makes the user plan their day around a reset 29 days out.
    let mut snap = UsageSnapshot::new("codex");
    snap.primary = primary.map(|w| {
        let label = label_for(w.minutes());
        w.into_window(label)
    });
    snap.secondary = secondary.map(|w| {
        let label = label_for(w.minutes());
        w.into_window(label)
    });
    snap.tertiary = additional_window(body);

    // A plan without credits still reports "balance": "0"; showing that helps nobody.
    snap.credits = body
        .get("credits")
        .filter(|c| c.get("has_credits").and_then(Value::as_bool) == Some(true))
        .and_then(|c| c.get("balance"))
        .and_then(|b| {
            b.as_f64()
                .or_else(|| b.as_str().and_then(|s| s.trim().parse().ok()))
        });

    snap.plan = body
        .get("plan_type")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| auth.plan_from_claims());
    // Row 35. The account id is already decoded for the `ChatGPT-Account-Id` header, and
    // it is what changes when the user runs `codex login` into a second account, so
    // history keys on it instead of appending the new account onto the old series.
    snap.account_key = auth.account_id();
    // The id_token carries the email, but the usage payload repeats it for API-key logins.
    snap.account = auth.email().or_else(|| {
        body.get("email")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    });
    snap
}

/// `additional_rate_limits` carries model-specific limits (Codex Spark and friends).
/// The snapshot has one spare lane, so exactly one entry can be shown.
///
/// The pick is the alphabetically first name, not the first array element: the API is
/// free to reorder the array between refreshes, and a tertiary lane that silently swaps
/// identity draws a cliff in the sparkline that never happened.
fn additional_window(body: &Value) -> Option<UsageWindow> {
    body.get("additional_rate_limits")?
        .as_array()?
        .iter()
        .filter_map(|entry| {
            let rate_limit = entry.get("rate_limit");
            let snapshot = window_at(rate_limit, "primary_window")
                .or_else(|| window_at(rate_limit, "secondary_window"))?;
            let name = ["limit_name", "metered_feature"]
                .iter()
                .filter_map(|k| entry.get(*k).and_then(Value::as_str))
                .map(str::trim)
                .find(|s| !s.is_empty())
                .unwrap_or("Extra limit")
                .to_string();
            Some((name, snapshot))
        })
        .min_by(|a, b| a.0.cmp(&b.0))
        .map(|(name, snapshot)| snapshot.into_window(name))
}

// MARK: small helpers

fn str_field(obj: &Value, snake: &str, camel: &str) -> Option<String> {
    [snake, camel]
        .iter()
        .filter_map(|k| obj.get(*k).and_then(Value::as_str))
        .find(|s| !s.is_empty())
        .map(str::to_string)
}

fn claim_str(claims: &Value, namespace: &str, key: &str) -> Option<String> {
    claims
        .get(namespace)
        .and_then(|d| d.get(key))
        .or_else(|| claims.get(key))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_rfc3339(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Payload claims of a JWT. No signature verification: these tokens come from a file
/// we already trust and are only read for display fields.
fn decode_jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_windows_credits_and_extra_limit() {
        let body: Value = serde_json::from_str(
            r#"{
              "plan_type": "pro",
              "rate_limit": {
                "primary_window": {"used_percent": 42, "reset_at": 2000000000, "limit_window_seconds": 18000},
                "secondary_window": {"used_percent": 7.5, "reset_at": 2000600000, "limit_window_seconds": 604800}
              },
              "credits": {"has_credits": true, "unlimited": false, "balance": "12.5"},
              "additional_rate_limits": [
                {"limit_name": "GPT-5.3-Codex-Spark", "rate_limit": {"primary_window": {"used_percent": 300, "reset_at": 0, "limit_window_seconds": 18000}}}
              ]
            }"#,
        )
        .unwrap();
        let auth = CodexAuth {
            access_token: String::new(),
            refresh_token: String::new(),
            id_token: None,
            account_id: None,
            last_refresh: None,
            raw: Value::Null,
        };

        let snap = map_usage(&body, &auth);
        let primary = snap.primary.unwrap();
        assert_eq!(primary.label, "5h");
        assert_eq!(primary.used_percent, Some(42.0));
        assert_eq!(primary.window_minutes, Some(300));
        assert!(primary.resets_at.is_some());
        assert_eq!(snap.secondary.unwrap().label, "Weekly");
        let extra = snap.tertiary.unwrap();
        assert_eq!(extra.label, "GPT-5.3-Codex-Spark");
        assert_eq!(extra.used_percent, Some(100.0)); // clamped
        assert_eq!(extra.resets_at, None); // reset_at 0 means unknown
        assert_eq!(snap.credits, Some(12.5));
        assert_eq!(
            map_usage(
                &serde_json::json!({"credits": {"has_credits": false, "balance": "0"}}),
                &auth
            )
            .credits,
            None
        );
        assert_eq!(snap.plan.as_deref(), Some("pro"));
    }

    #[test]
    fn swapped_lanes_are_reordered_by_window_length() {
        let body: Value = serde_json::from_str(
            r#"{"rate_limit": {
                 "primary_window": {"used_percent": 10, "reset_at": 1, "limit_window_seconds": 604800},
                 "secondary_window": {"used_percent": 20, "reset_at": 2, "limit_window_seconds": 18000}}}"#,
        )
        .unwrap();
        let auth = CodexAuth {
            access_token: String::new(),
            refresh_token: String::new(),
            id_token: None,
            account_id: None,
            last_refresh: None,
            raw: Value::Null,
        };
        let snap = map_usage(&body, &auth);
        assert_eq!(snap.primary.unwrap().used_percent, Some(20.0));
        assert_eq!(snap.secondary.unwrap().used_percent, Some(10.0));
    }

    /// Row 18. A window the API did not size is "Session", never an invented "5h" or
    /// "Weekly", and the extra-limit lane does not depend on array order.
    #[test]
    fn unsized_windows_keep_a_neutral_label_and_the_extra_lane_is_stable() {
        let auth = CodexAuth {
            access_token: String::new(),
            refresh_token: String::new(),
            id_token: None,
            account_id: None,
            last_refresh: None,
            raw: Value::Null,
        };
        let body: Value = serde_json::from_str(
            r#"{"rate_limit": {
                 "primary_window": {"used_percent": 10, "reset_at": 0},
                 "secondary_window": {"used_percent": 20, "reset_at": 0}}}"#,
        )
        .unwrap();
        let snap = map_usage(&body, &auth);
        assert_eq!(snap.primary.as_ref().unwrap().label, "Session");
        assert_eq!(snap.primary.unwrap().window_minutes, None);
        assert_eq!(snap.secondary.unwrap().label, "Session");

        let one_order: Value = serde_json::from_str(
            r#"{"additional_rate_limits": [
                 {"limit_name": "Zeta", "rate_limit": {"primary_window": {"used_percent": 1}}},
                 {"limit_name": "Alpha", "rate_limit": {"primary_window": {"used_percent": 2}}}]}"#,
        )
        .unwrap();
        let other_order: Value = serde_json::from_str(
            r#"{"additional_rate_limits": [
                 {"limit_name": "Alpha", "rate_limit": {"primary_window": {"used_percent": 2}}},
                 {"limit_name": "Zeta", "rate_limit": {"primary_window": {"used_percent": 1}}}]}"#,
        )
        .unwrap();
        assert_eq!(additional_window(&one_order).unwrap().label, "Alpha");
        assert_eq!(additional_window(&other_order).unwrap().label, "Alpha");

        // A window the API sent with no percentage at all is unknown, not 0 percent used.
        let unsized_pct: Value =
            serde_json::from_str(r#"{"rate_limit": {"primary_window": {"reset_at": 0}}}"#).unwrap();
        assert_eq!(
            map_usage(&unsized_pct, &auth).primary.unwrap().used_percent,
            None
        );
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("agentsbar-codex-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn auth_with(raw: Value) -> CodexAuth {
        CodexAuth {
            access_token: "new-access".into(),
            refresh_token: "new-refresh".into(),
            id_token: None,
            account_id: None,
            last_refresh: None,
            raw,
        }
    }

    /// Row 11. One rotation per window per provider, whatever the credential file says,
    /// and a denied claim does not slide the window forward.
    #[test]
    fn the_refresh_floor_allows_one_attempt_per_window() {
        let gate = RefreshFloor::new();
        let t0 = Instant::now();
        assert!(gate.claim_at(t0), "the first attempt must run");
        assert!(!gate.claim_at(t0 + Duration::from_secs(5 * 60)));
        assert!(!gate.claim_at(t0 + REFRESH_FLOOR - Duration::from_secs(1)));
        assert!(gate.claim_at(t0 + REFRESH_FLOOR), "the window has elapsed");
        assert!(!gate.claim_at(t0 + REFRESH_FLOOR + Duration::from_secs(1)));
    }

    /// Row 10. `save` reads auth.json again instead of replaying the copy captured
    /// before the network call, so a write the CLI made in between survives, and keys
    /// inside `tokens` that we do not model are merged rather than dropped.
    #[test]
    fn save_rereads_the_file_and_merges_unknown_keys() {
        let path = scratch("save-merge").join("auth.json");
        std::fs::write(
            &path,
            r#"{"OPENAI_API_KEY": null,
                "written_by_the_cli_after_we_loaded": "keep me",
                "tokens": {"access_token": "old", "refresh_token": "old", "oauth_scope": "keep me too"}}"#,
        )
        .unwrap();

        // The stale in-memory copy: it has neither of the CLI's keys.
        auth_with(serde_json::json!({"tokens": {"access_token": "stale"}}))
            .save_to(&path)
            .unwrap();

        let back: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            back["written_by_the_cli_after_we_loaded"], "keep me",
            "a key the CLI added during our request window was clobbered"
        );
        assert_eq!(back["tokens"]["oauth_scope"], "keep me too");
        assert_eq!(back["tokens"]["access_token"], "new-access");
        assert_eq!(back["tokens"]["refresh_token"], "new-refresh");
        assert!(back["last_refresh"].is_string());
        assert!(!path.with_extension("json.agentsbar-tmp").exists());
    }

    /// An unreadable or corrupt file falls back to what we loaded, it does not erase.
    #[test]
    fn save_falls_back_to_the_loaded_copy_when_the_file_is_unusable() {
        let path = scratch("save-corrupt").join("auth.json");
        std::fs::write(&path, "{ not json").unwrap();
        auth_with(serde_json::json!({"loaded_key": "kept"}))
            .save_to(&path)
            .unwrap();
        let back: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back["loaded_key"], "kept");
        assert_eq!(back["tokens"]["access_token"], "new-access");
    }

    /// Row 10. A write that cannot land is fatal to the refresh: the rotation already
    /// happened server side, so continuing would hand the next tick a dead token. The
    /// message must name the fix and must never carry the token.
    #[test]
    fn a_failed_write_becomes_a_loud_auth_error() {
        let blocker = scratch("save-fail").join("not-a-directory");
        std::fs::write(&blocker, "").unwrap();
        let path = blocker.join("auth.json");

        let err = auth_with(Value::Null).save_to(&path).unwrap_err();
        match persist_failed(err) {
            ProviderError::Auth(msg) => {
                assert!(msg.contains("codex login"), "{msg}");
                assert!(
                    !msg.contains("new-access") && !msg.contains("new-refresh"),
                    "{msg}"
                );
            }
            other => panic!("a lost credential write must fail the refresh: {other}"),
        }
        assert!(!path.exists());
    }

    #[test]
    fn usage_url_variants() {
        assert_eq!(
            normalize_base_url("https://chatgpt.com/") + "/wham/usage",
            "https://chatgpt.com/backend-api/wham/usage"
        );
        assert_eq!(
            parse_base_url(
                "model = \"gpt\"\nchatgpt_base_url = \"https://proxy.local/v1\" # note\n"
            )
            .as_deref(),
            Some("https://proxy.local/v1")
        );
        // Lines without an `=` must not stop the scan.
        assert_eq!(
            parse_base_url(
                "# comment\n\n[tui]\nsomething\nchatgpt_base_url = \"https://proxy.local/v1\"\n"
            )
            .as_deref(),
            Some("https://proxy.local/v1")
        );
        assert_eq!(parse_base_url("[tui]\nmodel = \"gpt\"\n"), None);
    }

    #[test]
    fn jwt_claims_decode_without_padding() {
        // {"email":"a@b.c"} base64url, no padding.
        let token = "x.eyJlbWFpbCI6ImFAYi5jIn0.y";
        let claims = decode_jwt_claims(token).unwrap();
        assert_eq!(claims.get("email").unwrap().as_str(), Some("a@b.c"));
    }

    /// Real-credential smoke test. Run with:
    /// cargo test -p agentsbar codex_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs real ~/.codex/auth.json"]
    async fn codex_live_smoke() {
        let ctx = FetchContext {
            http: reqwest::Client::new(),
            config: Config::default(),
        };
        match Codex.fetch(&ctx).await {
            Ok(s) => {
                for (lane, w) in [
                    ("primary", &s.primary),
                    ("secondary", &s.secondary),
                    ("tertiary", &s.tertiary),
                ] {
                    if let Some(w) = w {
                        println!(
                            "codex {lane}: {} used {:?}% window {:?}m resets {:?}",
                            w.label, w.used_percent, w.window_minutes, w.resets_at
                        );
                    }
                }
                println!(
                    "codex plan={:?} credits={:?} account_present={}",
                    s.plan,
                    s.credits,
                    s.account.is_some()
                );
            }
            Err(e) => println!("codex fetch failed: {e}"),
        }
    }
}
