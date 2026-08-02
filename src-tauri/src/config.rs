use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

const DEFAULT_ENABLED: [&str; 4] = ["codex", "claude", "gemini", "copilot"];

/// `%APPDATA%\AgentsBar`, holding `config.json` and `history.json`.
pub fn dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("AgentsBar")
}

/// Files the legacy directory can hold. Nothing else was ever written there.
const MIGRATED_FILES: [&str; 2] = ["config.json", "history.json"];

/// The product was called AgentBar before the wave 5 rename. Move the old directory to the
/// new name once, at startup, so an existing user keeps their settings, API keys and history.
pub fn migrate_legacy_dir() {
    let Some(base) = dirs::config_dir() else {
        return;
    };
    migrate_dir(&base.join("AgentBar"), &base.join("AgentsBar"));
}

/// The gate is the payload, not the directory: `history.save()` creates the new directory
/// within the first refresh cycle, so keying off `new.exists()` would seal a failed
/// migration permanently. A rename can fail for reasons a same-volume argument does not
/// cover (a surviving old-name copy holding the directory open, an AV or OneDrive handle),
/// and the user's DPAPI-wrapped API keys are on the other side of it, so retry next start
/// and fall back to copying the individual files.
fn migrate_dir(old: &Path, new: &Path) {
    if new.join("config.json").exists() || !old.is_dir() {
        return;
    }
    if std::fs::rename(old, new).is_ok() {
        log::info!("moved settings from {} to {}", old.display(), new.display());
        return;
    }
    // Rename failed. Copy what is there instead, never overwriting anything already in the
    // new directory, and leave the old one alone so the next start can try again.
    let _ = std::fs::create_dir_all(new);
    for name in MIGRATED_FILES {
        let (from, to) = (old.join(name), new.join(name));
        if from.is_file() && !to.exists() {
            match std::fs::copy(&from, &to) {
                Ok(_) => log::info!("copied {name} from {} to {}", old.display(), new.display()),
                Err(e) => log::warn!("could not copy {name} from {}: {e}", old.display()),
            }
        }
    }
}

