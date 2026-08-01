//! Browser cookie import for the cookie authenticated providers.
//!
//! Scope is deliberately narrow (SPEC2 section 1):
//!
//! - Firefox: plaintext `moz_cookies.value`, no decryption at all.
//! - Chromium `v10`/`v11`: AES-256-GCM with the DPAPI protected `os_crypt.encrypted_key`
//!   from `Local State`.
//! - Chromium legacy rows: the plaintext `value` column, or a bare DPAPI blob.
//!
//! Chromium `v20` (app bound encryption) rows are SKIPPED, never bypassed. Defeating it
//! needs COM elevation, browser process impersonation or raw NTFS reads, all of which are
//! attacks on a security control. Affected browsers are reported as partially supported
//! through [`BrowserProfile::app_bound`] and the user is pointed at Chrome, Edge, Firefox
//! or a manually pasted cookie header.
//!
//! Cookie values are secrets. Nothing in this module logs, formats or panics with a
//! value: names and counts only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{Connection, OpenFlags};

use crate::config::Config;

/// Windows FILETIME epoch (1601-01-01) expressed as seconds before the unix epoch.
const FILETIME_EPOCH_OFFSET: i64 = 11_644_473_600;
/// Chromium prepends a SHA256 of the domain to the plaintext on newer versions.
const DOMAIN_HASH_LEN: usize = 32;
/// How long a browser scan (or its failure) stays cached.
///
/// ponytail: a flat TTL, not an invalidation graph. Cookie databases change on the order
/// of hours and `is_configured` is called on every popover open, so without this the tray
/// would copy and decrypt four cookie databases per provider per open. Config writes call
/// [`invalidate`] so a settings change is never stale.
const CACHE_TTL: Duration = Duration::from_secs(300);
/// File name prefix of the working copies, also what [`sweep_temp_copies`] looks for.
const TEMP_PREFIX: &str = "agentbar-cookies-";

// ------------------------------------------------------------------ types

#[derive(Debug, Clone)]
pub enum BrowserKind {
    Chromium { local_state: PathBuf },
    Firefox,
}

#[derive(Debug, Clone)]
pub struct BrowserProfile {
    /// Stable id used by `ProviderConfig::cookie_browser`.
    pub browser: &'static str,
    /// Human label, disambiguated when a browser has several profiles.
    pub label: String,
    pub cookies_db: PathBuf,
    pub kind: BrowserKind,
}

#[derive(Debug, Clone)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub expires: Option<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error)]
pub enum CookieError {
    #[error("no supported browser with a cookie database was found")]
    NoBrowser,
    #[error("could not read the cookie database: {0}")]
    Io(String),
    #[error("cookie database query failed: {0}")]
    Db(String),
    #[error("could not unlock {browser}'s cookie encryption key: {reason}")]
    Key { browser: String, reason: String },
    #[error("{0}")]
    Missing(String),
}

/// Result of scanning one profile, used to build actionable error messages.
struct Scan {
    cookies: Vec<Cookie>,
    /// Rows skipped because they are app bound encrypted (v20).
    skipped_app_bound: usize,
}

// ------------------------------------------------------------------ detection

/// Chromium roots under `%LOCALAPPDATA%`, in the order `auto` tries them.
const CHROMIUM_ROOTS: [(&str, &str, &str); 3] = [
    ("chrome", "Chrome", r"Google\Chrome\User Data"),
    ("edge", "Edge", r"Microsoft\Edge\User Data"),
    ("brave", "Brave", r"BraveSoftware\Brave-Browser\User Data"),
];

/// Every installed browser profile that actually has a cookie database, in the order
/// `cookie_source = "auto"` tries them: Chrome, Edge, Brave, Firefox.
pub fn detect_browsers() -> Vec<BrowserProfile> {
    let mut out = Vec::new();
    if let Some(local) = dirs::data_local_dir() {
        for (id, label, rel) in CHROMIUM_ROOTS {
            let root = local.join(rel);
            let local_state = root.join("Local State");
            if !local_state.is_file() {
                continue;
            }
            for (profile_dir, profile_name) in chromium_profiles(&root) {
                // Chrome and Brave moved the database under Network/, Edge did not.
                let db = ["Network/Cookies", "Cookies"]
                    .iter()
                    .map(|rel| profile_dir.join(rel))
                    .find(|p| p.is_file());
                let Some(cookies_db) = db else { continue };
                out.push(BrowserProfile {
                    browser: id,
                    label: decorate(label, &profile_name),
                    cookies_db,
                    kind: BrowserKind::Chromium {
                        local_state: local_state.clone(),
                    },
                });
            }
        }
    }
    if let Some(roaming) = dirs::data_dir() {
        let profiles = roaming.join(r"Mozilla\Firefox\Profiles");
        for (dir, name) in read_subdirs(&profiles) {
            let db = dir.join("cookies.sqlite");
            if db.is_file() {
                out.push(BrowserProfile {
                    browser: "firefox",
                    label: decorate("Firefox", &name),
                    cookies_db: db,
                    kind: BrowserKind::Firefox,
                });
            }
        }
    }
    out
}

