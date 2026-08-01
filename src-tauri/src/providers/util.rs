use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::api_token::TIMEOUT;
use super::ProviderError;

/// Read and deserialize a JSON credential/config file.
/// A missing file is `NotConfigured`, a broken one is `Parse`.
pub fn read_json_file<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T, ProviderError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|_| ProviderError::NotConfigured)?;
    serde_json::from_str(&text)
        .map_err(|e| ProviderError::Parse(format!("{}: {e}", path.display())))
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(std::env::temp_dir)
}

/// `%USERPROFILE%\<name>`, unless `env_override` names an env var that is set.
pub fn home_subdir(name: &str, env_override: Option<&str>) -> PathBuf {
    if let Some(var) = env_override {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                return PathBuf::from(v);
            }
        }
    }
    home_dir().join(name)
}

pub fn local_app_data() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir)
}

/// used / limit as a percentage, clamped to 0..=100. Zero limit reads as 0.
pub fn percent(used: f64, limit: f64) -> f64 {
    if limit <= 0.0 {
        return 0.0;
    }
    (used / limit * 100.0).clamp(0.0, 100.0)
}

pub fn remaining_percent(used_percent: f64) -> f64 {
    (100.0 - used_percent).clamp(0.0, 100.0)
}

/// The error for a response that came back with a status we cannot use.
///
/// Row 23. A 429 is a rate limit at every endpoint, not a generic transport failure, so it
/// is classified as one and carries whatever `Retry-After` the server sent. That is what
/// makes the scheduler wait the length it was asked for instead of a flat five minutes,
/// makes the tile print the rate limited copy instead of "could not reach", and keeps the
/// retry ladder (which exists for "the network is down") from dragging every other provider
/// onto a 15 second cadence to be told 429 faster. Anything else keeps the caller's own
/// message, which is the one that names the endpoint.
///
/// Must be called before the body is consumed: reading it takes the response by value.
pub fn http_error(resp: &reqwest::Response, message: impl FnOnce() -> String) -> ProviderError {
    match resp.status().as_u16() {
        429 => ProviderError::RateLimited {
            retry_after: super::retry_after_of(resp),
        },
        _ => ProviderError::Http(message()),
    }
}

/// True when a URL is a sign-in page or an identity provider's host: a `login`, `signin`
/// or `sign-in` path segment, or a host under `auth.`, WorkOS, Auth0, Okta or Clerk.
///
/// This is how a bounced session is told from a page that merely moved. It must stay
/// cheap and stringly: the input is whatever a provider was redirected to.
pub fn is_login_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let after_scheme = lower
        .split_once("://")
        .map_or(lower.as_str(), |(_, rest)| rest);
    let host = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let path = after_scheme.split(['?', '#']).next().unwrap_or("");
    host.starts_with("auth.")
        || ["workos.com", "auth0.com", "okta.com", "clerk."]
            .iter()
            .any(|idp| host.contains(idp))
        || path
            .split('/')
            .any(|segment| matches!(segment, "login" | "signin" | "sign-in"))
}

/// The `Location` of a redirect response, if it is one.
///
/// The shared client refuses to follow a redirect that leaves the request origin (see
/// [`crate::state::AppState::new`]), because these requests carry imported cookies. So a
/// 3xx reaching a provider IS the sign-out bounce, and its `Location` is the only evidence
/// of where the session was sent: `response.url()` still names the page we asked for.
pub fn redirect_target(response: &reqwest::Response) -> Option<&str> {
    response
        .status()
        .is_redirection()
        .then(|| {
            response
                .headers()
                .get(reqwest::header::LOCATION)?
                .to_str()
                .ok()
        })
        .flatten()
}

// ------------------------------------------------------------------ cookie web helpers

/// GET returning the raw body. `headers` carries `Cookie` or `Authorization`; neither the
/// error nor any log line ever repeats a header value.
pub async fn web_get(
    http: &reqwest::Client,
    url: &str,
    headers: &[(&str, &str)],
    signin_hint: &str,
) -> Result<String, ProviderError> {
    let mut req = http.get(url).timeout(TIMEOUT);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    web_send(req, signin_hint).await
}

