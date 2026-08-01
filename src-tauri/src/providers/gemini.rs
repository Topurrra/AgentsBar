//! Gemini CLI usage, ported from CodexBar `Providers/Gemini/GeminiStatusProbe.swift`.
//!
//! Credentials come from `%USERPROFILE%\.gemini\oauth_creds.json` (written by the
//! Gemini CLI). Expired access tokens are refreshed against Google's token endpoint
//! with the Gemini CLI public OAuth client and written back to the same file, exactly
//! like the CLI does. Quota comes from the Cloud Code private API.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use super::util::{home_subdir, read_json_file};
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow};
use crate::config::Config;

const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const LOAD_CODE_ASSIST: &str = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const RETRIEVE_QUOTA: &str = "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota";
const PROJECTS_ENDPOINT: &str = "https://cloudresourcemanager.googleapis.com/v1/projects";

/// The OAuth client the Gemini CLI uses for its own installed-app flow.
///
/// These belong to Google, not to us, so AgentsBar does NOT ship them. We read them out
/// of the Gemini CLI the user already has installed, which is what CodexBar's Swift probe
/// does (`GeminiStatusProbe` scrapes the CLI's `oauth2.js`). Hardcoding them would mean
/// redistributing someone else's credentials, and secret scanners correctly flag that.
///
/// Resolution order: the two env vars, then the installed CLI's bundle. Resolved once and
/// cached, because the scan walks a directory.
static OAUTH_CLIENT: std::sync::OnceLock<Option<(String, String)>> = std::sync::OnceLock::new();

const CLIENT_ID_RE_SUFFIX: &str = ".apps.googleusercontent.com";
const CLIENT_SECRET_PREFIX: &str = "GOCSPX-";

fn gemini_dir() -> PathBuf {
    home_subdir(".gemini", None)
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Directories that may hold an installed Gemini CLI. `GEMINI_CLI_PATH` wins so a user
/// with an unusual layout (volta, fnm, a pnpm store) can point us straight at it.
fn cli_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(explicit) = env_var("GEMINI_CLI_PATH") {
        roots.push(PathBuf::from(explicit));
    }
    for var in ["APPDATA", "LOCALAPPDATA", "ProgramFiles"] {
        if let Ok(base) = std::env::var(var) {
            roots.push(
                PathBuf::from(base)
                    .join("npm")
                    .join("node_modules")
                    .join("@google")
                    .join("gemini-cli"),
            );
        }
    }
    roots
}

/// Pulls the `<digits>-<id>.apps.googleusercontent.com` / `GOCSPX-...` pair out of one
/// JavaScript file. Both must come from the same file so we cannot pair a client id with
/// an unrelated secret.
fn scrape_pair(text: &str) -> Option<(String, String)> {
    let secret_start = text.find(CLIENT_SECRET_PREFIX)?;
    let secret_end = text[secret_start..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .map(|i| secret_start + i)
        .unwrap_or(text.len());
    let secret = &text[secret_start..secret_end];
    if secret.len() <= CLIENT_SECRET_PREFIX.len() {
        return None;
    }

    // A minified bundle holds client ids for several Google products, so "first id in the
    // file" pairs the secret with an unrelated client and every refresh 401s. The two
    // halves of one credential are declared next to each other, so take the id NEAREST the
    // secret: in the shipped CLI the right one is ~100 bytes away and the decoy is 6.6 MB.
    let mut best: Option<(usize, &str)> = None;
    for (id_end, _) in text.match_indices(CLIENT_ID_RE_SUFFIX) {
        let id_start = text[..id_end]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
            .map(|i| i + 1)
            .unwrap_or(0);
        let id = &text[id_start..id_end + CLIENT_ID_RE_SUFFIX.len()];
        if id.len() <= CLIENT_ID_RE_SUFFIX.len() {
            continue;
        }
        let distance = id_start.abs_diff(secret_start);
        if best.map_or(true, |(d, _)| distance < d) {
            best = Some((distance, id));
        }
    }
    Some((best?.1.to_string(), secret.to_string()))
}

fn scrape_installed_cli() -> Option<(String, String)> {
    for root in cli_roots() {
        for sub in ["bundle", "dist", "."] {
            let dir = root.join(sub);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("js") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                if let Some(pair) = scrape_pair(&text) {
                    return Some(pair);
                }
            }
        }
    }
    None
}

