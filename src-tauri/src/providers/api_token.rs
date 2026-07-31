//! Shared strategy for providers whose credential is a plain API key or token stored
//! in the AgentBar config. Modeled on APITokenFetchStrategy.swift: resolve the token,
//! perform one JSON call, hand the decoded body to the caller's mapper.
//!
//! Not itself in the registry: the per-provider modules build on it.

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use reqwest::{Client, RequestBuilder};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::ProviderError;
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

/// The configured key for `id`, trimmed. Absent or blank reads as NotConfigured.
pub fn api_key<'a>(config: &'a Config, id: &str) -> Result<&'a str, ProviderError> {
    Ok(config
        .api_key(id)
        .ok_or(ProviderError::NotConfigured)?
        .trim())
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
