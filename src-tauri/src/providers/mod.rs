use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use crate::cookies::Want;

pub mod util;

pub mod amp;
pub mod api_token;
pub mod augment;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod deepgram;
pub mod deepseek;
pub mod devin;
pub mod elevenlabs;
pub mod factory;
pub mod gemini;
// groq.rs is kept on disk but unregistered: its only real usage source is a
// browser cookie (see the file header). Re-add here when cookie import lands.
pub mod kimi;
pub mod manus;
pub mod minimax;
pub mod openai;
pub mod opencode;
pub mod openrouter;
pub mod qwen;
pub mod t3chat;
pub mod warp;
pub mod windsurf;
pub mod xai;
pub mod zai;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageWindow {
    pub label: String,
    /// Percent of the quota consumed, 0..=100, or `None` when the provider did not tell
    /// us. `None` is not zero: it serializes to JSON `null` and the UI must render it as
    /// unknown rather than as a full green bar.
    pub used_percent: Option<f64>,
    pub resets_at: Option<DateTime<Utc>>,
    pub window_minutes: Option<u64>,
}

impl UsageWindow {
    /// Build a window. Every provider mapper goes through here, so the validation lives
    /// in one place instead of once per provider.
    ///
    /// `used_percent` of `None`, NaN or infinity is UNKNOWN and stays `None`; a finite
    /// value is clamped to 0..=100. A `resets_at` that has already passed is dropped,
    /// because a countdown to a deadline in the past is worse than no countdown. A zero
    /// `window_minutes` is no duration at all.
    pub fn new(
        label: impl Into<String>,
        used_percent: Option<f64>,
        resets_at: Option<DateTime<Utc>>,
        window_minutes: Option<u64>,
    ) -> Self {
        Self::at(label, used_percent, resets_at, window_minutes, Utc::now())
    }

    /// [`Self::new`] against an explicit clock. Mappers that already hold a `now` use
    /// this, so their tests do not depend on the wall clock.
    pub fn at(
        label: impl Into<String>,
        used_percent: Option<f64>,
        resets_at: Option<DateTime<Utc>>,
        window_minutes: Option<u64>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            label: label.into(),
            used_percent: used_percent
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.0, 100.0)),
            resets_at: resets_at.filter(|t| *t > now),
            window_minutes: window_minutes.filter(|m| *m > 0),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub provider_id: String,
    pub primary: Option<UsageWindow>,
    pub secondary: Option<UsageWindow>,
    pub tertiary: Option<UsageWindow>,
    pub credits: Option<f64>,
    /// The unit `credits` is denominated in, when known ("USD", "CNY", ...). The cost
    /// summary only aggregates credits whose unit is "USD", so a CNY balance or a token
    /// count is never silently added to a dollar total. `None` means the unit is unknown
    /// or `credits` is not a currency, and is left out of the money math.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits_unit: Option<String>,
    /// Real USD billed over the provider's trailing reporting window (OpenAI's last 30
    /// days). A SPEND, not a balance: it is what left the account, distinct from `credits`
    /// which is what remains. Aggregated separately in the cost summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend_usd: Option<f64>,
    pub plan: Option<String>,
    pub account: Option<String>,
    /// Row 35. A stable, non-PII identity for the account these numbers belong to, when
    /// the provider already knows one (Codex decodes `chatgpt_account_id`). `account` is
    /// a display string; this is the one history keys on, so that a `codex login` into a
    /// second account starts a new series instead of appending its 4 percent onto the
    /// first account's 91 percent and drawing a cliff that never happened.
    ///
    /// Identity TAGGING only. Nothing here switches, stores or lists accounts.
    #[serde(default)]
    pub account_key: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub error: Option<String>,
    /// Row 21. The classified form of `error`, so the UI can tell a transient blip from a
    /// dead session without pattern matching an English string. Always set and cleared
    /// together with `error`: use [`Self::set_error`] and [`Self::clear_error`].
    #[serde(default)]
    pub error_kind: Option<ProviderErrorKind>,
}

