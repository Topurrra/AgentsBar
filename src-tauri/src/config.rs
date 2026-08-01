use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const DEFAULT_ENABLED: [&str; 4] = ["codex", "claude", "gemini", "copilot"];

/// Where a cookie provider gets its `Cookie` header from.
pub const COOKIE_SOURCES: [&str; 3] = ["auto", "off", "manual"];
/// Browsers the cookie layer can read, in the order `auto` tries them.
pub const COOKIE_BROWSERS: [&str; 4] = ["chrome", "edge", "brave", "firefox"];

fn default_cookie_source() -> String {
    "auto".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: Option<String>,
    /// "auto" | "off" | "manual". Only meaningful for `AuthKind::Cookie` providers.
    #[serde(default = "default_cookie_source")]
    pub cookie_source: String,
    /// Pin `auto` to one browser. `None` means try every detected browser in order.
    #[serde(default)]
    pub cookie_browser: Option<String>,
    /// A pasted `Cookie` header. As sensitive as `api_key`: redacted on the way out,
    /// merged back on the way in, never logged.
    #[serde(default)]
    pub cookie_header: Option<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key: None,
            cookie_source: default_cookie_source(),
            cookie_browser: None,
            cookie_header: None,
        }
    }
}

impl ProviderConfig {
    fn defaults_for(id: &str) -> Self {
        Self {
            enabled: DEFAULT_ENABLED.contains(&id),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub refresh_minutes: u64,
    pub pinned_provider: Option<String>,
    pub providers: HashMap<String, ProviderConfig>,
    pub launch_at_startup: bool,
    pub theme: String,
}

impl Default for Config {
    fn default() -> Self {
        let providers = crate::providers::all_providers()
            .iter()
            .map(|p| (p.id().to_string(), ProviderConfig::defaults_for(p.id())))
            .collect();
        Self {
            refresh_minutes: 5,
            pinned_provider: None,
            providers,
            launch_at_startup: false,
            theme: "auto".to_string(),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("AgentBar")
            .join("config.json")
    }

    /// Never fails: a missing file falls back to defaults. A corrupt file is moved
    /// aside first, so the API keys in it survive the defaults being written back.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut cfg) => {
                cfg.normalize();
                cfg
            }
            Err(e) => {
                let bad = path.with_extension("json.bad");
                let moved = std::fs::rename(&path, &bad).is_ok();
                log::warn!(
                    "config parse failed ({e}), using defaults; old file {}",
                    if moved {
                        format!("kept at {}", bad.display())
                    } else {
                        "could not be moved aside".to_string()
                    }
                );
                Self::default()
            }
        }
    }

    pub fn parse(text: &str) -> Self {
        match serde_json::from_str::<Self>(text) {
            Ok(mut cfg) => {
                cfg.normalize();
                cfg
            }
            Err(e) => {
                log::warn!("config parse failed, using defaults: {e}");
                Self::default()
            }
        }
    }

