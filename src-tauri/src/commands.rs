use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::config::{Config, COOKIE_BROWSERS, COOKIE_SOURCES};
use crate::history::Sample;
use crate::providers::{all_providers, AuthKind, ProviderInfo, UsageSnapshot, UsageWindow};
use crate::state::{AppState, DisplaySnapshot};

#[tauri::command]
pub async fn list_providers(state: State<'_, AppState>) -> Result<Vec<ProviderInfo>, String> {
    let config = state.config.read().await.clone();
    // `is_configured` reads cookie databases with blocking file IO on a cold cache, so it
    // does not belong on a runtime worker thread.
    tauri::async_runtime::spawn_blocking(move || {
        all_providers()
            .iter()
            .map(|p| ProviderInfo {
                id: p.id(),
                name: p.name(),
                auth: p.auth_kind(),
                configured: p.is_configured(&config),
                doc_url: p.doc_url(),
                env_key: p.env_key(),
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_snapshots(state: State<'_, AppState>) -> Result<Vec<DisplaySnapshot>, String> {
    // Row 25: the same clamped lanes the tray reads, so the tile cannot disagree with it.
    // Must stay the same shape as the `usage-updated` payload in `scheduler::publish`.
    Ok(state
        .snapshots_in_order()
        .await
        .iter()
        .map(DisplaySnapshot::from)
        .collect())
}

#[tauri::command]
pub async fn refresh_all(app: AppHandle) -> Result<(), String> {
    crate::scheduler::refresh_now(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn refresh_provider(app: AppHandle, id: String) -> Result<(), String> {
    crate::scheduler::refresh_one(&app, &id).await;
    Ok(())
}

#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> Result<Config, String> {
    // API keys never leave the backend: the UI only needs ProviderInfo::configured.
    Ok(state.config.read().await.redacted())
}

/// Row 20 and row 24. The longest gap the ACTIVE cadence policy can leave between two
/// batches, in seconds, for the frontend's staleness threshold.
///
/// `refresh_minutes` is the wrong number to size that from: under adaptive it is only the
/// fixed interval sitting underneath, so an idle machine batching every 30 minutes would
/// render "29m ago" on every tile from ten minutes after each batch onward. `tray::is_stale`
/// already reads this same function, and the tray and the tiles must not disagree about
/// what counts as old.
#[tauri::command]
pub async fn get_cadence_secs(state: State<'_, AppState>) -> Result<i64, String> {
    let config = state.config.read().await;
    Ok(crate::scheduler::max_cadence_secs(&config))
}

#[tauri::command]
pub async fn set_config(app: AppHandle, mut config: Config) -> Result<(), String> {
    config.normalize();
    let launch_at_startup = config.launch_at_startup;
    {
        let state = app.state::<AppState>();
        let mut current = state.config.write().await;
        // The incoming copy is redacted and may be stale, so keys stay as stored.
        config.merge_keys_from(&current);
        config.save().map_err(|e| e.to_string())?;
        *current = config;
    }
    // A changed cookie source or browser must not be answered from the memo.
    crate::cookies::invalidate();
    crate::tray::apply_autostart(&app, launch_at_startup);
    // Refreshing also restarts the scheduler countdown, so a new interval applies now.
    crate::scheduler::refresh_now(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn set_api_key(app: AppHandle, id: String, key: String) -> Result<(), String> {
    let key = key.trim().to_string();
    {
        let state = app.state::<AppState>();
        let mut config = state.config.write().await;
        let entry = config.providers.entry(id.clone()).or_default();
        entry.enabled = !key.is_empty();
        entry.api_key = if key.is_empty() { None } else { Some(key) };
        config.save().map_err(|e| e.to_string())?;
    }
    crate::scheduler::refresh_one(&app, &id).await;
    Ok(())
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}

// ------------------------------------------------------------------ cookies

#[derive(Debug, Serialize)]
pub struct BrowserInfo {
    /// One of `COOKIE_BROWSERS`, matching `ProviderConfig::cookie_browser`.
    pub id: &'static str,
    pub label: String,
    /// False when the browser writes app bound (v20) cookies we deliberately do not read.
    pub supported: bool,
    /// Why it is limited, when it is. `None` means fully supported.
    pub note: Option<String>,
}

/// Detected browsers with a cookie database, one row per browser (not per profile),
/// in the order `cookie_source = "auto"` tries them.
#[tauri::command]
pub fn list_browsers() -> Vec<BrowserInfo> {
    let mut out: Vec<BrowserInfo> = Vec::new();
    for profile in crate::cookies::detect_browsers() {
        let note = crate::cookies::limitation(&profile);
        match out.iter_mut().find(|b| b.id == profile.browser) {
            // A second profile of the same browser: the browser counts as usable if any
            // one of its profiles is.
            Some(existing) if note.is_none() => {
                existing.supported = true;
                existing.note = None;
            }
            Some(_) => {}
            None => out.push(BrowserInfo {
                id: profile.browser,
                label: base_label(&profile.label).to_string(),
                supported: note.is_none(),
                note,
            }),
        }
    }
    out
}

/// "Chrome (Profile 1)" back to "Chrome".
fn base_label(label: &str) -> &str {
    label.split(" (").next().unwrap_or(label).trim()
}

#[tauri::command]
pub async fn set_cookie_source(
    app: AppHandle,
    id: String,
    source: String,
    browser: Option<String>,
) -> Result<(), String> {
    if !COOKIE_SOURCES.contains(&source.as_str()) {
        return Err(format!("unknown cookie source {source:?}"));
    }
    if let Some(b) = browser.as_deref() {
        if !COOKIE_BROWSERS.contains(&b) {
            return Err(format!("unknown browser {b:?}"));
        }
    }
    {
        let state = app.state::<AppState>();
        let mut config = state.config.write().await;
        let entry = config.providers.entry(id.clone()).or_default();
        entry.cookie_source = source.clone();
        entry.cookie_browser = browser;
        // "off" is the user turning the provider off, anything else turns it on.
        entry.enabled = source != "off";
        config.save().map_err(|e| e.to_string())?;
    }
    crate::cookies::invalidate();
    crate::scheduler::refresh_one(&app, &id).await;
    Ok(())
}

/// The header is a secret and is never echoed back: `get_config` redacts it.
#[tauri::command]
pub async fn set_cookie_header(app: AppHandle, id: String, header: String) -> Result<(), String> {
    let header = header.trim().to_string();
    {
        let state = app.state::<AppState>();
        let mut config = state.config.write().await;
        let entry = config.providers.entry(id.clone()).or_default();
        if header.is_empty() {
            entry.cookie_header = None;
            // Fall back to reading the browser rather than leaving a dead manual source.
            if entry.cookie_source == "manual" {
                entry.cookie_source = "auto".to_string();
            }
        } else {
            entry.cookie_header = Some(header);
            entry.cookie_source = "manual".to_string();
            entry.enabled = true;
        }
        config.save().map_err(|e| e.to_string())?;
    }
    crate::cookies::invalidate();
    crate::scheduler::refresh_one(&app, &id).await;
    Ok(())
}

// ------------------------------------------------------------------ history

#[tauri::command]
pub async fn get_history(
    state: State<'_, AppState>,
) -> Result<HashMap<String, Vec<Sample>>, String> {
    Ok(state.history.read().await.series().clone())
}

// ------------------------------------------------------------------ diagnostics

/// Row 27. Drop the memoized cookie scans, so the next fetch re-reads the browser.
///
/// A stale cookie cache is the likeliest support ticket: the user signs back in and the
/// tile keeps showing the 30 minute old "no session" answer. This is the reset, and it
/// refreshes straight afterwards so the user sees the result rather than being told to
/// wait for the next tick.
#[tauri::command]
pub async fn clear_cookie_cache(app: AppHandle) -> Result<(), String> {
    crate::cookies::invalidate();
    // A manual refresh, so it also bypasses any row 23 backoff.
    crate::scheduler::refresh_now(&app).await;
    Ok(())
}

/// Row 27. A support report the user can paste into a GitHub issue, so a staleness report
/// takes one round trip instead of three.
///
/// **It carries no secrets**, and not by scrubbing them out afterwards: it is assembled
/// field by field from a list that contains none. No `api_key`, no `cookie_header`, no
/// token, no cookie value, no cookie name, no `account` display string (Claude and Gemini
/// put an email there), no `account_key` and no filesystem paths. Only booleans about
/// whether a credential is present. The finished string still goes through the log sink's
/// redactor on the way out, so if a provider error message ever starts carrying a
/// credential it is caught by the same rule that catches it in the log.
#[tauri::command]
pub async fn export_diagnostics(state: State<'_, AppState>) -> Result<String, String> {
    let config = state.config.read().await.clone();
    let snapshots = state.snapshots.read().await.clone();
    // Counts only. The keys carry the account half of a series (row 35) and never leave
    // the backend; `render_report` splits the provider id back off them.
    let history: HashMap<String, usize> = state
        .history
        .read()
        .await
        .series()
        .iter()
        .map(|(key, samples)| (key.clone(), samples.len()))
        .collect();
    let skips = state.skips().await;
    // `is_configured` and browser detection both read cookie databases with blocking file
    // IO on a cold cache, exactly as `list_providers` does.
    tauri::async_runtime::spawn_blocking(move || {
        render_report(&config, &snapshots, &list_browsers(), &history, &skips)
    })
    .await
    .map_err(|e| e.to_string())
}

fn render_report(
    config: &Config,
    snapshots: &HashMap<String, UsageSnapshot>,
    browsers: &[BrowserInfo],
    history: &HashMap<String, usize>,
    skips: &[(String, DateTime<Utc>)],
) -> String {
    let now = Utc::now();
    let mut out: Vec<String> = vec![
        "AgentsBar diagnostics".to_string(),
        "====================".to_string(),
        format!("generated:   {}", now.to_rfc3339()),
        format!("app version: {}", env!("CARGO_PKG_VERSION")),
        format!("windows:     {}", windows_version()),
        format!(
            "webview2:    {}",
            tauri::webview_version().unwrap_or_else(|e| format!("unavailable ({e})"))
        ),
        // Row 24. "every 5 minutes" would be a lie on a fresh install, which is adaptive
        // with `refresh_minutes` still sitting underneath as the interval to return to.
        format!(
            "refresh:     {}",
            if config.refresh_adaptive {
                format!("adaptive (fixed setting is {} min)", config.refresh_minutes)
            } else {
                format!("every {} minute(s)", config.refresh_minutes)
            }
        ),
        format!("theme:       {}", config.theme),
        format!("autostart:   {}", config.launch_at_startup),
        format!(
            "pinned:      {}",
            config.pinned_provider.as_deref().unwrap_or("none")
        ),
        format!(
            "config file: {}",
            if Config::path().is_file() {
                "present"
            } else {
                "missing"
            }
        ),
        String::new(),
        "Browsers".to_string(),
        "--------".to_string(),
    ];

    if browsers.is_empty() {
        out.push("  none detected with a readable cookie database".to_string());
    }
    for b in browsers {
        out.push(format!(
            "  {:<8} {}",
            b.id,
            match &b.note {
                // The note already says whether it is app-bound (v20) encryption, the
                // browser being open, or an unreadable file.
                Some(note) => format!("LIMITED: {note}"),
                None => "supported".to_string(),
            }
        ));
    }

    out.push(String::new());
    out.push("Providers".to_string());
    out.push("---------".to_string());
    for p in all_providers() {
        let id = p.id();
        let auth = p.auth_kind();
        out.push(format!(
            "  {:<11} {:<10} enabled={:<3} configured={:<3} {}",
            id,
            format!("{auth:?}"),
            yes_no(config.is_enabled(id)),
            yes_no(p.is_configured(config)),
            credential_state(config, id, auth),
        ));
        out.push(format!("    {}", last_result(snapshots.get(id), now)));
        let series: Vec<usize> = history
            .iter()
            // Provider ids never contain a colon, so the first one is the account
            // boundary. The account half itself is deliberately not printed.
            .filter(|(key, _)| key.split(':').next() == Some(id))
            .map(|(_, count)| *count)
            .collect();
        if !series.is_empty() {
            out.push(format!(
                "    history: {} sample(s) across {} series",
                series.iter().sum::<usize>(),
                series.len()
            ));
        }
    }

    if !skips.is_empty() {
        out.push(String::new());
        out.push("Backoff (row 23), providers the periodic loop is holding back".to_string());
        out.push("-------".to_string());
        for (id, until) in skips {
            out.push(format!(
                "  {:<11} until {} ({}s)",
                id,
                until.to_rfc3339(),
                (*until - now).num_seconds().max(0)
            ));
        }
    }

    out.push(String::new());
    out.push(
        "This report deliberately contains no API keys, cookies, tokens, account names, \
         email addresses or file paths."
            .to_string(),
    );

    // Belt and braces: the same sink that keeps credentials out of the log file. Nothing
    // above reads a secret field, so this should never have anything to do. What it can
    // still catch is the one field assembled elsewhere, the provider's own error message:
    // beside the named vendor shapes the redactor also cuts any long opaque token, so a
    // future provider that echoes a session value into an error does not leak it here.
    crate::redact::redact(&out.join("\n")).into_owned()
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

/// Whether a credential is present, never what it is.
fn credential_state(config: &Config, id: &str, auth: AuthKind) -> String {
    match auth {
        AuthKind::Cookie => format!(
            // Not "cookie:", which the log redactor reads as a header line.
            "cookie source={} browser={} pasted={}",
            config.cookie_source(id),
            config.cookie_browser(id).unwrap_or("any"),
            yes_no(config.cookie_header(id).is_some()),
        ),
        AuthKind::ApiKey | AuthKind::Token => {
            format!("key={}", yes_no(config.api_key(id).is_some()))
        }
        // OAuth file providers read a credential file the user's CLI wrote, so
        // `configured` above is the whole story.
        AuthKind::OauthFile | AuthKind::None => String::new(),
    }
}

/// One line for the last fetch: when, whether it worked, and the row 21 error kind.
fn last_result(snapshot: Option<&UsageSnapshot>, now: DateTime<Utc>) -> String {
    let Some(s) = snapshot else {
        return "last: never fetched this session".to_string();
    };
    let when = format!(
        "last: {} ({})",
        s.fetched_at.to_rfc3339(),
        age((now - s.fetched_at).num_seconds())
    );
    match (&s.error, s.error_kind) {
        // The kind is what the UI and the backoff switch on; the message is what the user
        // sees on the tile, so a report without both cannot explain either.
        (Some(message), kind) => format!("{when} FAILED kind={} {message}", kind_name(kind)),
        (None, _) => format!(
            "{when} ok, lanes [{}], plan={}, account={}",
            lanes(s),
            s.plan.as_deref().unwrap_or("none"),
            // NOT the value: `account` is an email on Claude and Gemini, and `account_key`
            // is an identity that keys the history file.
            yes_no(s.account.is_some() || s.account_key.is_some()),
        ),
    }
}

fn kind_name(kind: Option<crate::providers::ProviderErrorKind>) -> String {
    // Serde spells the same snake_case the frontend switches on, so a report and a bug
    // report about the frontend use one vocabulary.
    match kind.and_then(|k| serde_json::to_string(&k).ok()) {
        Some(json) => json.trim_matches('"').to_string(),
        None => "unset".to_string(),
    }
}

fn lanes(s: &UsageSnapshot) -> String {
    let one = |w: &UsageWindow| match w.used_percent {
        Some(p) => format!("{}={p}%", w.label),
        None => format!("{}=unknown", w.label),
    };
    let lanes: Vec<String> = [&s.primary, &s.secondary, &s.tertiary]
        .into_iter()
        .flatten()
        .map(one)
        .collect();
    if lanes.is_empty() {
        return "none".to_string();
    }
    lanes.join(", ")
}

fn age(secs: i64) -> String {
    match secs {
        s if s < 0 => "in the future, check the clock".to_string(),
        s if s < 90 => format!("{s}s ago"),
        s if s < 5400 => format!("{}m ago", s / 60),
        s if s < 172_800 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// The real Windows build number.
///
/// `RtlGetVersion` rather than `GetVersionEx`, because `GetVersionEx` reports 6.2 to any
/// process without a compatibility manifest, and "Windows 8" in every bug report is worse
/// than no line at all. Declared here rather than pulled from the `windows` crate: that
/// would mean a new crate feature in `Cargo.toml`, which this agent does not own, for one
/// call.
fn windows_version() -> String {
    #[repr(C)]
    struct OsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform_id: u32,
        csd_version: [u16; 128],
    }
    extern "system" {
        #[link_name = "RtlGetVersion"]
        fn rtl_get_version(info: *mut OsVersionInfoW) -> i32;
    }
    let mut info = OsVersionInfoW {
        size: std::mem::size_of::<OsVersionInfoW>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform_id: 0,
        csd_version: [0; 128],
    };
    // SAFETY: `info` is a correctly sized, correctly laid out OSVERSIONINFOW with its
    // `size` field set, which is the entire contract. RtlGetVersion only writes it.
    if unsafe { rtl_get_version(&mut info) } != 0 {
        return "unknown".to_string();
    }
    // Windows 11 still reports 10.0; only the build number separates the two.
    let name = match (info.major, info.build) {
        (10, b) if b >= 22000 => "Windows 11",
        (10, _) => "Windows 10",
        _ => "Windows",
    };
    format!("{name} {}.{}.{}", info.major, info.minor, info.build)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ProviderError, UsageWindow};

    /// Every value here is synthetic. None of it is, or ever was, a real credential.
    const FAKE_SECRETS: [&str; 8] = [
        "sk-proj-FAKEAPIKEY00000000",
        "WorkosCursorSessionToken=FAKECOOKIEVALUE9999",
        "FAKECOOKIEVALUE9999",
        "ghp_FAKEGITHUBTOKEN000000",
        "someone@example.com",
        "FAKEBEARERTOKEN0000",
        "FAKEACCOUNTKEY0000",
        "FAKEPASTEDBUNDLE0000",
    ];

    /// A config with a fake secret in every field that can hold one, on every provider.
    fn config_full_of_secrets() -> Config {
        let mut config = Config::default();
        for (id, provider) in config.providers.iter_mut() {
            provider.enabled = true;
            provider.api_key = Some(format!("sk-proj-FAKEAPIKEY00000000-{id}"));
            provider.cookie_header = Some(format!(
                "WorkosCursorSessionToken=FAKECOOKIEVALUE9999; auth={id}"
            ));
            provider.cookie_source = "manual".to_string();
        }
        // Devin and Windsurf take a pasted bundle through the same field.
        config.providers.get_mut("devin").unwrap().cookie_header =
            Some("Bearer FAKEPASTEDBUNDLE0000; https://app.devin.ai/org/acme".to_string());
        config
    }

    fn snapshots_full_of_secrets() -> HashMap<String, UsageSnapshot> {
        let mut ok = UsageSnapshot::new("codex");
        ok.primary = Some(UsageWindow::new("5h", Some(12.5), None, Some(300)));
        ok.plan = Some("Pro".to_string());
        // The two identity fields, both of which the report must reduce to a boolean.
        ok.account = Some("someone@example.com".to_string());
        ok.account_key = Some("FAKEACCOUNTKEY0000".to_string());

        let mut failed = UsageSnapshot::new("cursor");
        // The worst realistic case: an upstream error string that echoed the credential
        // back at us. Nothing in AgentsBar builds one, and the sink must catch it anyway.
        failed.set_error(&ProviderError::Auth(
            "HTTP 401 for cookie: WorkosCursorSessionToken=FAKECOOKIEVALUE9999".to_string(),
        ));

        let mut limited = UsageSnapshot::new("openrouter");
        limited.set_error(&ProviderError::RateLimited { retry_after: None });

        [ok, failed, limited]
            .into_iter()
            .map(|s| (s.provider_id.clone(), s))
            .collect()
    }

    /// Row 27, the deliverable half of the row: the report a user pastes into a public
    /// issue must not carry a credential, an account name or an email address.
    #[test]
    fn the_diagnostics_report_contains_no_secret_from_the_config() {
        let report = render_report(
            &config_full_of_secrets(),
            &snapshots_full_of_secrets(),
            &[
                BrowserInfo {
                    id: "chrome",
                    label: "Chrome".to_string(),
                    supported: true,
                    note: None,
                },
                BrowserInfo {
                    id: "brave",
                    label: "Brave".to_string(),
                    supported: false,
                    note: Some(
                        "Brave encrypts newer cookies with app-bound encryption (v20).".to_string(),
                    ),
                },
            ],
            &HashMap::from([
                ("codex:FAKEACCOUNTKEY0000".to_string(), 288),
                ("codex".to_string(), 12),
                ("cursor".to_string(), 5),
            ]),
            &[(
                "openrouter".to_string(),
                Utc::now() + chrono::Duration::minutes(5),
            )],
        );

        for secret in FAKE_SECRETS {
            assert!(
                !report.contains(secret),
                "the report leaked {secret:?}:\n{report}"
            );
        }
        // A report that leaked nothing because it said nothing would pass the loop above.
        for expected in [
            "AgentsBar diagnostics",
            "codex",
            "cursor",
            "configured=",
            "kind=auth",
            "kind=rate_limited",
            "app-bound encryption (v20)",
            // 288 under `codex:<account>` plus 12 under the bare id, and the account half
            // of the key is not printed.
            "300 sample(s) across 2 series",
            "Backoff",
        ] {
            assert!(
                report.contains(expected),
                "the report is missing {expected:?}:\n{report}"
            );
        }
    }

    /// The identity fields are reported as presence, never as a value, and an account key
    /// never reaches the report through the history keys either.
    #[test]
    fn identity_is_reported_as_a_boolean() {
        let mut s = UsageSnapshot::new("codex");
        s.account = Some("someone@example.com".to_string());
        assert_eq!(
            last_result(Some(&s), s.fetched_at),
            "last: ".to_string()
                + &s.fetched_at.to_rfc3339()
                + " (0s ago) ok, lanes [none], plan=none, account=yes"
        );
        assert!(last_result(None, Utc::now()).contains("never fetched"));
    }

    /// A wave 3 snapshot has an `error` with no `error_kind`, and the report must still
    /// say which provider failed rather than dropping the line.
    #[test]
    fn an_error_without_a_kind_still_reports() {
        let mut s = UsageSnapshot::new("codex");
        s.error = Some("boom".to_string());
        let line = last_result(Some(&s), s.fetched_at);
        assert!(line.contains("FAILED kind=unset boom"), "{line}");
    }

    #[test]
    fn the_windows_build_is_readable() {
        let version = windows_version();
        assert!(version.starts_with("Windows"), "{version}");
        assert!(version.contains('.'), "{version}");
    }
}
