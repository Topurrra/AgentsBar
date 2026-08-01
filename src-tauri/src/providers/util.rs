use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

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

    #[test]
    fn percent_edges() {
        assert_eq!(percent(0.0, 0.0), 0.0);
        assert_eq!(percent(5.0, 10.0), 50.0);
        assert_eq!(percent(20.0, 10.0), 100.0);
        assert_eq!(remaining_percent(150.0), 0.0);
    }
}
