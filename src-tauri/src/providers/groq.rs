//! Deliberately left unimplemented in wave 1. Groq's real usage source is the
//! console API behind a `stytch_session_jwt` browser cookie
//! (Sources/CodexBarCore/Providers/Groq/GroqConsoleFetcher.swift). The only API-key
//! path is an enterprise-tier Prometheus metrics endpoint that returns request and
//! token rates with no quota, so every window it produces would gauge 0% and a
//! standard key would simply 404. See Docs/notes-providers-c.md.

use async_trait::async_trait;

use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot};
use crate::config::Config;

pub struct Groq;

#[async_trait]
impl Provider for Groq {
    fn id(&self) -> &'static str {
        "groq"
    }

    fn name(&self) -> &'static str {
        "Groq"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn doc_url(&self) -> &'static str {
        "https://console.groq.com/keys"
    }

    fn is_configured(&self, _config: &Config) -> bool {
        false
    }

    async fn fetch(&self, _ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        Err(ProviderError::NotConfigured)
    }
}
