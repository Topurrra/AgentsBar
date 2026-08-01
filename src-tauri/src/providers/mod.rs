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
    pub plan: Option<String>,
    pub account: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub error: Option<String>,
}

impl UsageSnapshot {
    pub fn new(provider_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            primary: None,
            secondary: None,
            tertiary: None,
            credits: None,
            plan: None,
            account: None,
            fetched_at: Utc::now(),
            error: None,
        }
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
    ]
}

pub fn provider_by_id(id: &str) -> Option<Box<dyn Provider>> {
    all_providers().into_iter().find(|p| p.id() == id)
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
