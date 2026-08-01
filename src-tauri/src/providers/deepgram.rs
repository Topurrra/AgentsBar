//! Ported from Sources/CodexBarCore/Providers/Deepgram/DeepgramUsageFetcher.swift:
//! list projects, then sum each project's usage breakdown.
//!
//! Deepgram publishes no quota or limit through this API, so there is no percentage
//! window to draw. The usage totals land in `plan` (the only free-text field in the
//! snapshot) and the project label in `account`.
//! ponytail: `plan` doubles as a usage line here; give the snapshot a real detail
//! field if a second provider ever needs one.

use async_trait::async_trait;
use serde::Deserialize;

use super::api_token::{api_key, get_json, has_api_key, Auth};
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot};
use crate::config::Config;

const BASE: &str = "https://api.deepgram.com/v1";

pub struct Deepgram;

#[derive(Debug, Deserialize)]
struct ProjectsResponse {
    #[serde(default)]
    projects: Vec<Project>,
}

#[derive(Debug, Deserialize)]
struct Project {
    project_id: String,
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct UsageResponse {
    #[serde(default)]
    results: Vec<UsageResult>,
}

#[derive(Debug, Default, Deserialize)]
struct UsageResult {
    #[serde(default)]
    hours: f64,
    #[serde(default)]
    total_hours: f64,
    #[serde(default)]
    requests: i64,
}

#[derive(Debug, Default, PartialEq)]
struct Totals {
    hours: f64,
    total_hours: f64,
    requests: i64,
}

fn totals(response: &UsageResponse) -> Totals {
    response
        .results
        .iter()
        .fold(Totals::default(), |acc, r| Totals {
            hours: acc.hours + r.hours,
            total_hours: acc.total_hours + r.total_hours,
            requests: acc.requests + r.requests,
        })
}

fn usage_line(totals: &Totals) -> String {
    let hours = if totals.total_hours > 0.0 {
        totals.total_hours
    } else {
        totals.hours
    };
    if hours > 0.0 {
        format!("{} req, {:.1} h", totals.requests, hours)
    } else {
        format!("{} req", totals.requests)
    }
}

fn account_label(projects: &[Project]) -> Option<String> {
    match projects {
        [] => None,
        [only] => Some(
            only.name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(&only.project_id)
                .to_string(),
        ),
        many => Some(format!("{} projects", many.len())),
    }
}

#[async_trait]
impl Provider for Deepgram {
    fn id(&self) -> &'static str {
        "deepgram"
    }

    fn name(&self) -> &'static str {
        "Deepgram"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn doc_url(&self) -> &'static str {
        "https://console.deepgram.com"
    }

    fn env_key(&self) -> Option<&'static str> {
        Some("DEEPGRAM_API_KEY")
    }

    fn is_configured(&self, config: &Config) -> bool {
        has_api_key(config, self)
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let key = api_key(&ctx.config, self)?;
        let header = format!("Token {key}");
        let auth = Auth::Header("Authorization", &header);

        let projects: ProjectsResponse =
            get_json(&ctx.http, &format!("{BASE}/projects"), &auth, &[]).await?;
        if projects.projects.is_empty() {
            return Err(ProviderError::Auth(
                "no Deepgram projects for this key".into(),
            ));
        }

        let mut summed = Totals::default();
        for project in &projects.projects {
            let url = format!("{BASE}/projects/{}/usage/breakdown", project.project_id);
            let usage: UsageResponse = get_json(&ctx.http, &url, &auth, &[]).await?;
            let project_totals = totals(&usage);
            summed = Totals {
                hours: summed.hours + project_totals.hours,
                total_hours: summed.total_hours + project_totals.total_hours,
                requests: summed.requests + project_totals.requests,
            };
        }

        let mut snapshot = UsageSnapshot::new(self.id());
        snapshot.account = account_label(&projects.projects);
        snapshot.plan = Some(usage_line(&summed));
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_every_result_bucket() {
        let usage: UsageResponse = serde_json::from_str(
            r#"{"start":"2026-01-01","end":"2026-01-31","results":[
                {"hours":1.5,"total_hours":2.0,"requests":10},
                {"hours":0.5,"total_hours":1.0,"requests":5}
            ]}"#,
        )
        .unwrap();
        let summed = totals(&usage);
        assert_eq!(summed.requests, 15);
        assert_eq!(summed.total_hours, 3.0);
        assert_eq!(usage_line(&summed), "15 req, 3.0 h");
    }

    #[test]
    fn empty_results_read_as_zero_not_as_an_error() {
        let usage: UsageResponse = serde_json::from_str(r#"{"results":[]}"#).unwrap();
        assert_eq!(totals(&usage), Totals::default());
        assert_eq!(usage_line(&totals(&usage)), "0 req");
    }

    #[test]
    fn account_label_prefers_a_named_single_project() {
        let response: ProjectsResponse =
            serde_json::from_str(r#"{"projects":[{"project_id":"abc","name":"Voice bot"}]}"#)
                .unwrap();
        assert_eq!(
            account_label(&response.projects).as_deref(),
            Some("Voice bot")
        );

        let response: ProjectsResponse = serde_json::from_str(
            r#"{"projects":[{"project_id":"abc"},{"project_id":"def","name":"Other"}]}"#,
        )
        .unwrap();
        assert_eq!(
            account_label(&response.projects).as_deref(),
            Some("2 projects")
        );
    }
}