fn oauth_client() -> Result<&'static (String, String), ProviderError> {
    OAUTH_CLIENT
        .get_or_init(|| {
            if let (Some(id), Some(secret)) = (
                env_var("GEMINI_OAUTH_CLIENT_ID"),
                env_var("GEMINI_OAUTH_CLIENT_SECRET"),
            ) {
                return Some((id, secret));
            }
            scrape_installed_cli()
        })
        .as_ref()
        .ok_or_else(|| {
            ProviderError::Auth(
                "Gemini needs the Gemini CLI installed to refresh its token, or set \
                 GEMINI_OAUTH_CLIENT_ID and GEMINI_OAUTH_CLIENT_SECRET"
                    .into(),
            )
        })
}

#[derive(Debug, Deserialize)]
struct OauthCreds {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    /// Milliseconds since epoch, as the CLI writes it.
    expiry_date: Option<f64>,
}

#[derive(Debug, Default)]
struct CodeAssistStatus {
    tier: Option<String>,
    project: Option<String>,
    paid_tier_name: Option<String>,
}

#[derive(Debug, Default)]
struct Claims {
    email: Option<String>,
    hosted_domain: Option<String>,
}

pub struct Gemini;

#[async_trait]
impl Provider for Gemini {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn name(&self) -> &'static str {
        "Gemini"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::OauthFile
    }

    fn doc_url(&self) -> &'static str {
        "https://github.com/google-gemini/gemini-cli"
    }

    fn is_configured(&self, _config: &Config) -> bool {
        gemini_dir().join("oauth_creds.json").is_file()
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let dir = gemini_dir();
        if let Some(kind) = unsupported_auth(&dir) {
            return Err(ProviderError::Auth(format!(
                "Gemini {kind} auth is not supported, sign in with a Google account"
            )));
        }

        let creds: OauthCreds = read_json_file(dir.join("oauth_creds.json"))?;
        let expired = creds
            .expiry_date
            .is_some_and(|ms| ms / 1000.0 <= Utc::now().timestamp() as f64);
        let mut access = creds.access_token.clone().filter(|t| !t.is_empty());
        let mut id_token = creds.id_token.clone();

        if access.is_none() || expired {
            let refresh = creds
                .refresh_token
                .as_deref()
                .filter(|t| !t.is_empty())
                .ok_or_else(|| {
                    ProviderError::Auth(
                        "Gemini is not logged in, run gemini in a terminal to authenticate".into(),
                    )
                })?;
            let (new_access, new_id) = refresh_access_token(&ctx.http, refresh, &dir).await?;
            access = Some(new_access);
            if new_id.is_some() {
                id_token = new_id;
            }
        }
        let access = access.ok_or_else(|| ProviderError::Auth("Gemini is not logged in".into()))?;

        let claims = claims_from_id_token(id_token.as_deref());
        let status = load_code_assist(&ctx.http, &access).await?;
        let project = match status.project {
            Some(p) => Some(p),
            None => discover_project(&ctx.http, &access).await,
        };
        let quotas = retrieve_quota(&ctx.http, &access, project.as_deref()).await?;

        let mut snap = UsageSnapshot::new("gemini");
        snap.primary = window("Pro", quotas.iter().filter(|q| is_pro(&q.model)));
        snap.secondary = window("Flash", quotas.iter().filter(|q| is_flash(&q.model)));
        snap.tertiary = window(
            "Flash Lite",
            quotas.iter().filter(|q| is_flash_lite(&q.model)),
        );
        // Row 35. Same as Claude: the id_token email is the only identity available, and
        // it already travelled in `account`.
        snap.account_key = claims.email.clone();
        snap.account = claims.email;
        snap.plan = account_plan(
            status.tier.as_deref(),
            claims.hosted_domain.as_deref(),
            status.paid_tier_name.as_deref(),
        );
        Ok(snap)
    }
}