fn decorate(label: &str, profile: &str) -> String {
    if profile.eq_ignore_ascii_case("Default") || profile.is_empty() {
        label.to_string()
    } else {
        format!("{label} ({profile})")
    }
}

/// `Default` first, then `Profile 1`, `Profile 2`, ... so the common case is tried first.
fn chromium_profiles(root: &Path) -> Vec<(PathBuf, String)> {
    let mut dirs: Vec<(PathBuf, String)> = read_subdirs(root)
        .into_iter()
        .filter(|(_, name)| name == "Default" || name.starts_with("Profile "))
        .collect();
    dirs.sort_by(|a, b| (a.1 != "Default").cmp(&(b.1 != "Default")).then(a.1.cmp(&b.1)));
    dirs
}

fn read_subdirs(dir: &Path) -> Vec<(PathBuf, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, String)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| (e.path(), e.file_name().to_string_lossy().into_owned()))
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

impl BrowserProfile {
    /// True when the browser writes app bound (v20) cookies, which we do not decrypt.
    pub fn app_bound(&self) -> bool {
        let BrowserKind::Chromium { local_state } = &self.kind else {
            return false;
        };
        std::fs::read_to_string(local_state)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| {
                v.get("os_crypt")?
                    .get("app_bound_encrypted_key")
                    .map(|k| !k.is_null())
            })
            .unwrap_or(false)
    }
}

// ------------------------------------------------------------------ reading

/// Every cookie whose host matches one of `domain_suffixes`, decrypted.
pub fn cookies_for_domain(
    profile: &BrowserProfile,
    domain_suffixes: &[&str],
) -> Result<Vec<Cookie>, CookieError> {
    Ok(scan(profile, domain_suffixes)?.cookies)
}

fn scan(profile: &BrowserProfile, domains: &[&str]) -> Result<Scan, CookieError> {
    let copy = TempCopy::of(&profile.cookies_db)?;
    // Our private copy, so plain read/write flags: a WAL sidecar has to be replayed to
    // see recent sign-ins, and SQLITE_OPEN_READ_ONLY refuses to do that.
    let conn = Connection::open_with_flags(
        &copy.path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| CookieError::Db(e.to_string()))?;

    match &profile.kind {
        BrowserKind::Firefox => read_firefox(&conn, domains),
        BrowserKind::Chromium { local_state } => {
            let key = chromium_key(local_state).map_err(|reason| CookieError::Key {
                browser: profile.label.clone(),
                reason,
            })?;
            read_chromium(&conn, domains, &key)
        }
    }
}

fn read_firefox(conn: &Connection, domains: &[&str]) -> Result<Scan, CookieError> {
    let mut stmt = conn
        .prepare("SELECT host, name, value, expiry FROM moz_cookies")
        .map_err(|e| CookieError::Db(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3).unwrap_or(0),
            ))
        })
        .map_err(|e| CookieError::Db(e.to_string()))?;
    let mut cookies = Vec::new();
    for row in rows.flatten() {
        let (host, name, value, expiry) = row;
        if !host_matches(&host, domains) || value.is_empty() {
            continue;
        }
        cookies.push(Cookie {
            name,
            value,
            domain: host,
            expires: unix_to_utc(expiry),
        });
    }
    Ok(Scan {
        cookies,
        skipped_app_bound: 0,
    })
}

fn read_chromium(conn: &Connection, domains: &[&str], key: &[u8; 32]) -> Result<Scan, CookieError> {
    let mut stmt = conn
        .prepare("SELECT host_key, name, value, encrypted_value, expires_utc FROM cookies")
        .map_err(|e| CookieError::Db(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2).unwrap_or_default(),
                r.get::<_, Vec<u8>>(3).unwrap_or_default(),
                r.get::<_, i64>(4).unwrap_or(0),
            ))
        })
        .map_err(|e| CookieError::Db(e.to_string()))?;

    let mut cookies = Vec::new();
    let mut skipped_app_bound = 0usize;
    for row in rows.flatten() {
        let (host, name, plain, encrypted, expires_utc) = row;
        if !host_matches(&host, domains) {
            continue;
        }
        if is_app_bound(&encrypted) {
            skipped_app_bound += 1;
            continue;
        }
        let Some(value) = decrypt_chromium(&encrypted, &plain, key) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        cookies.push(Cookie {
            name,
            value,
            domain: host,
            expires: filetime_to_utc(expires_utc),
        });
    }
    Ok(Scan {
        cookies,
        skipped_app_bound,
    })
}

