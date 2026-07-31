use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod util;

pub mod api_token;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod deepgram;
pub mod deepseek;
pub mod elevenlabs;
pub mod gemini;
// groq.rs is kept on disk but unregistered: its only real usage source is a
// browser cookie (see the file header). Re-add here when cookie import lands.
pub mod kimi;
pub mod minimax;
pub mod openai;
pub mod openrouter;
pub mod xai;
pub mod zai;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageWindow {
    pub label: String,
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
    pub window_minutes: Option<u64>,
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
    ]
}

pub fn provider_by_id(id: &str) -> Option<Box<dyn Provider>> {
    all_providers().into_iter().find(|p| p.id() == id)
}