/// The pre-rename build registered autostart as `AgentBar`. The bundle identifier changed
/// too, so the single-instance lock does not make the two copies exclude each other: left
/// behind, this value starts a second tray icon at every logon writing to a second config
/// directory, and becomes a dead pointer once the old install is deleted.
pub fn remove_legacy_autostart() {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    const RUN_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
    let removed = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
        .and_then(|k| k.delete_value("AgentBar"))
        .is_ok();
    if removed {
        log::info!("removed the pre-rename AgentBar autostart entry");
    }
}

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
    /// Row 24. Adaptive refresh cadence instead of the fixed `refresh_minutes`, which keeps
    /// its value underneath so turning Adaptive off returns the user to the interval they
    /// had.
    ///
    /// The FIELD level `#[serde(default)]` is load bearing. Without it the container
    /// `#[serde(default)]` on `Config` fills a missing key from `Config::default()`, which
    /// is `true`, and every existing `config.json` would silently switch itself to adaptive.
    /// Field level wins, so an absent key reads `false` and an existing install keeps its
    /// interval. A fresh install has no file at all, so it gets `Config::default()` and gets
    /// adaptive, which is exactly the row 24 rule.
    #[serde(default)]
    pub refresh_adaptive: bool,
    pub pinned_provider: Option<String>,
    pub providers: HashMap<String, ProviderConfig>,
    pub launch_at_startup: bool,
    pub theme: String,
    #[serde(default = "default_true")]
    pub notify_on_exhaustion: bool,
    /// Optional HTTP(S) proxy all provider requests are routed through, for corporate
    /// networks that egress via one. `None` (the default) is a direct connection.
    #[serde(default)]
    pub proxy_url: Option<String>,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        let providers = crate::providers::all_providers()
            .iter()
            .map(|p| (p.id().to_string(), ProviderConfig::defaults_for(p.id())))
            .collect();
        Self {
            refresh_minutes: 5,
            refresh_adaptive: true,
            pinned_provider: None,
            providers,
            launch_at_startup: false,
            theme: "auto".to_string(),
            notify_on_exhaustion: true,
            proxy_url: None,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        dir().join("config.json")
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
        // A blank or whitespace proxy is no proxy at all.
        self.proxy_url = self
            .proxy_url
            .take()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());
        for provider in self.providers.values_mut() {
            provider.unwrap_secrets();
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

    /// Copy of the config with every secret DPAPI wrapped, for writing to disk.
    /// See [`DPAPI_PREFIX`] for what that does and does not protect against.
    fn wrapped_for_disk(&self) -> Self {
        let mut copy = self.clone();
        for provider in copy.providers.values_mut() {
            provider.wrap_secrets();
        }
        copy
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
        // Secrets are wrapped here, on the way out, so the in-memory config stays plain
        // and `redacted`, `merge_keys_from` and the accessors are unaffected. A plaintext
        // value loaded from an older file is upgraded by this write.
        let json = serde_json::to_vec_pretty(&self.wrapped_for_disk())?;
        std::fs::write(&tmp, json)?;
        // The temp file holds every secret the real file does, so it must not survive a
        // failed rename.
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ secrets at rest

/// Marks a value wrapped by DPAPI (`CryptProtectData`, current user scope). The blob is
/// base64 encoded after the prefix so the config file stays plain, hand editable JSON.
///
/// **The honest ceiling.** This does NOT stop code running as the same user: anything
/// running as you can call `CryptUnprotectData` and read these back, exactly the way
/// AgentsBar does. It is not a keystore and there is no master password.
///
/// What it does stop is the file leaking as readable text. A `%APPDATA%` folder synced to
/// OneDrive, a config pasted into a bug report or a GitHub issue, a file level backup or
/// disk image restored on another machine, and file scraping stealers that grab known
/// config paths without executing anything all end up with a blob that is useless off this
/// user account. That is the entire claim.
///
/// Plaintext values are still accepted and are transparently re-wrapped on the next save,
/// so the file stays hand editable and a config written by an older build never locks
/// anyone out.
const DPAPI_PREFIX: &str = "dpapi:";

impl ProviderConfig {
    /// Wrap the two secret fields for disk. A wrap failure keeps the plaintext: losing
    /// the user's key would be worse than storing it the way every earlier build did.
    fn wrap_secrets(&mut self) {
        for field in [&mut self.api_key, &mut self.cookie_header] {
            let Some(plain) = field.as_deref() else {
                continue;
            };
            if plain.starts_with(DPAPI_PREFIX) {
                continue; // already wrapped, never wrap twice
            }
            if let Some(wrapped) = dpapi_wrap(plain) {
                *field = Some(wrapped);
            } else {
                log::warn!("DPAPI wrap failed, storing this secret unwrapped");
            }
        }
    }

    /// Unwrap on the way in. Absent or plaintext values are left alone, so old files and
    /// hand edits keep working.
    fn unwrap_secrets(&mut self) {
        for field in [&mut self.api_key, &mut self.cookie_header] {
            let Some(b64) = field.as_deref().and_then(|v| v.strip_prefix(DPAPI_PREFIX)) else {
                continue;
            };
            let plain = dpapi_unwrap(b64);
            if plain.is_none() {
                // A corrupt blob, or one wrapped by a different user or machine. Drop it
                // rather than hand a ciphertext string to a provider as if it were a key;
                // the user re-enters it and the next save re-wraps it here.
                log::warn!("a stored secret could not be unwrapped and was dropped");
            }
            *field = plain;
        }
    }
}

/// `plain` -> `dpapi:<base64>`, or `None` if the Win32 call fails.
fn dpapi_wrap(plain: &str) -> Option<String> {
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let bytes = plain.as_bytes();
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).ok()?,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let blob = unsafe {
        CryptProtectData(
            &input,
            windows::core::PCWSTR::null(),
            None,
            None,
            None,
            0,
            &mut output,
        )
        .ok()?;
        take_blob(&output)
    };
    Some(format!(
        "{DPAPI_PREFIX}{}",
        base64::engine::general_purpose::STANDARD.encode(blob)
    ))
}

/// The base64 body of a `dpapi:` value back to plaintext. `None` means unreadable: bad
/// base64, a corrupt blob, another user's blob, or bytes that are not UTF-8.
///
/// Mirrors `cookies::dpapi_unprotect`, which is private to that module.
fn dpapi_unwrap(b64: &str) -> Option<String> {
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let blob = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(blob.len()).ok()?,
        pbData: blob.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let plain = unsafe {
        CryptUnprotectData(&input, None, None, None, None, 0, &mut output).ok()?;
        take_blob(&output)
    };
    String::from_utf8(plain).ok()
}

/// Copy a `CryptProtectData`/`CryptUnprotectData` output blob out and free it.
///
/// # Safety
/// `output` must be a blob just filled in by one of those two calls, not yet freed.
unsafe fn take_blob(
    output: &windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB,
) -> Vec<u8> {
    let out = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
    let _ = windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
        output.pbData as *mut _,
    )));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rename must carry a wave 4 user's settings, API keys and history across, and
    /// must never overwrite a directory that is already in use. Runs entirely under
    /// `%TEMP%`; it never looks at the real `%APPDATA%`.
    #[test]
    fn the_legacy_config_directory_moves_once_and_never_clobbers() {
        let root = std::env::temp_dir().join(format!("agentsbar-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (old, new) = (root.join("AgentBar"), root.join("AgentsBar"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("config.json"), r#"{"refresh_minutes":7}"#).unwrap();
        std::fs::write(old.join("history.json"), "{}").unwrap();

        migrate_dir(&old, &new);
        assert!(!old.exists(), "the old directory is moved, not copied");
        assert_eq!(
            std::fs::read_to_string(new.join("config.json")).unwrap(),
            r#"{"refresh_minutes":7}"#
        );
        assert!(new.join("history.json").is_file(), "history came along");

        // Run it again with both present: the live directory wins, nothing is destroyed.
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("config.json"), "stale").unwrap();
        migrate_dir(&old, &new);
        assert_eq!(
            std::fs::read_to_string(new.join("config.json")).unwrap(),
            r#"{"refresh_minutes":7}"#
        );
        assert!(old.exists(), "a second migration is a no-op, not a delete");

        // A missing old directory is the normal case on a fresh install.
        std::fs::remove_dir_all(&old).unwrap();
        migrate_dir(&old, &new);
        assert!(new.join("config.json").is_file());

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The failure the directory-existence gate used to make permanent: the new directory is
    /// already there (`history.save()` creates it within the first refresh) but the config
    /// never arrived. The migration has to still run, and it has to survive the rename
    /// failing, which is why an open handle inside the old directory is held here.
    #[test]
    fn a_failed_migration_is_retried_and_falls_back_to_copying() {
        let root = std::env::temp_dir().join(format!("agentsbar-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (old, new) = (root.join("AgentBar"), root.join("AgentsBar"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("config.json"), r#"{"refresh_minutes":9}"#).unwrap();
        std::fs::write(old.join("history.json"), "{}").unwrap();
        // The new directory already exists and already holds a history, exactly as it does
        // one refresh after a failed rename. Only config.json is missing.
        std::fs::write(new.join("history.json"), r#"{"kept":true}"#).unwrap();

        let held = std::fs::File::open(old.join("config.json")).unwrap();
        migrate_dir(&old, &new);
        drop(held);

        assert_eq!(
            std::fs::read_to_string(new.join("config.json")).unwrap(),
            r#"{"refresh_minutes":9}"#,
            "the config arrives whether the rename worked or the copy fallback ran"
        );
        assert_eq!(
            std::fs::read_to_string(new.join("history.json")).unwrap(),
            r#"{"kept":true}"#,
            "a file already in the new directory is never overwritten"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The test above proves the mechanic with synthetic files. This one proves it against
    /// a real wave 4 directory: point `AGENTSBAR_MIGRATION_FIXTURE` at a **copy** of an
    /// `%APPDATA%\AgentBar` and it migrates that copy, then reloads both files through the
    /// same parsers the app uses, so "the bytes moved" and "the app can still read them"
    /// are separate assertions. Skipped when the variable is unset, so a normal `cargo test`
    /// stays hermetic. Nothing from the fixture is ever printed: it holds wrapped API keys.
    #[test]
    fn a_real_legacy_directory_survives_the_move_and_still_parses() {
        let Ok(fixture) = std::env::var("AGENTSBAR_MIGRATION_FIXTURE") else {
            return;
        };
        let fixture = Path::new(&fixture);
        let root = std::env::temp_dir().join(format!("agentsbar-fixture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (old, new) = (root.join("AgentBar"), root.join("AgentsBar"));
        std::fs::create_dir_all(&old).unwrap();

        // Flat directory: config.json and history.json, nothing nested.
        let mut before = Vec::new();
        for entry in std::fs::read_dir(fixture).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                let name = path.file_name().unwrap().to_owned();
                let bytes = std::fs::read(&path).unwrap();
                std::fs::write(old.join(&name), &bytes).unwrap();
                before.push((name, bytes));
            }
        }
        assert!(!before.is_empty(), "fixture directory is empty");

        migrate_dir(&old, &new);

        assert!(!old.exists(), "the old directory is moved, not copied");
        for (name, bytes) in &before {
            assert_eq!(
                &std::fs::read(new.join(name)).unwrap(),
                bytes,
                "{} changed during the move",
                name.to_string_lossy()
            );
        }

        // Byte-identical is not the same as usable. Parse both files after the move:
        // Config::parse also unwraps the DPAPI-wrapped secrets, so a broken migration
        // would show up here rather than at the user's next refresh.
        for (name, bytes) in &before {
            let text = String::from_utf8(bytes.clone()).unwrap();
            let moved = std::fs::read_to_string(new.join(name)).unwrap();
            match name.to_string_lossy().as_ref() {
                "config.json" => {
                    let cfg = Config::parse(&moved);
                    assert_eq!(cfg.providers.len(), Config::parse(&text).providers.len());
                    assert!(cfg.refresh_minutes >= 1, "settings survived the move");
                    assert!(!cfg.providers.is_empty(), "provider settings survived");
                }
                "history.json" => {
                    let series = crate::history::History::parse(&moved);
                    assert_eq!(
                        series.series().len(),
                        crate::history::History::parse(&text).series().len(),
                        "sparkline history survived the move"
                    );
                }
                _ => {}
            }
        }

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn parsing_never_panics_and_keeps_defaults() {
        // Corrupt file: defaults, not a panic.
        let cfg = Config::parse("{ not json ");
        assert_eq!(cfg.refresh_minutes, 5);
        assert!(cfg.is_enabled("codex"));
        assert!(!cfg.is_enabled("openrouter"));

        // Partial file: missing keys default, missing providers get filled in.
        let cfg =
            Config::parse(r#"{"refresh_minutes":0,"providers":{"openrouter":{"enabled":true}}}"#);
        assert_eq!(cfg.refresh_minutes, 1);
        assert!(cfg.is_enabled("openrouter"));
        assert!(cfg.provider("codex").is_some());
        assert_eq!(cfg.api_key("openrouter"), None);

        // Round trip through the serialized form.
        let again = Config::parse(&serde_json::to_string(&cfg).unwrap());
        assert_eq!(again.refresh_minutes, cfg.refresh_minutes);
        assert!(again.is_enabled("openrouter"));
    }

    /// Row 24, "fresh installs only". The FIELD level `#[serde(default)]` on
    /// `refresh_adaptive` is the whole migration: delete it and the container default fills
    /// the missing key from `Config::default()`, which is `true`, and every wave 3 install
    /// switches itself to adaptive on the next launch. Nothing else in the suite fails when
    /// that attribute goes, so this is the test that guards it.
    #[test]
    fn an_existing_config_keeps_its_interval_and_a_fresh_install_is_adaptive() {
        assert!(
            !Config::parse(r#"{"refresh_minutes":7}"#).refresh_adaptive,
            "a file written before the key existed must not switch itself to adaptive"
        );
        assert!(Config::parse(r#"{"refresh_adaptive":true}"#).refresh_adaptive);
        assert!(
            Config::default().refresh_adaptive,
            "a fresh install has no file at all"
        );
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
    fn dpapi_round_trips_a_secret() {
        let wrapped = dpapi_wrap("sk-live-0123456789").expect("CryptProtectData");
        let b64 = wrapped
            .strip_prefix(DPAPI_PREFIX)
            .expect("wrapped value carries the prefix");
        assert!(!wrapped.contains("sk-live-0123456789"));
        assert_eq!(dpapi_unwrap(b64).as_deref(), Some("sk-live-0123456789"));

        // Empty and non-ASCII survive the byte round trip too.
        for value in ["", "a=1; b=2", "clé-é\u{1F600}"] {
            let w = dpapi_wrap(value).expect("CryptProtectData");
            let body = w.strip_prefix(DPAPI_PREFIX).unwrap();
            assert_eq!(dpapi_unwrap(body).as_deref(), Some(value));
        }
    }

    #[test]
    fn secrets_are_wrapped_on_disk_and_plaintext_in_memory() {
        let mut stored = Config::default();
        stored.providers.get_mut("openrouter").unwrap().api_key = Some("sk-secret".into());
        stored.providers.get_mut("cursor").unwrap().cookie_header = Some("a=1; b=2".into());

        let on_disk = serde_json::to_string(&stored.wrapped_for_disk()).unwrap();
        assert!(!on_disk.contains("sk-secret"));
        assert!(!on_disk.contains("a=1; b=2"));
        assert!(on_disk.contains(DPAPI_PREFIX));

        // Reading it back gives plaintext again, so every accessor is unaffected.
        let loaded = Config::parse(&on_disk);
        assert_eq!(loaded.api_key("openrouter"), Some("sk-secret"));
        assert_eq!(loaded.cookie_header("cursor"), Some("a=1; b=2"));

        // Redaction still strips both, and leaks no ciphertext either.
        let outbound = serde_json::to_string(&loaded.redacted()).unwrap();
        assert!(!outbound.contains("sk-secret"));
        assert!(!outbound.contains(DPAPI_PREFIX));
    }

    #[test]
    fn plaintext_config_from_an_older_build_loads_and_upgrades() {
        // Exactly the shape a pre-DPAPI build wrote: no prefix anywhere.
        let old = r#"{"refresh_minutes":7,"providers":{
            "openrouter":{"enabled":true,"api_key":"plain-key"},
            "cursor":{"enabled":true,"cookie_source":"manual","cookie_header":" a=1 "}}}"#;
        let loaded = Config::parse(old);
        assert_eq!(loaded.refresh_minutes, 7);
        assert_eq!(loaded.api_key("openrouter"), Some("plain-key"));
        assert_eq!(loaded.cookie_header("cursor"), Some("a=1"));

        // The next save upgrades it in place, and wrapping is not applied twice.
        let once = loaded.wrapped_for_disk();
        let twice = once.wrapped_for_disk();
        assert_eq!(
            once.providers["openrouter"].api_key,
            twice.providers["openrouter"].api_key
        );
        assert_eq!(
            Config::parse(&serde_json::to_string(&twice).unwrap()).api_key("openrouter"),
            Some("plain-key")
        );
    }

    #[test]
    fn unreadable_ciphertext_is_dropped_not_panicked_on() {
        // Not base64, a valid-base64 blob that is not a DPAPI blob (what a config copied
        // from another user or machine looks like), and an empty body.
        for body in ["not base64 !!", "AAAAAAAAAAAAAAAAAAAAAA==", ""] {
            let text = format!(
                r#"{{"providers":{{"openrouter":{{"enabled":true,"api_key":"{DPAPI_PREFIX}{body}"}}}}}}"#
            );
            let cfg = Config::parse(&text);
            assert_eq!(cfg.api_key("openrouter"), None, "body {body:?}");
            assert!(
                cfg.is_enabled("openrouter"),
                "the rest of the config survives"
            );
        }
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