impl UsageSnapshot {
    pub fn new(provider_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            primary: None,
            secondary: None,
            tertiary: None,
            credits: None,
            credits_unit: None,
            spend_usd: None,
            plan: None,
            account: None,
            account_key: None,
            fetched_at: Utc::now(),
            error: None,
            error_kind: None,
        }
    }

    /// Record a failure. The message is what the tile prints today, the kind is what the
    /// error copy and the styling switch on, and they must never disagree.
    pub fn set_error(&mut self, e: &ProviderError) {
        self.error = Some(e.to_string());
        self.error_kind = Some(e.kind());
    }

    /// Clear both halves. A snapshot kept across a failure and then recovered must not
    /// keep a stale kind behind a cleared message.
    pub fn clear_error(&mut self) {
        self.error = None;
        self.error_kind = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    OauthFile,
    ApiKey,
    Token,
    /// Authenticated by a browser session cookie, imported by [`crate::cookies`].
    /// The UI renders these with a browser picker instead of an API key field.
    Cookie,
    None,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub auth: AuthKind,
    pub configured: bool,
    pub doc_url: &'static str,
    /// Row 38. The environment variable this provider's key can also come from, so the
    /// error copy can name it. A key resolved from the environment is invisible in
    /// Settings, and "paste it in Settings" is useless advice when that is where it came
    /// from. `None` when the provider reads no variable.
    pub env_key: Option<&'static str>,
}

pub struct FetchContext {
    pub http: reqwest::Client,
    pub config: crate::config::Config,
}

impl FetchContext {
    /// The `Cookie` header value for a cookie authenticated provider.
    ///
    /// Honours the provider's configured source: `manual` uses the pasted header, `off`
    /// always fails, and `auto` walks the detected browsers (Chrome, Edge, Brave,
    /// Firefox, or only the pinned one) until one satisfies `want`.
    ///
    /// `domains` are host suffixes, for example `["cursor.com"]`. One scan per browser
    /// profile and domain set is memoized, so calling this from `is_configured` and from
    /// `fetch`, with any mix of [`Want`] shapes, still costs one scan.
    ///
    /// The returned string is a secret. Put it straight into a request header, never
    /// into a log line or an error message.
    pub fn cookie_header(
        &self,
        provider_id: &str,
        domains: &[&str],
        want: Want,
    ) -> Result<String, ProviderError> {
        crate::cookies::resolve(&self.config, provider_id, domains, want).map_err(Into::into)
    }

    /// Run `attempt` against each imported browser session until the provider's API
    /// accepts one, then remember which browser won.
    ///
    /// Row 8. Holding the right cookie NAMES is not proof the session is alive: a logged
    /// out Chrome and a signed in Edge look identical on disk, so [`cookie_header`] alone
    /// lets the dead Chrome session mask the live Edge one forever. Only the API can tell
    /// them apart, which is why the walk lives here and not in `cookies.rs`.
    ///
    /// Only an [`ProviderError::Auth`] failure moves to the next candidate. An `Http` 500
    /// or a `Parse` error will fail identically on every browser, so retrying them is just
    /// three more requests before the same error.
    ///
    /// `signin_hint` is appended only when no browser session could be built at all, which
    /// is where the per-provider "sign in at x, or paste a cookie header in Settings"
    /// wording belongs.
    ///
    /// The `String` handed to `attempt` is a secret. Put it straight into a request
    /// header, never into a log line or an error message.
    ///
    /// [`cookie_header`]: FetchContext::cookie_header
    pub async fn with_cookies<T, F, Fut>(
        &self,
        provider_id: &str,
        domains: &[&str],
        want: Want<'_>,
        signin_hint: &str,
        attempt: F,
    ) -> Result<T, ProviderError>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<T, ProviderError>>,
    {
        let list = crate::cookies::candidates(&self.config, provider_id, domains, want)
            .map_err(|e| ProviderError::Auth(format!("{e}. {signin_hint}")))?;
        walk(provider_id, list, attempt).await
    }
}