/// `settings.json` -> `security.auth.selectedType`. Only Google OAuth is supported.
fn unsupported_auth(dir: &Path) -> Option<&'static str> {
    let settings: Value = read_json_file(dir.join("settings.json")).ok()?;
    match settings.pointer("/security/auth/selectedType")?.as_str()? {
        "api-key" | "gemini-api-key" => Some("API key"),
        "vertex-ai" => Some("Vertex AI"),
        _ => None,
    }
}

/// Returns (access_token, id_token) and persists both back into oauth_creds.json.
async fn refresh_access_token(
    http: &reqwest::Client,
    refresh_token: &str,
    dir: &Path,
) -> Result<(String, Option<String>), ProviderError> {
    let (client_id, client_secret) = oauth_client()?;
    let response = http
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        check_consumer_tier(&body)?;
        return Err(ProviderError::Auth(format!(
            "Gemini token refresh failed with HTTP {}",
            status.as_u16()
        )));
    }

    let json: Value = serde_json::from_str(&body)
        .map_err(|e| ProviderError::Parse(format!("token refresh response: {e}")))?;
    let access = json["access_token"]
        .as_str()
        .ok_or_else(|| ProviderError::Parse("token refresh response has no access_token".into()))?
        .to_string();
    let id_token = json["id_token"].as_str().map(str::to_string);
    persist_refreshed(dir, &json);
    Ok((access, id_token))
}