fn is_app_bound(encrypted: &[u8]) -> bool {
    encrypted.starts_with(b"v20")
}

/// `None` means "could not read this row", never a decrypted-to-empty value.
fn decrypt_chromium(encrypted: &[u8], plain: &str, key: &[u8; 32]) -> Option<String> {
    if encrypted.is_empty() {
        return (!plain.is_empty()).then(|| plain.to_string());
    }
    if encrypted.starts_with(b"v10") || encrypted.starts_with(b"v11") {
        return aes_gcm_decrypt(encrypted, key).map(text_of);
    }
    // Pre-v10 rows: either stored in the clear or a bare DPAPI blob.
    if let Ok(text) = std::str::from_utf8(encrypted) {
        return Some(text.to_string());
    }
    Some(text_of(dpapi_unprotect(encrypted).ok()?))
}

/// nonce = bytes 3..15, ciphertext = the rest with the 16 byte tag appended.
fn aes_gcm_decrypt(blob: &[u8], key: &[u8; 32]) -> Option<Vec<u8>> {
    if blob.len() < 3 + 12 + 16 {
        return None;
    }
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let nonce = Nonce::from_slice(&blob[3..15]);
    cipher.decrypt(nonce, &blob[15..]).ok()
}

fn text_of(plain: Vec<u8>) -> String {
    String::from_utf8_lossy(strip_domain_hash(&plain)).into_owned()
}

/// Newer Chromium prepends a 32 byte SHA256 of the domain to the plaintext. That hash is
/// binary, so drop it only when the leading 32 bytes are not valid UTF-8 on their own.
fn strip_domain_hash(plain: &[u8]) -> &[u8] {
    if plain.len() > DOMAIN_HASH_LEN && std::str::from_utf8(&plain[..DOMAIN_HASH_LEN]).is_err() {
        return &plain[DOMAIN_HASH_LEN..];
    }
    plain
}

/// `.cursor.com` and `cursor.com` both match the suffix `cursor.com`.
fn host_matches(host: &str, suffixes: &[&str]) -> bool {
    let host = host.trim_start_matches('.').to_ascii_lowercase();
    suffixes.iter().any(|s| {
        let s = s.trim_start_matches('.').to_ascii_lowercase();
        host == s || host.ends_with(&format!(".{s}"))
    })
}

fn filetime_to_utc(micros: i64) -> Option<DateTime<Utc>> {
    unix_to_utc(micros.checked_div(1_000_000)?.checked_sub(FILETIME_EPOCH_OFFSET)?)
}

fn unix_to_utc(secs: i64) -> Option<DateTime<Utc>> {
    if secs <= 0 {
        return None; // session cookie
    }
    Utc.timestamp_opt(secs, 0).single()
}

// ------------------------------------------------------------------ key material

/// `os_crypt.encrypted_key` is base64, prefixed with the literal `DPAPI`, and unwraps to
/// the 32 byte AES key. The key itself is never logged.
fn chromium_key(local_state: &Path) -> Result<[u8; 32], String> {
    let text = std::fs::read_to_string(local_state).map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let b64 = json
        .get("os_crypt")
        .and_then(|o| o.get("encrypted_key"))
        .and_then(|k| k.as_str())
        .ok_or("Local State has no os_crypt.encrypted_key")?;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| e.to_string())?;
    let blob = blob
        .strip_prefix(b"DPAPI".as_slice())
        .ok_or("encrypted_key is not DPAPI wrapped")?;
    let key = dpapi_unprotect(blob)?;
    key.try_into()
        .map_err(|_| "unwrapped key is not 32 bytes".to_string())
}

/// `CryptUnprotectData` bound to the current user. No prompt, no UI.
fn dpapi_unprotect(blob: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(blob.len()).map_err(|_| "blob too large".to_string())?,
        pbData: blob.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(&input, None, None, None, None, 0, &mut output)
            .map_err(|e| e.message())?;
        let out = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(windows::Win32::Foundation::HLOCAL(
            output.pbData as *mut _,
        )));
        Ok(out)
    }
}

// ------------------------------------------------------------------ temp copy