    /// Clamp user supplied values and add providers the file predates.
    /// The upper bound keeps `refresh_minutes * 60` from overflowing a u64 duration.
    pub fn normalize(&mut self) {
        self.refresh_minutes = self.refresh_minutes.clamp(1, 1440);
        for provider in self.providers.values_mut() {
            // An unknown source would silently disable a provider, so fall back to auto.
            if !COOKIE_SOURCES.contains(&provider.cookie_source.as_str()) {
                provider.cookie_source = default_cookie_source();
            }
            provider.cookie_browser = provider
                .cookie_browser
                .take()
                .filter(|b| COOKIE_BROWSERS.contains(&b.as_str()));
            provider.cookie_header = provider
                .cookie_header
                .take()
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty());
        }
        self.fill_missing_providers();
    }

    /// Copy of the config without secrets, for anything that leaves the backend.
    pub fn redacted(&self) -> Self {
        let mut copy = self.clone();
        for provider in copy.providers.values_mut() {
            provider.api_key = None;
            provider.cookie_header = None;
        }
        copy
    }

    /// The webview only ever sees a redacted config, so an absent secret in an incoming
    /// config means "unchanged", never "cleared". Clearing goes through set_api_key or
    /// set_cookie_header.
    pub fn merge_keys_from(&mut self, current: &Self) {
        for (id, provider) in self.providers.iter_mut() {
            let Some(existing) = current.providers.get(id) else {
                continue;
            };
            if provider.api_key.is_none() {
                provider.api_key = existing.api_key.clone();
            }
            if provider.cookie_header.is_none() {
                provider.cookie_header = existing.cookie_header.clone();
            }
        }
    }

    /// Providers added in a later release are absent from an older config file.
    pub fn fill_missing_providers(&mut self) {
        for p in crate::providers::all_providers() {
            self.providers
                .entry(p.id().to_string())
                .or_insert_with(|| ProviderConfig::defaults_for(p.id()));
        }
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.get(id)
    }

    pub fn api_key(&self, id: &str) -> Option<&str> {
        self.providers
            .get(id)
            .and_then(|p| p.api_key.as_deref())
            .filter(|k| !k.trim().is_empty())
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.providers.get(id).is_some_and(|p| p.enabled)
    }

    /// Always one of [`COOKIE_SOURCES`]; providers absent from the file default to auto.
    pub fn cookie_source(&self, id: &str) -> &str {
        self.providers
            .get(id)
            .map(|p| p.cookie_source.as_str())
            .filter(|s| COOKIE_SOURCES.contains(s))
            .unwrap_or("auto")
    }

    pub fn cookie_browser(&self, id: &str) -> Option<&str> {
        self.providers
            .get(id)?
            .cookie_browser
            .as_deref()
            .filter(|b| COOKIE_BROWSERS.contains(b))
    }

    pub fn cookie_header(&self, id: &str) -> Option<&str> {
        self.providers
            .get(id)
            .and_then(|p| p.cookie_header.as_deref())
            .filter(|h| !h.trim().is_empty())
    }

    /// Atomic write: temp file next to the target, then rename over it.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(&tmp, json)?;
        // The temp file holds every API key in plaintext, so it must not survive a
        // failed rename.
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_never_panics_and_keeps_defaults() {
        // Corrupt file: defaults, not a panic.
        let cfg = Config::parse("{ not json ");
        assert_eq!(cfg.refresh_minutes, 5);
        assert!(cfg.is_enabled("codex"));
        assert!(!cfg.is_enabled("openrouter"));

        // Partial file: missing keys default, missing providers get filled in.
        let cfg = Config::parse(r#"{"refresh_minutes":0,"providers":{"openrouter":{"enabled":true}}}"#);
        assert_eq!(cfg.refresh_minutes, 1);
        assert!(cfg.is_enabled("openrouter"));
        assert!(cfg.provider("codex").is_some());
        assert_eq!(cfg.api_key("openrouter"), None);

        // Round trip through the serialized form.
        let again = Config::parse(&serde_json::to_string(&cfg).unwrap());
        assert_eq!(again.refresh_minutes, cfg.refresh_minutes);
        assert!(again.is_enabled("openrouter"));
    }

    #[test]
    fn refresh_minutes_cannot_overflow_the_sleep_duration() {
        let cfg = Config::parse(r#"{"refresh_minutes":18446744073709551615}"#);
        assert_eq!(cfg.refresh_minutes, 1440);
        assert!(cfg.refresh_minutes.checked_mul(60).is_some());
    }

    #[test]
    fn keys_are_redacted_outbound_and_restored_inbound() {
        let mut stored = Config::default();
        stored.providers.get_mut("openrouter").unwrap().api_key = Some("secret".into());

        let mut outbound = stored.redacted();
        assert_eq!(outbound.api_key("openrouter"), None);

        // A toggle saved from that redacted copy must not drop the stored key.
        outbound.providers.get_mut("openrouter").unwrap().enabled = true;
        outbound.merge_keys_from(&stored);
        assert_eq!(outbound.api_key("openrouter"), Some("secret"));
        assert!(outbound.is_enabled("openrouter"));
    }

    #[test]
    fn cookie_headers_are_secrets_like_api_keys() {
        let mut stored = Config::default();
        stored.providers.get_mut("cursor").unwrap().cookie_header = Some("a=1; b=2".into());

        let mut outbound = stored.redacted();
        assert_eq!(outbound.cookie_header("cursor"), None);

        outbound.providers.get_mut("cursor").unwrap().enabled = true;
        outbound.merge_keys_from(&stored);
        assert_eq!(outbound.cookie_header("cursor"), Some("a=1; b=2"));
    }

    #[test]
    fn cookie_fields_default_and_normalize() {
        // Absent from an older config file: auto, no pinned browser, no header.
        let cfg = Config::parse(r#"{"providers":{"cursor":{"enabled":true}}}"#);
        assert_eq!(cfg.cookie_source("cursor"), "auto");
        assert_eq!(cfg.cookie_browser("cursor"), None);
        assert_eq!(cfg.cookie_header("cursor"), None);
        // A provider missing entirely still answers auto rather than panicking.
        assert_eq!(cfg.cookie_source("not-a-provider"), "auto");

        // Junk from a hand-edited file falls back instead of disabling the provider.
        let cfg = Config::parse(
            r#"{"providers":{"cursor":{"cookie_source":"nonsense","cookie_browser":"netscape","cookie_header":"   "}}}"#,
        );
        assert_eq!(cfg.cookie_source("cursor"), "auto");
        assert_eq!(cfg.cookie_browser("cursor"), None);
        assert_eq!(cfg.cookie_header("cursor"), None);

        let cfg = Config::parse(
            r#"{"providers":{"cursor":{"cookie_source":"manual","cookie_browser":"edge","cookie_header":" a=1 "}}}"#,
        );
        assert_eq!(cfg.cookie_source("cursor"), "manual");
        assert_eq!(cfg.cookie_browser("cursor"), Some("edge"));
        assert_eq!(cfg.cookie_header("cursor"), Some("a=1"));
    }
}