/// Best effort: a read-only credentials file must not fail the whole fetch.
fn persist_refreshed(dir: &Path, refreshed: &Value) {
    let path = dir.join("oauth_creds.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(mut stored) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    let Some(map) = stored.as_object_mut() else {
        return;
    };
    if let Some(token) = refreshed.get("access_token") {
        map.insert("access_token".into(), token.clone());
    }
    if let Some(id_token) = refreshed.get("id_token") {
        map.insert("id_token".into(), id_token.clone());
    }
    if let Some(expires_in) = refreshed["expires_in"].as_f64() {
        let expiry_ms = (Utc::now().timestamp() as f64 + expires_in) * 1000.0;
        map.insert("expiry_date".into(), json!(expiry_ms));
    }
    let Ok(bytes) = serde_json::to_vec_pretty(&stored) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

async fn load_code_assist(
    http: &reqwest::Client,
    access: &str,
) -> Result<CodeAssistStatus, ProviderError> {
    let body = json!({"metadata": {"ideType": "GEMINI_CLI", "pluginType": "GEMINI"}});
    let Ok(response) = http
        .post(LOAD_CODE_ASSIST)
        .bearer_auth(access)
        .json(&body)
        .send()
        .await
    else {
        // Tier and project are optional context, the quota call still works without them.
        return Ok(CodeAssistStatus::default());
    };
    let ok = response.status().is_success();
    let text = response.text().await.unwrap_or_default();
    if !ok {
        check_consumer_tier(&text)?;
        return Ok(CodeAssistStatus::default());
    }
    let Ok(json) = serde_json::from_str::<Value>(&text) else {
        return Ok(CodeAssistStatus::default());
    };

    let project = json["cloudaicompanionProject"]
        .as_str()
        .or_else(|| {
            json.pointer("/cloudaicompanionProject/id")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            json.pointer("/cloudaicompanionProject/projectId")
                .and_then(Value::as_str)
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Ok(CodeAssistStatus {
        tier: json
            .pointer("/currentTier/id")
            .and_then(Value::as_str)
            .map(str::to_string),
        project,
        paid_tier_name: json
            .pointer("/paidTier/name")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
    })
}

/// Fallback used when loadCodeAssist reports no managed project.
async fn discover_project(http: &reqwest::Client, access: &str) -> Option<String> {
    let response = http
        .get(PROJECTS_ENDPOINT)
        .bearer_auth(access)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let json: Value = response.json().await.ok()?;
    for project in json["projects"].as_array()? {
        let Some(id) = project["projectId"].as_str() else {
            continue;
        };
        if id.starts_with("gen-lang-client")
            || project.pointer("/labels/generative-language").is_some()
        {
            return Some(id.to_string());
        }
    }
    None
}

#[derive(Debug)]
struct ModelQuota {
    model: String,
    percent_left: f64,
    resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaBucket {
    model_id: Option<String>,
    remaining_fraction: Option<f64>,
    reset_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QuotaResponse {
    buckets: Option<Vec<QuotaBucket>>,
}

async fn retrieve_quota(
    http: &reqwest::Client,
    access: &str,
    project: Option<&str>,
) -> Result<Vec<ModelQuota>, ProviderError> {
    let body = match project {
        Some(p) => json!({ "project": p }),
        None => json!({}),
    };
    let response = http
        .post(RETRIEVE_QUOTA)
        .bearer_auth(access)
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    // Row 23. Off the response before the body consumes it, so a 429 that named its own
    // wait gets that wait instead of the flat default.
    let retry_after = super::retry_after_of(&response);
    let text = response.text().await?;
    if status == reqwest::StatusCode::UNAUTHORIZED {
        check_consumer_tier(&text)?;
        return Err(ProviderError::Auth("Gemini is not logged in".into()));
    }
    if !status.is_success() {
        check_consumer_tier(&text)?;
        return Err(if status.as_u16() == 429 {
            ProviderError::RateLimited { retry_after }
        } else {
            ProviderError::Http(format!(
                "Gemini quota request failed with HTTP {}",
                status.as_u16()
            ))
        });
    }

    let parsed: QuotaResponse = serde_json::from_str(&text)
        .map_err(|e| ProviderError::Parse(format!("quota response: {e}")))?;
    let buckets = parsed
        .buckets
        .filter(|b| !b.is_empty())
        .ok_or_else(|| ProviderError::Parse("no quota buckets in response".into()))?;

    // One model reports several buckets (input, output, requests). Keep the tightest.
    let mut lowest: HashMap<String, (f64, Option<String>)> = HashMap::new();
    for bucket in buckets {
        let (Some(model), Some(fraction)) = (bucket.model_id, bucket.remaining_fraction) else {
            continue;
        };
        let entry = lowest
            .entry(model)
            .or_insert((fraction, bucket.reset_time.clone()));
        if fraction < entry.0 {
            *entry = (fraction, bucket.reset_time);
        }
    }

    let mut quotas: Vec<ModelQuota> = lowest
        .into_iter()
        .map(|(model, (fraction, reset))| ModelQuota {
            model: model.to_lowercase(),
            percent_left: fraction * 100.0,
            resets_at: reset.as_deref().and_then(parse_reset),
        })
        .collect();
    quotas.sort_by(|a, b| a.model.cmp(&b.model));
    Ok(quotas)
}

fn parse_reset(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn is_flash_lite(model: &str) -> bool {
    model.contains("flash-lite")
}

fn is_flash(model: &str) -> bool {
    model.contains("flash") && !is_flash_lite(model)
}

fn is_pro(model: &str) -> bool {
    model.contains("pro")
}

/// Lowest quota of a model tier as a window. Gemini quotas roll over daily.
fn window<'a>(label: &str, quotas: impl Iterator<Item = &'a ModelQuota>) -> Option<UsageWindow> {
    let lowest = quotas.min_by(|a, b| a.percent_left.total_cmp(&b.percent_left))?;
    Some(UsageWindow::new(
        label,
        Some(100.0 - lowest.percent_left),
        lowest.resets_at,
        Some(1440),
    ))
}

fn claims_from_id_token(id_token: Option<&str>) -> Claims {
    let Some(payload) = id_token.and_then(|t| t.split('.').nth(1)) else {
        return Claims::default();
    };
    let Ok(bytes) =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('='))
    else {
        return Claims::default();
    };
    let Ok(json) = serde_json::from_slice::<Value>(&bytes) else {
        return Claims::default();
    };
    Claims {
        email: json["email"].as_str().map(str::to_string),
        hosted_domain: json["hd"].as_str().map(str::to_string),
    }
}

/// A named paid tier wins over the tier id, matching the Gemini CLI contract.
fn account_plan(
    tier: Option<&str>,
    hosted_domain: Option<&str>,
    paid: Option<&str>,
) -> Option<String> {
    if let Some(paid) = paid {
        return Some(paid.to_string());
    }
    match (tier, hosted_domain) {
        (Some("standard-tier"), _) => Some("Paid".into()),
        (Some("free-tier"), Some(_)) => Some("Workspace".into()),
        (Some("free-tier"), None) => Some("Free".into()),
        (Some("legacy-tier"), _) => Some("Legacy".into()),
        _ => None,
    }
}

/// Google shut the consumer Gemini Code Assist tier down, the responses are recognizable.
fn check_consumer_tier(body: &str) -> Result<(), ProviderError> {
    let text = body.to_lowercase();
    let deprecated = text.contains("unsupported_client")
        || text.contains("ineligibletiererror")
        || (text.contains("no longer supported") && text.contains("gemini code assist"));
    if deprecated {
        return Err(ProviderError::Auth(
            "Google no longer supports this Gemini Code Assist tier for the CLI".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: the first scrape took the first client id in the file, which in the
    /// real CLI bundle belongs to another Google product and sits 6.6 MB from the secret.
    /// Every token refresh then failed with HTTP 401. The right id is the nearest one.
    #[test]
    fn the_client_id_paired_with_the_secret_is_the_nearest_one() {
        let decoy = "111-decoy.apps.googleusercontent.com";
        let real = "681255809395-real.apps.googleusercontent.com";
        let text = format!(
            "var a=\"{decoy}\";{}var id=\"{real}\",secret=\"GOCSPX-realsecret\";",
            "x".repeat(5000)
        );
        let (id, secret) = scrape_pair(&text).expect("pair");
        assert_eq!(id, real);
        assert_eq!(secret, "GOCSPX-realsecret");
    }

    #[test]
    fn a_file_without_a_secret_yields_nothing() {
        assert!(scrape_pair("var id=\"1-x.apps.googleusercontent.com\";").is_none());
    }

    #[test]
    fn model_tiers_are_disjoint() {
        assert!(is_pro("gemini-2.5-pro"));
        assert!(is_flash("gemini-2.5-flash"));
        assert!(!is_flash("gemini-2.5-flash-lite"));
        assert!(is_flash_lite("gemini-2.5-flash-lite"));
    }

    #[test]
    fn window_takes_the_lowest_quota_and_clamps() {
        let quotas = [
            ModelQuota {
                model: "gemini-2.5-flash".into(),
                percent_left: 80.0,
                resets_at: None,
            },
            ModelQuota {
                model: "gemini-2.5-flash-002".into(),
                percent_left: 12.5,
                resets_at: None,
            },
        ];
        let w = window("Flash", quotas.iter()).unwrap();
        assert_eq!(w.used_percent, Some(87.5));
        assert_eq!(w.window_minutes, Some(1440));
    }

    #[test]
    fn plan_prefers_paid_tier_name() {
        assert_eq!(
            account_plan(Some("free-tier"), None, Some("Google AI Pro")).as_deref(),
            Some("Google AI Pro")
        );
        assert_eq!(
            account_plan(Some("free-tier"), Some("acme.com"), None).as_deref(),
            Some("Workspace")
        );
        assert_eq!(account_plan(None, None, None), None);
    }

    #[test]
    fn consumer_tier_signal_is_detected() {
        assert!(check_consumer_tier("{\"error\":\"unsupported_client\"}").is_err());
        assert!(check_consumer_tier("{\"buckets\":[]}").is_ok());
    }

    /// Hits the real Cloud Code API with the local Gemini CLI login.
    /// cargo test -p agentsbar gemini_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn gemini_live() {
        let ctx = FetchContext {
            http: reqwest::Client::new(),
            config: Config::default(),
        };
        let snap = Gemini.fetch(&ctx).await.expect("gemini fetch");
        println!(
            "plan={:?} has_account={} primary={:?} secondary={:?} tertiary={:?}",
            snap.plan,
            snap.account.is_some(),
            snap.primary,
            snap.secondary,
            snap.tertiary
        );
        assert!(snap.primary.is_some() || snap.secondary.is_some() || snap.tertiary.is_some());
    }
}
