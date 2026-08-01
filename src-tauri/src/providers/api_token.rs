//! Shared strategy for providers whose credential is a plain API key or token stored
//! in the AgentsBar config. Modeled on APITokenFetchStrategy.swift: resolve the token,
//! perform one JSON call, hand the decoded body to the caller's mapper.
//!
//! Not itself in the registry: the per-provider modules build on it.

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use reqwest::{Client, RequestBuilder};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{Provider, ProviderError};
use crate::config::Config;

/// Every provider API call gets the same bound. CodexBar uses 15s throughout.
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// How the credential is presented. Most providers want `Bearer`, a few use a
/// custom header (ElevenLabs `xi-api-key`, Deepgram `Authorization: Token ...`).
pub enum Auth<'a> {
    Bearer(&'a str),
    Header(&'a str, &'a str),
}

impl Auth<'_> {
    fn apply(&self, req: RequestBuilder) -> RequestBuilder {
        match self {
            Auth::Bearer(token) => req.header("Authorization", format!("Bearer {token}")),
            Auth::Header(name, value) => req.header(*name, *value),
        }
    }
}

/// Row 38. The credential for `provider`, trimmed: the config first, then the provider's
/// [`Provider::env_key`] environment variable. Absent or blank in both reads as
/// NotConfigured.
///
/// The config wins deliberately. A key the user typed into Settings is the one they can
/// see and change; a stale `OPENROUTER_API_KEY` left in a shell profile silently shadowing
/// it would be undebuggable from inside the app.
pub fn api_key(config: &Config, provider: &dyn Provider) -> Result<String, ProviderError> {
    resolve_key(config, provider, from_env).ok_or(ProviderError::NotConfigured)
}

/// [`api_key`] as a yes or no, for `is_configured`. Without this an env-only key would
/// leave the tile "not configured" and the provider would never be fetched at all.
pub fn has_api_key(config: &Config, provider: &dyn Provider) -> bool {
    resolve_key(config, provider, from_env).is_some()
}