/// The browser holds a lock on the live database, so work on a copy and delete it again.
///
/// The copy is a byte for byte copy of the browser's own file, so a Chromium copy holds
/// DPAPI protected values but a **Firefox copy holds cleartext cookie values**, because
/// Firefox stores them that way. Nothing decrypted is ever written back to it. It lives
/// in the per-user `%TEMP%` (ACL'd to this user) and is deleted in [`Drop`], but Drop does
/// not run on a panic (release builds abort), so [`sweep_temp_copies`] clears anything a
/// killed run left behind at the next start.
struct TempCopy {
    path: PathBuf,
}

/// Delete cookie database copies an earlier run left behind.
///
/// Called once at startup, before any scan. AgentBar is single instance, so nothing else
/// owns a file with this prefix at that moment.
pub fn sweep_temp_copies() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX)
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    if removed > 0 {
        log::info!("removed {removed} leftover cookie database cop(ies) from the temp folder");
    }
}

impl TempCopy {
    fn of(src: &Path) -> Result<Self, CookieError> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("{TEMP_PREFIX}{}-{n}.db", std::process::id()));
        // Deliberately not std::fs::copy: that is CopyFileExW, which opens the source
        // denying concurrent writers, so it fails with a sharing violation while the
        // browser is running. A plain File::open shares read, write and delete, which is
        // what the browser's own handle allows.
        let copy = Self { path };
        share_copy(src, &copy.path).map_err(|e| CookieError::Io(describe_open_failure(&e)))?;
        // A WAL sidecar can hold the newest rows, so it has to come along.
        for ext in ["-wal", "-shm"] {
            let from = sidecar(src, ext);
            if from.is_file() {
                let _ = share_copy(&from, &sidecar(&copy.path, ext));
            }
        }
        Ok(copy)
    }
}

fn share_copy(src: &Path, dst: &Path) -> std::io::Result<()> {
    let mut from = std::fs::File::open(src)?;
    let mut to = std::fs::File::create(dst)?;
    std::io::copy(&mut from, &mut to)?;
    Ok(())
}

/// ERROR_SHARING_VIOLATION, the one failure a user can actually do something about.
const ERROR_SHARING_VIOLATION: i32 = 32;

fn describe_open_failure(e: &std::io::Error) -> String {
    if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) {
        return "the browser is running and holds its cookie database open exclusively. \
                Close the browser and try again, or use another browser, or paste a \
                cookie header in Settings"
            .to_string();
    }
    e.to_string()
}

/// Why a profile cannot be read fully, if at all. `None` means fully supported.
///
/// Both limitations here are browser hardening against cookie theft, and both are
/// respected rather than defeated: bypassing them needs COM elevation or raw NTFS reads.
pub fn limitation(profile: &BrowserProfile) -> Option<String> {
    let name = profile.label.split(" (").next().unwrap_or(&profile.label);
    if let Err(e) = std::fs::File::open(&profile.cookies_db) {
        if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) {
            return Some(format!(
                "{name} is running and holds its cookie database open exclusively, so \
                 AgentBar cannot read it. Close {name}, or use another browser, or paste \
                 a cookie header in Settings."
            ));
        }
        return Some(format!("{name}'s cookie database cannot be opened: {e}"));
    }
    if profile.app_bound() {
        return Some(format!(
            "{name} encrypts newer cookies with app-bound encryption (v20). AgentBar does \
             not bypass that security control, so only older cookies can be read. Use \
             Chrome, Edge or Firefox, or paste a cookie header in Settings."
        ));
    }
    None
}

fn sidecar(base: &Path, ext: &str) -> PathBuf {
    let mut s = base.as_os_str().to_os_string();
    s.push(ext);
    PathBuf::from(s)
}

impl Drop for TempCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        for ext in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(sidecar(&self.path, ext));
        }
    }
}

// ------------------------------------------------------------------ config driven resolve

/// What a provider needs out of one profile's jar for the domains it named.
#[derive(Debug, Clone, Copy)]
pub enum Want<'a> {
    /// Every one of these names. Fails naming the ones that are missing.
    All(&'a [&'a str]),
    /// Every one of these names that is present, at least one required. This is what a
    /// provider with several candidate session cookie names wants.
    Any(&'a [&'a str]),
    /// The whole domain jar, the way the browser itself would send it, gated on at least
    /// one of these names being present. An empty list accepts any non-empty jar. The
    /// CodexBar importers for Cursor, Factory, T3 Chat and Augment all work this way, and
    /// the extra cookies are what Vercel and Cloudflare bot mitigation reads.
    Jar(&'a [&'a str]),
}

/// One scan per (profile, domain set), shared by every provider and every candidate name.
///
/// This is what keeps `is_configured` cheap. Ten cookie providers probing twenty odd
/// candidate names cost one copy-and-decrypt per browser profile per domain set, not one
/// each. The cached jar holds decrypted values in memory, exactly as the resolved header
/// it replaced did, and never reaches the disk.
type Cached = Result<Arc<Scan>, String>;