/// The candidate walk itself, split out so it is testable without a browser on disk.
async fn walk<T, F, Fut>(
    provider_id: &str,
    list: Vec<crate::cookies::Candidate>,
    attempt: F,
) -> Result<T, ProviderError>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<T, ProviderError>>,
{
    // `candidates` never returns an empty Ok, so this is the index of a real entry.
    let last = list.len().saturating_sub(1);
    let mut rejected = None;
    for (i, candidate) in list.into_iter().enumerate() {
        match attempt(candidate.header().to_string()).await {
            Ok(value) => {
                crate::cookies::remember(provider_id, &candidate);
                return Ok(value);
            }
            Err(e @ ProviderError::Auth(_)) if i < last => {
                // The label names a browser and a profile, never a cookie value.
                log::info!(
                    "{provider_id}: the {} session was rejected, trying the next browser",
                    candidate.label
                );
                rejected = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(rejected.unwrap_or(ProviderError::NotConfigured))
}

/// Companion to [`FetchContext::cookie_header`] for `is_configured`, which only gets a
/// `&Config`. Same memoized scan, so it does not cost a second database read.
pub fn has_cookies(
    config: &crate::config::Config,
    provider_id: &str,
    domains: &[&str],
    want: Want,
) -> bool {
    config.cookie_source(provider_id) != "off"
        && crate::cookies::available(config, provider_id, domains, want)
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("not configured")]
    NotConfigured,
    #[error("auth error: {0}")]
    Auth(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("parse error: {0}")]
    Parse(String),
    /// Row 23. A 429. `retry_after` is the parsed `Retry-After` header when the provider
    /// sent one, which is the only number in the whole system that tells us when it is
    /// polite to come back. `None` means the provider did not say, so the caller applies
    /// its own default cooldown.
    #[error("rate limited")]
    RateLimited {
        retry_after: Option<std::time::Duration>,
    },
}

/// Row 21. The stable, serialized shape of a failure.
///
/// The message on the snapshot is for a human; this is what the frontend switches on to
/// choose wording (row 22) and styling (auth is red and demands action, http is muted and
/// self-healing), and what the backoff policy switches on to decide whether to wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    NotConfigured,
    Auth,
    Http,
    Parse,
    RateLimited,
}

impl ProviderError {
    pub fn kind(&self) -> ProviderErrorKind {
        match self {
            ProviderError::NotConfigured => ProviderErrorKind::NotConfigured,
            ProviderError::Auth(_) => ProviderErrorKind::Auth,
            ProviderError::Http(_) => ProviderErrorKind::Http,
            ProviderError::Parse(_) => ProviderErrorKind::Parse,
            ProviderError::RateLimited { .. } => ProviderErrorKind::RateLimited,
        }
    }

    /// How long the provider asked us to wait, if it asked at all.
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            ProviderError::RateLimited { retry_after } => *retry_after,
            _ => None,
        }
    }
}

/// A `Retry-After` header value is server controlled, so it is clamped. An hour is longer
/// than any cadence we run and short enough that a hostile or broken header cannot park a
/// provider for a day.
pub const MAX_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(3600);

/// Parse a `Retry-After` header: delta-seconds, or an HTTP-date. Clamped to
/// [`MAX_RETRY_AFTER`], and `None` for anything that would not make us wait at all, so a
/// `Retry-After: 0` falls back to the caller's own cooldown rather than to no cooldown.
pub fn parse_retry_after(value: &str) -> Option<std::time::Duration> {
    retry_after_at(value, Utc::now())
}

/// The `Retry-After` a response asked for, if it asked at all.
///
/// Every 429 in the app goes through here, so a provider cannot accidentally become the one
/// that ignores the only number telling us when it is polite to come back. Must be called
/// before the body is consumed, since that takes the response by value.
pub fn retry_after_of(resp: &reqwest::Response) -> Option<std::time::Duration> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after)
}