/// The value never reaches a log or an error: only `is_some` is ever reported.
fn from_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// The resolution itself, against an injected environment so the precedence is testable
/// without mutating the process environment.
fn resolve_key(
    config: &Config,
    provider: &dyn Provider,
    env: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    config
        .api_key(provider.id())
        .map(str::to_string)
        .or_else(|| provider.env_key().and_then(env))
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

pub async fn get_json<T: DeserializeOwned>(
    http: &Client,
    url: &str,
    auth: &Auth<'_>,
    extra_headers: &[(&str, &str)],
) -> Result<T, ProviderError> {
    let mut req = auth
        .apply(http.get(url))
        .header("Accept", "application/json")
        .timeout(TIMEOUT);
    for (name, value) in extra_headers {
        req = req.header(*name, *value);
    }
    send(req).await
}

pub async fn post_json<T: DeserializeOwned>(
    http: &Client,
    url: &str,
    auth: &Auth<'_>,
    extra_headers: &[(&str, &str)],
    body: &Value,
) -> Result<T, ProviderError> {
    let mut req = auth
        .apply(http.post(url))
        .header("Accept", "application/json")
        .json(body)
        .timeout(TIMEOUT);
    for (name, value) in extra_headers {
        req = req.header(*name, *value);
    }
    send(req).await
}

/// The response body never reaches the error or the log: provider error envelopes
/// can echo request context, and nothing here is worth risking a credential leak.
/// Only the status code travels out.
async fn send<T: DeserializeOwned>(req: RequestBuilder) -> Result<T, ProviderError> {
    let resp = req
        .send()
        .await
        .map_err(|e| ProviderError::Http(e.to_string()))?;
    let status = resp.status();
    if status == 401 || status == 403 {
        return Err(ProviderError::Auth(format!("HTTP {}", status.as_u16())));
    }
    // Row 23. A 429 carries its own cooldown length often enough to be worth reading, and
    // the header has to come off the response before the body is consumed. `None` means the
    // server did not say, and the scheduler applies its own default rather than no wait.
    if status == 429 {
        return Err(ProviderError::RateLimited {
            retry_after: super::retry_after_of(&resp),
        });
    }
    if !status.is_success() {
        return Err(ProviderError::Http(format!("HTTP {}", status.as_u16())));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| ProviderError::Http(e.to_string()))?;
    if text.trim().is_empty() {
        // A 200 with no body means wrong host or region far more often than "no usage".
        return Err(ProviderError::Parse("empty response body".into()));
    }
    serde_json::from_str(&text).map_err(|e| ProviderError::Parse(e.to_string()))
}

/// Epoch seconds or milliseconds (providers mix both) to UTC.
pub fn epoch_to_utc(raw: i64) -> Option<DateTime<Utc>> {
    if raw > 1_000_000_000_000 {
        Utc.timestamp_millis_opt(raw).single()
    } else if raw > 1_000_000_000 {
        Utc.timestamp_opt(raw, 0).single()
    } else {
        None
    }
}

pub fn parse_rfc3339(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Numeric JSON field that some providers quote and others do not.
pub fn loose_f64(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{AuthKind, FetchContext, UsageSnapshot};

    struct Fake(Option<&'static str>);

    #[async_trait::async_trait]
    impl Provider for Fake {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn name(&self) -> &'static str {
            "Fake"
        }
        fn auth_kind(&self) -> AuthKind {
            AuthKind::ApiKey
        }
        fn env_key(&self) -> Option<&'static str> {
            self.0
        }
        fn is_configured(&self, config: &Config) -> bool {
            has_api_key(config, self)
        }
        async fn fetch(&self, _ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
            unreachable!()
        }
    }

    fn config_with(key: Option<&str>) -> Config {
        let mut config = Config::default();
        if let Some(key) = key {
            config.providers.entry("fake".into()).or_default().api_key = Some(key.to_string());
        }
        config
    }

    /// Row 38. The environment fills in when the config is empty, the config wins when
    /// both are set, and a provider that declares no variable never reads one.
    #[test]
    fn the_config_wins_over_the_environment_and_the_environment_over_nothing() {
        let env = |name: &str| (name == "FAKE_API_KEY").then(|| " from-env ".to_string());
        let declared = Fake(Some("FAKE_API_KEY"));
        let silent = Fake(None);

        assert_eq!(
            resolve_key(&config_with(None), &declared, env).as_deref(),
            Some("from-env"),
            "an exported key must be picked up and trimmed"
        );
        assert_eq!(
            resolve_key(&config_with(Some(" from-config ")), &declared, env).as_deref(),
            Some("from-config"),
            "a saved key must never be shadowed by the environment"
        );
        assert_eq!(resolve_key(&config_with(None), &silent, env), None);
        // A blank export is not a credential.
        assert_eq!(
            resolve_key(&config_with(None), &declared, |_| Some("   ".into())),
            None
        );
        assert_eq!(resolve_key(&config_with(None), &declared, |_| None), None);
    }

    #[test]
    fn an_unresolvable_key_is_not_configured() {
        let provider = Fake(Some("FAKE_API_KEY"));
        assert!(matches!(
            api_key(&config_with(None), &provider),
            Err(ProviderError::NotConfigured)
        ));
        assert!(!provider.is_configured(&config_with(None)));
        assert!(provider.is_configured(&config_with(Some("k"))));
    }

    #[test]
    fn epochs_and_loose_numbers() {
        assert_eq!(
            epoch_to_utc(1_768_507_567_547).map(|d| d.timestamp()),
            Some(1_768_507_567)
        );
        assert_eq!(
            epoch_to_utc(1_738_356_858).map(|d| d.timestamp()),
            Some(1_738_356_858)
        );
        assert_eq!(epoch_to_utc(0), None);
        assert_eq!(loose_f64(Some(&Value::from("2048"))), Some(2048.0));
        assert_eq!(loose_f64(Some(&Value::from(12.5))), Some(12.5));
        assert_eq!(loose_f64(Some(&Value::Null)), None);
        assert_eq!(
            parse_rfc3339("2026-01-09T15:23:13.373329235Z").map(|d| d.timestamp()),
            Some(1_767_972_193)
        );
    }
}