static SCANS: Mutex<Option<HashMap<String, (Instant, Cached)>>> = Mutex::new(None);
/// Detection walks the filesystem, and `is_configured` runs for every provider on every
/// popover open, so the profile list rides the same TTL.
static PROFILES: Mutex<Option<(Instant, Vec<BrowserProfile>)>> = Mutex::new(None);

/// Drop the memoized scans and the detected profile list. Called whenever the cookie
/// config changes.
pub fn invalidate() {
    if let Ok(mut c) = SCANS.lock() {
        *c = None;
    }
    if let Ok(mut p) = PROFILES.lock() {
        *p = None;
    }
}

fn detected() -> Vec<BrowserProfile> {
    let Ok(mut guard) = PROFILES.lock() else {
        return detect_browsers();
    };
    if let Some((at, profiles)) = guard.as_ref() {
        if at.elapsed() <= CACHE_TTL {
            return profiles.clone();
        }
    }
    let fresh = detect_browsers();
    *guard = Some((Instant::now(), fresh.clone()));
    fresh
}

/// ponytail: the lock is held across the scan, so two providers racing the same profile
/// wait instead of copying the same database twice. Scans are hundreds of milliseconds at
/// worst and only on a cold cache.
fn scan_cached(profile: &BrowserProfile, domains: &[&str]) -> Cached {
    let key = format!("{}|{}", profile.cookies_db.display(), domains.join(","));
    let Ok(mut guard) = SCANS.lock() else {
        return scan(profile, domains).map(Arc::new).map_err(|e| e.to_string());
    };
    let map = guard.get_or_insert_with(HashMap::new);
    if let Some((at, value)) = map.get(&key) {
        if at.elapsed() <= CACHE_TTL {
            return value.clone();
        }
    }
    let value = scan(profile, domains).map(Arc::new).map_err(|e| e.to_string());
    map.insert(key, (Instant::now(), value.clone()));
    value
}

/// Why this profile cannot satisfy the request, naming cookie NAMES only.
fn no_match(found: &Scan, label: &str, domains: &[&str], what: String) -> String {
    let extra = if found.skipped_app_bound > 0 {
        format!(
            " ({} cookie(s) there are app bound encrypted (v20) and cannot be read)",
            found.skipped_app_bound
        )
    } else {
        String::new()
    };
    format!("{label} has {what} for {}{extra}", domains.join(", "))
}

/// The `name=value` pairs `want` asks for, or a message naming what is missing.
fn pairs_for(found: &Scan, want: Want, label: &str, domains: &[&str]) -> Result<Vec<String>, String> {
    let now = Utc::now();
    let unexpired = |c: &&Cookie| c.expires.map_or(true, |e| e > now);
    let live = |name: &str| {
        found
            .cookies
            .iter()
            .filter(|c| c.name == name)
            .find(unexpired)
    };
    let pair = |c: &Cookie| format!("{}={}", c.name, c.value);

    match want {
        Want::All(names) => {
            let mut parts = Vec::with_capacity(names.len());
            let mut absent = Vec::new();
            for name in names {
                match live(name) {
                    Some(c) => parts.push(pair(c)),
                    None => absent.push(*name),
                }
            }
            if !absent.is_empty() {
                return Err(no_match(
                    found,
                    label,
                    domains,
                    format!("no {} cookie", absent.join(", ")),
                ));
            }
            Ok(parts)
        }
        Want::Any(names) => {
            let parts: Vec<String> = names.iter().filter_map(|n| live(n)).map(pair).collect();
            if parts.is_empty() {
                return Err(no_match(
                    found,
                    label,
                    domains,
                    format!("none of the {} cookies", names.join(", ")),
                ));
            }
            Ok(parts)
        }
        Want::Jar(require) => {
            let jar: Vec<&Cookie> = found.cookies.iter().filter(unexpired).collect();
            let usable = !jar.is_empty()
                && (require.is_empty() || jar.iter().any(|c| require.contains(&c.name.as_str())));
            if !usable {
                return Err(no_match(
                    found,
                    label,
                    domains,
                    "no session cookie".to_string(),
                ));
            }
            Ok(jar.into_iter().map(pair).collect())
        }
    }
}