/// [`parse_retry_after`] against an explicit clock, so the HTTP-date case is testable.
pub fn retry_after_at(value: &str, now: DateTime<Utc>) -> Option<std::time::Duration> {
    let value = value.trim();
    let secs = match value.parse::<i64>() {
        Ok(secs) => secs,
        // RFC 9110 allows an IMF-fixdate, "Wed, 21 Oct 2015 07:28:00 GMT". chrono's RFC
        // 2822 parser accepts that spelling including the obsolete GMT zone.
        Err(_) => {
            let at = DateTime::parse_from_rfc2822(value)
                .ok()?
                .with_timezone(&Utc);
            (at - now).num_seconds()
        }
    };
    (secs > 0).then(|| std::time::Duration::from_secs(secs as u64).min(MAX_RETRY_AFTER))
}

impl From<reqwest::Error> for ProviderError {
    fn from(e: reqwest::Error) -> Self {
        ProviderError::Http(e.to_string())
    }
}

/// Cookie problems are auth problems as far as the UI is concerned. The message names
/// browsers and cookie NAMES only, never a value.
impl From<crate::cookies::CookieError> for ProviderError {
    fn from(e: crate::cookies::CookieError) -> Self {
        ProviderError::Auth(e.to_string())
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn auth_kind(&self) -> AuthKind;
    fn doc_url(&self) -> &'static str {
        ""
    }
    /// Row 38. The conventional environment variable holding this provider's key, for
    /// example `OPENROUTER_API_KEY`. `api_token::api_key` consults the config first and
    /// falls back to this, so the override is one uniform rule instead of ad hoc handling
    /// in a handful of providers. `None` when the provider has no conventional variable:
    /// do not invent one, take it from the provider's own documentation.
    fn env_key(&self) -> Option<&'static str> {
        None
    }
    fn is_configured(&self, config: &crate::config::Config) -> bool;
    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError>;
}

/// Registry order. It drives tile order in the UI and pinned-provider fallback.
pub fn all_providers() -> Vec<Box<dyn Provider>> {
    vec![
        Box::new(codex::Codex),
        Box::new(claude::Claude),
        Box::new(gemini::Gemini),
        Box::new(copilot::Copilot),
        Box::new(openai::OpenAi),
        Box::new(zai::Zai),
        Box::new(minimax::Minimax),
        Box::new(kimi::Kimi),
        Box::new(openrouter::OpenRouter),
        Box::new(deepseek::Deepseek),
        Box::new(elevenlabs::ElevenLabs),
        Box::new(deepgram::Deepgram),
        Box::new(xai::Xai),
        // Cookie authenticated, wave 2.
        Box::new(cursor::Cursor),
        Box::new(factory::Factory),
        Box::new(devin::Devin),
        Box::new(t3chat::T3Chat),
        Box::new(opencode::OpenCode),
        Box::new(manus::Manus),
        Box::new(warp::Warp),
        Box::new(windsurf::Windsurf),
        Box::new(augment::Augment),
        Box::new(amp::Amp),
        Box::new(qwen::QwenCloud),
    ]
}

pub fn provider_by_id(id: &str) -> Option<Box<dyn Provider>> {
    all_providers().into_iter().find(|p| p.id() == id)
}