/// POST with a JSON body, returning the raw body.
pub async fn web_post(
    http: &reqwest::Client,
    url: &str,
    headers: &[(&str, &str)],
    body: &Value,
    signin_hint: &str,
) -> Result<String, ProviderError> {
    let mut req = http.post(url).json(body).timeout(TIMEOUT);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    web_send(req, signin_hint).await
}

/// The shared response policy: 401/403 and a bounce to an identity provider are `Auth`,
/// a bot mitigation 429 is an actionable `Http`, a bare 429 is a `RateLimited`.
pub async fn web_send(
    req: reqwest::RequestBuilder,
    signin_hint: &str,
) -> Result<String, ProviderError> {
    let resp = req
        .send()
        .await
        .map_err(|e| ProviderError::Http(e.to_string()))?;
    let status = resp.status();
    if status == 401 || status == 403 {
        return Err(ProviderError::Auth(signin_hint.to_string()));
    }
    if let Some(target) = redirect_target(&resp) {
        return Err(if is_login_url(target) {
            ProviderError::Auth(signin_hint.to_string())
        } else {
            ProviderError::Http(format!(
                "HTTP {}, redirected off the request origin and not followed",
                status.as_u16()
            ))
        });
    }
    if status == 429 {
        let challenged = ["x-vercel-mitigated", "cf-mitigated"]
            .iter()
            .any(|h| resp.headers().contains_key(*h));
        return Err(if challenged {
            ProviderError::Http(
                "blocked by the site's bot mitigation (HTTP 429 challenge). Open the \
                 provider's site in your browser, pass the check, then refresh"
                    .to_string(),
            )
        } else {
            ProviderError::RateLimited {
                retry_after: super::retry_after_of(&resp),
            }
        });
    }
    if !status.is_success() {
        return Err(ProviderError::Http(format!("HTTP {}", status.as_u16())));
    }
    resp.text()
        .await
        .map_err(|e| ProviderError::Http(e.to_string()))
}

pub fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, ProviderError> {
    serde_json::from_str(text).map_err(|e| ProviderError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_urls_and_identity_hosts_are_recognised() {
        for url in [
            "https://ampcode.com/auth/sign-in?returnTo=/settings",
            "https://auth.ampcode.com/authorize",
            "https://ampcode.com/login",
            "https://accounts.example.workos.com/sso",
        ] {
            assert!(is_login_url(url), "{url}");
        }
        for url in [
            "https://ampcode.com/settings",
            // "logins" is not "login".
            "https://ampcode.com/settings/logins",
            "https://api.cursor.com/dashboard/get-usage",
        ] {
            assert!(!is_login_url(url), "{url}");
        }
    }

    /// Row 23. Every endpoint in the app must classify a 429 as a rate limit, or the
    /// scheduler waits a flat five minutes instead of the length the server asked for and
    /// the tile prints "could not reach" over a provider that answered in detail.
    #[test]
    fn a_429_is_a_rate_limit_everywhere_and_carries_the_servers_own_wait() {
        let response = |status: u16, retry_after: Option<&str>| {
            let mut b = http::Response::builder().status(status);
            if let Some(v) = retry_after {
                b = b.header("retry-after", v);
            }
            reqwest::Response::from(b.body(Vec::new()).unwrap())
        };
        let boom = || "endpoint said no".to_string();

        let limited = http_error(&response(429, Some("90")), boom);
        assert!(matches!(limited, ProviderError::RateLimited { .. }));
        assert_eq!(limited.retry_after().map(|d| d.as_secs()), Some(90));

        // No header: still a rate limit, and the caller applies its own cooldown.
        let bare = http_error(&response(429, None), boom);
        assert!(matches!(bare, ProviderError::RateLimited { .. }));
        assert_eq!(bare.retry_after(), None);

        // Everything else keeps the caller's message, which names the endpoint.
        for status in [500, 503, 404, 302] {
            let other = http_error(&response(status, None), boom);
            assert!(
                matches!(&other, ProviderError::Http(m) if m == "endpoint said no"),
                "{status}: {other:?}"
            );
        }
    }

    #[test]
    fn percent_edges() {
        assert_eq!(percent(0.0, 0.0), 0.0);
        assert_eq!(percent(5.0, 10.0), 50.0);
        assert_eq!(percent(20.0, 10.0), 100.0);
        assert_eq!(remaining_percent(150.0), 0.0);
    }
}