/// The `Cookie` header for `provider_id`, honouring its configured source.
///
/// - `off`: always an error.
/// - `manual`: the stored header verbatim, and only then. A header left over from an
///   earlier manual setup must never shadow the browser once the user switches to auto.
/// - `auto`: the pinned browser, else every detected browser in order Chrome, Edge,
///   Brave, Firefox, taking the first that satisfies `want`.
///
/// The returned string is a secret: put it straight into a request header.
pub fn resolve(
    config: &Config,
    provider_id: &str,
    domains: &[&str],
    want: Want,
) -> Result<String, CookieError> {
    match config.cookie_source(provider_id) {
        "off" => {
            return Err(CookieError::Missing(format!(
                "{provider_id} cookie import is turned off in Settings"
            )))
        }
        "manual" => {
            return config
                .cookie_header(provider_id)
                .map(str::to_string)
                .ok_or_else(|| {
                    CookieError::Missing(format!(
                        "{provider_id} is set to a manual cookie header but none is saved"
                    ))
                })
        }
        _ => {}
    }

    let wanted = config.cookie_browser(provider_id);
    let all = detected();
    if all.is_empty() {
        return Err(CookieError::NoBrowser);
    }
    // A pinned browser is used exclusively, so the failure names that browser.
    let candidates: Vec<&BrowserProfile> = match wanted {
        Some(id) => all.iter().filter(|p| p.browser == id).collect(),
        None => all.iter().collect(),
    };
    if candidates.is_empty() {
        return Err(CookieError::Missing(format!(
            "{} is selected for {provider_id} but is not installed",
            wanted.unwrap_or("that browser")
        )));
    }

    let mut last = None;
    for profile in candidates {
        let attempt = scan_cached(profile, domains)
            .and_then(|found| pairs_for(&found, want, &profile.label, domains));
        match attempt {
            Ok(parts) => {
                log::info!(
                    "{provider_id}: using {} cookie(s) from {}",
                    parts.len(),
                    profile.label
                );
                return Ok(parts.join("; "));
            }
            // Both a failed scan and an unsatisfied `want` already carry a message that
            // names browsers and cookie names only.
            Err(message) => last = Some(CookieError::Missing(message)),
        }
    }
    Err(last.unwrap_or(CookieError::NoBrowser))
}