/// The display name for a provider id, falling back to the id itself for an unknown one.
pub fn provider_name(id: &str) -> String {
    provider_by_id(id).map_or_else(|| id.to_string(), |p| p.name().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    fn win(used: Option<f64>) -> UsageWindow {
        UsageWindow::at("5h", used, None, Some(300), now())
    }

    #[test]
    fn unknown_inputs_stay_unknown() {
        assert_eq!(win(None).used_percent, None);
        assert_eq!(win(Some(f64::NAN)).used_percent, None);
        assert_eq!(win(Some(f64::INFINITY)).used_percent, None);
        assert_eq!(win(Some(f64::NEG_INFINITY)).used_percent, None);
    }

    #[test]
    fn finite_values_are_clamped_not_dropped() {
        assert_eq!(win(Some(-5.0)).used_percent, Some(0.0));
        assert_eq!(win(Some(0.0)).used_percent, Some(0.0));
        assert_eq!(win(Some(42.5)).used_percent, Some(42.5));
        assert_eq!(win(Some(140.0)).used_percent, Some(100.0));
    }

    /// Zero used and unknown must not collapse into the same value.
    #[test]
    fn zero_is_not_unknown() {
        assert_ne!(win(Some(0.0)).used_percent, win(None).used_percent);
    }

    #[test]
    fn an_elapsed_reset_is_dropped_and_a_future_one_kept() {
        let past = now() - chrono::Duration::seconds(1);
        let future = now() + chrono::Duration::seconds(1);
        assert_eq!(
            UsageWindow::at("5h", Some(1.0), Some(past), None, now()).resets_at,
            None
        );
        assert_eq!(
            UsageWindow::at("5h", Some(1.0), Some(now()), None, now()).resets_at,
            None
        );
        assert_eq!(
            UsageWindow::at("5h", Some(1.0), Some(future), None, now()).resets_at,
            Some(future)
        );
    }

    #[test]
    fn a_zero_length_window_has_no_duration() {
        assert_eq!(
            UsageWindow::at("5h", None, None, Some(0), now()).window_minutes,
            None
        );
    }

    /// The frontend contract: unknown is JSON `null`, not `0`.
    #[test]
    fn unknown_serializes_as_null() {
        let json = serde_json::to_string(&win(None)).unwrap();
        assert!(json.contains("\"used_percent\":null"), "{json}");
        let json = serde_json::to_string(&win(Some(0.0))).unwrap();
        assert!(json.contains("\"used_percent\":0.0"), "{json}");
    }

    // ------------------------------------------------------ row 21, error taxonomy

    #[test]
    fn every_error_has_a_kind_and_only_rate_limits_carry_a_delay() {
        let cases = [
            (
                ProviderError::NotConfigured,
                ProviderErrorKind::NotConfigured,
            ),
            (ProviderError::Auth("401".into()), ProviderErrorKind::Auth),
            (ProviderError::Http("502".into()), ProviderErrorKind::Http),
            (ProviderError::Parse("bad".into()), ProviderErrorKind::Parse),
        ];
        for (e, kind) in cases {
            assert_eq!(e.kind(), kind);
            assert_eq!(e.retry_after(), None);
        }
        let limited = ProviderError::RateLimited {
            retry_after: Some(std::time::Duration::from_secs(30)),
        };
        assert_eq!(limited.kind(), ProviderErrorKind::RateLimited);
        assert_eq!(limited.retry_after().map(|d| d.as_secs()), Some(30));
    }

    /// The frontend contract: the kind travels next to the message, in snake_case.
    #[test]
    fn the_kind_serializes_next_to_the_message() {
        let mut s = UsageSnapshot::new("codex");
        s.set_error(&ProviderError::RateLimited { retry_after: None });
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"error\":\"rate limited\""), "{json}");
        assert!(json.contains("\"error_kind\":\"rate_limited\""), "{json}");

        s.clear_error();
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"error_kind\":null"), "{json}");
    }

    /// A snapshot written by wave 3 has neither field.
    #[test]
    fn a_snapshot_without_the_new_fields_still_deserializes() {
        let json = r#"{"provider_id":"codex","primary":null,"secondary":null,
            "tertiary":null,"credits":null,"plan":null,"account":null,
            "fetched_at":"2026-01-01T00:00:00Z","error":null}"#;
        let s: UsageSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(s.error_kind, None);
        assert_eq!(s.account_key, None);
    }

    #[test]
    fn retry_after_reads_seconds_and_http_dates() {
        let now = now();
        assert_eq!(retry_after_at(" 30 ", now).map(|d| d.as_secs()), Some(30));
        // An HTTP-date, which is what half the providers send.
        let at = (now + chrono::Duration::seconds(90)).to_rfc2822();
        assert_eq!(retry_after_at(&at, now).map(|d| d.as_secs()), Some(90));
        assert_eq!(
            retry_after_at("Wed, 21 Oct 2015 07:28:00 GMT", now).map(|d| d.as_secs()),
            None,
            "a date already in the past is not a wait"
        );
        // Nothing that would not make us wait: the caller falls back to its own default.
        assert_eq!(retry_after_at("0", now), None);
        assert_eq!(retry_after_at("-5", now), None);
        assert_eq!(retry_after_at("soon", now), None);
        assert_eq!(retry_after_at("", now), None);
        // Server controlled, so it is clamped.
        assert_eq!(retry_after_at("999999", now), Some(MAX_RETRY_AFTER));
    }

    /// Row 38 is opt in, and an empty name would silently read the whole environment as
    /// "no override" while looking configured.
    #[test]
    fn a_declared_env_key_is_never_blank() {
        for p in all_providers() {
            assert!(
                p.env_key().is_none_or(|k| !k.trim().is_empty()),
                "{} declares a blank env_key",
                p.id()
            );
        }
    }

    // ---------------------------------------------------------- row 8, the walk

    fn candidates(headers: &[&str]) -> Vec<crate::cookies::Candidate> {
        headers
            .iter()
            .map(|h| crate::cookies::Candidate::synthetic("chrome", h))
            .collect()
    }

    /// A fake provider API that records every header it was handed, so a test can assert
    /// where the walk stopped. `live` succeeds, `dead` is rejected the way an expired
    /// session is, anything else fails the way a broken endpoint does.
    fn api(
        seen: &std::cell::RefCell<Vec<String>>,
    ) -> impl Fn(String) -> std::future::Ready<Result<&'static str, ProviderError>> + '_ {
        move |header: String| {
            seen.borrow_mut().push(header.clone());
            std::future::ready(match header.as_str() {
                "live" => Ok("usage"),
                "dead" => Err(ProviderError::Auth("rejected".into())),
                _ => Err(ProviderError::Http("500".into())),
            })
        }
    }

    fn block_on<T>(f: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// The row 8 bug: a logged out Chrome holds the same cookie names as a signed in Edge,
    /// so the first candidate can be dead while a later one works.
    #[test]
    fn a_rejected_session_moves_on_to_the_next_browser() {
        let seen = std::cell::RefCell::new(Vec::new());
        let got = block_on(walk(
            "cursor",
            candidates(&["dead", "dead", "live"]),
            api(&seen),
        ));
        assert_eq!(got.unwrap(), "usage");
        assert_eq!(seen.into_inner(), ["dead", "dead", "live"]);
    }

    /// An `Http` 500 or a `Parse` error fails identically on every browser, so retrying
    /// them is just more requests before the same error.
    #[test]
    fn only_an_auth_failure_walks_on() {
        let seen = std::cell::RefCell::new(Vec::new());
        let got = block_on(walk("cursor", candidates(&["boom", "live"]), api(&seen)));
        assert!(matches!(got, Err(ProviderError::Http(_))), "{got:?}");
        assert_eq!(seen.into_inner(), ["boom"], "a transport error was retried");
    }

    /// All dead: the caller must see the real rejection, not a generic NotConfigured.
    #[test]
    fn the_last_rejection_is_what_surfaces() {
        let seen = std::cell::RefCell::new(Vec::new());
        let got: Result<&str, _> =
            block_on(walk("cursor", candidates(&["dead", "dead"]), api(&seen)));
        assert!(matches!(got, Err(ProviderError::Auth(_))), "{got:?}");
        assert_eq!(seen.into_inner().len(), 2);
    }
}