/// Cheap enough for `is_configured`: everything but the first scan is memoized.
pub fn available(config: &Config, provider_id: &str, domains: &[&str], want: Want) -> bool {
    resolve(config, provider_id, domains, want).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::aead::Payload;

    /// Synthetic blob in Chromium's on-disk shape. Never a real cookie value.
    fn seal(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8], tag: &[u8; 3]) -> Vec<u8> {
        let cipher = Aes256Gcm::new_from_slice(key).unwrap();
        let ct = cipher
            .encrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: plaintext,
                    aad: b"",
                },
            )
            .unwrap();
        let mut out = tag.to_vec();
        out.extend_from_slice(nonce);
        out.extend_from_slice(&ct);
        out
    }

    #[test]
    fn v10_and_v11_round_trip() {
        let key = [7u8; 32];
        let nonce = [3u8; 12];
        for tag in [b"v10", b"v11"] {
            let blob = seal(&key, &nonce, b"synthetic-value", tag);
            assert_eq!(
                decrypt_chromium(&blob, "", &key).as_deref(),
                Some("synthetic-value")
            );
        }
    }

    #[test]
    fn v20_rows_are_skipped_never_bypassed() {
        let key = [7u8; 32];
        let blob = seal(&key, &[3u8; 12], b"synthetic-value", b"v20");
        assert!(is_app_bound(&blob));
        // Even asked directly, the v10 path must not produce a value for a v20 blob.
        assert_eq!(decrypt_chromium(&blob, "", &key), None);
    }

    #[test]
    fn domain_hash_prefix_is_stripped_only_when_binary() {
        let mut prefixed = vec![0u8; DOMAIN_HASH_LEN];
        prefixed[0] = 0xF0; // not a valid UTF-8 lead for a 1 byte char boundary
        prefixed.extend_from_slice(b"synthetic-value");
        let key = [7u8; 32];
        let blob = seal(&key, &[3u8; 12], &prefixed, b"v10");
        assert_eq!(
            decrypt_chromium(&blob, "", &key).as_deref(),
            Some("synthetic-value")
        );

        // A long plain ASCII value keeps all of its bytes.
        let long = "a".repeat(80);
        let blob = seal(&key, &[3u8; 12], long.as_bytes(), b"v10");
        assert_eq!(decrypt_chromium(&blob, "", &key).as_deref(), Some(&*long));
    }

    #[test]
    fn legacy_rows_use_the_plain_column() {
        let key = [7u8; 32];
        assert_eq!(
            decrypt_chromium(&[], "legacy", &key).as_deref(),
            Some("legacy")
        );
        assert_eq!(decrypt_chromium(&[], "", &key), None);
        // Garbage that is not a known prefix and not DPAPI decodable is dropped.
        assert_eq!(decrypt_chromium(&[0xFF, 0xFE, 0x00], "", &key), None);
    }

    #[test]
    fn wrong_key_yields_nothing_rather_than_garbage() {
        let blob = seal(&[7u8; 32], &[3u8; 12], b"synthetic-value", b"v10");
        assert_eq!(decrypt_chromium(&blob, "", &[9u8; 32]), None);
        assert_eq!(decrypt_chromium(b"v10short", "", &[9u8; 32]), None);
    }

    #[test]
    fn host_suffix_matching() {
        assert!(host_matches(".cursor.com", &["cursor.com"]));
        assert!(host_matches("cursor.com", &["cursor.com"]));
        assert!(host_matches("www.cursor.com", &["cursor.com"]));
        assert!(!host_matches("notcursor.com", &["cursor.com"]));
        assert!(!host_matches("cursor.com.evil.io", &["cursor.com"]));
        assert!(host_matches("api.factory.ai", &["app.devin.ai", "factory.ai"]));
    }

    #[test]
    fn expiry_conversion() {
        assert_eq!(unix_to_utc(0), None);
        assert_eq!(filetime_to_utc(0), None);
        // 1970-01-01 in FILETIME microseconds is exactly the epoch offset.
        assert_eq!(filetime_to_utc(FILETIME_EPOCH_OFFSET * 1_000_000), None);
        let one = filetime_to_utc((FILETIME_EPOCH_OFFSET + 1) * 1_000_000).unwrap();
        assert_eq!(one.timestamp(), 1);
    }

    /// Synthetic jar, never a real cookie value.
    fn jar(names: &[&str]) -> Scan {
        Scan {
            cookies: names
                .iter()
                .map(|n| Cookie {
                    name: (*n).to_string(),
                    value: "synthetic".into(),
                    domain: ".example.com".into(),
                    expires: None,
                })
                .collect(),
            skipped_app_bound: 0,
        }
    }

    fn header(found: &Scan, want: Want) -> Result<String, String> {
        pairs_for(found, want, "Chrome", &["example.com"]).map(|p| p.join("; "))
    }

    #[test]
    fn one_scan_answers_all_three_shapes_of_request() {
        let found = jar(&["session", "__Host-auth", "ajs_anonymous_id"]);

        // All: every name or nothing, and the failure names only what is missing.
        assert_eq!(
            header(&found, Want::All(&["session"])).as_deref(),
            Ok("session=synthetic")
        );
        let err = header(&found, Want::All(&["session", "wos-session"])).unwrap_err();
        assert!(err.contains("no wos-session cookie"), "{err}");
        assert!(!err.contains("synthetic"), "a value leaked into an error");

        // Any: every candidate that is present, at least one required.
        assert_eq!(
            header(&found, Want::Any(&["__Host-auth", "auth"])).as_deref(),
            Ok("__Host-auth=synthetic")
        );
        assert!(header(&found, Want::Any(&["auth"])).is_err());

        // Jar: the whole thing, but only when a session name is in it.
        assert_eq!(
            header(&found, Want::Jar(&["session"])).as_deref(),
            Ok("session=synthetic; __Host-auth=synthetic; ajs_anonymous_id=synthetic")
        );
        assert!(header(&jar(&["ajs_anonymous_id"]), Want::Jar(&["session"])).is_err());
        // An empty gate accepts any non-empty jar, and nothing accepts an empty one.
        assert!(header(&jar(&["ajs_anonymous_id"]), Want::Jar(&[])).is_ok());
        assert!(header(&jar(&[]), Want::Jar(&[])).is_err());
    }

    #[test]
    fn expired_rows_never_reach_a_header() {
        let past = Utc::now() - chrono::Duration::hours(1);
        let future = Utc::now() + chrono::Duration::hours(1);
        let mut found = jar(&["session", "stale"]);
        found.cookies[0].expires = Some(future);
        found.cookies[1].expires = Some(past);

        assert_eq!(
            header(&found, Want::Jar(&["session"])).as_deref(),
            Ok("session=synthetic")
        );
        found.cookies[0].expires = Some(past);
        assert!(header(&found, Want::Jar(&["session"])).is_err());
        assert!(header(&found, Want::All(&["session"])).is_err());
        assert!(header(&found, Want::Any(&["session"])).is_err());
    }

    /// A header pasted while the provider was on `manual` must not keep answering once
    /// the user switches back to `auto` and signs in again in the browser.
    #[test]
    fn a_saved_manual_header_never_answers_in_auto_or_off_mode() {
        // Pretend no browser is installed, so the auto branch is deterministic and this
        // test never reads the real cookie databases.
        *PROFILES.lock().unwrap() = Some((Instant::now(), Vec::new()));

        let mut config = Config::default();
        let cursor = config.providers.get_mut("cursor").unwrap();
        cursor.cookie_source = "manual".to_string();
        cursor.cookie_header = Some("stale=1".to_string());
        let ask = |config: &Config| resolve(config, "cursor", &["cursor.com"], Want::Jar(&[]));

        assert_eq!(ask(&config).unwrap(), "stale=1");

        config.providers.get_mut("cursor").unwrap().cookie_source = "auto".to_string();
        assert!(
            matches!(ask(&config), Err(CookieError::NoBrowser)),
            "auto mode answered from the saved manual header"
        );

        config.providers.get_mut("cursor").unwrap().cookie_source = "off".to_string();
        assert!(ask(&config).is_err());

        invalidate();
    }

    #[test]
    fn an_app_bound_browser_says_so_instead_of_just_failing() {
        let mut found = jar(&["ajs_anonymous_id"]);
        found.skipped_app_bound = 3;
        let err = header(&found, Want::Jar(&["session"])).unwrap_err();
        assert!(err.contains("3 cookie(s)") && err.contains("v20"), "{err}");
    }

    /// What one cold popover open costs: `is_configured` for every provider, which is
    /// every cookie provider probing its own domains against every detected browser.
    /// Prints timings only, never a cookie.
    /// Run with: cargo test -- --ignored --nocapture cold_probe
    #[test]
    #[ignore = "touches the real browser profiles on this machine"]
    fn cold_probe_of_every_provider_stays_quick() {
        let config = Config::default();
        for round in ["cold", "warm"] {
            if round == "cold" {
                invalidate();
            }
            let at = Instant::now();
            let configured = crate::providers::all_providers()
                .iter()
                .filter(|p| p.is_configured(&config))
                .count();
            println!(
                "{round}: {} provider(s) probed in {:?}, {configured} configured",
                crate::providers::all_providers().len(),
                at.elapsed()
            );
        }
    }

    /// Proof against this machine's real browsers. Prints browser names, cookie NAMES and
    /// counts only. Never a value, never a length that could leak one.
    /// Run with: cargo test -- --ignored --nocapture
    #[test]
    #[ignore = "touches the real browser profiles on this machine"]
    fn real_browsers_decrypt() {
        // Every TLD in use here, so the sweep proves decryption even for a browser that
        // holds no cookie for the provider domains below.
        const EVERYTHING: [&str; 8] = ["com", "org", "net", "ai", "io", "dev", "chat", "im"];
        // The wave 2 provider domains, printed by name so a porter can see what is there.
        const PROVIDERS: [&str; 10] = [
            "cursor.com",
            "factory.ai",
            "app.devin.ai",
            "t3.chat",
            "opencode.ai",
            "manus.im",
            "app.warp.dev",
            "windsurf.com",
            "app.augmentcode.com",
            "ampcode.com",
        ];

        let profiles = detect_browsers();
        assert!(!profiles.is_empty(), "no browser profile detected");
        let mut decrypted_any = false;
        for p in &profiles {
            let kind = match p.kind {
                BrowserKind::Chromium { .. } => "chromium",
                BrowserKind::Firefox => "firefox",
            };
            let all = match scan(p, &EVERYTHING) {
                Ok(found) => found,
                Err(e) => {
                    println!("{} [{kind}] UNREADABLE: {e}", p.label);
                    println!("    limitation: {:?}", limitation(p));
                    continue;
                }
            };
            println!(
                "{} [{kind}, app_bound={}] decrypted {} cookie(s), skipped {} app-bound (v20) row(s)",
                p.label,
                p.app_bound(),
                all.cookies.len(),
                all.skipped_app_bound
            );
            // A decrypt that silently produced empty strings would look like success.
            assert!(
                all.cookies.iter().all(|c| !c.value.is_empty()),
                "{}: decrypted an empty value",
                p.label
            );
            decrypted_any |= !all.cookies.is_empty();

            let found = scan(p, &PROVIDERS).unwrap();
            let mut names: Vec<&str> = found.cookies.iter().map(|c| c.name.as_str()).collect();
            names.sort_unstable();
            names.dedup();
            println!(
                "    wave 2 provider cookies: {} name(s){}",
                names.len(),
                if names.is_empty() {
                    String::new()
                } else {
                    format!(": {}", names.join(", "))
                }
            );
        }
        assert!(decrypted_any, "no browser yielded a single decrypted cookie");
    }
}
