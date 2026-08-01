//! Windsurf plan status, ported from CodexBar `Providers/Windsurf/`:
//! `WindsurfWebFetcher.swift` (`fetchPlanStatus`, `applyWindsurfHeaders`,
//! `parseManualSessionInput`) and its `WindsurfPlanStatusProtoCodec`.
//!
//! **Manual session bundle only.** Windsurf does not authenticate with cookies at all:
//! `WindsurfDevinSessionImporter` reads four `devin_*` keys out of Chromium's
//! **localStorage** LevelDB for app.devin.ai / windsurf.com, and this wave's cookie layer
//! reads cookie databases, not LevelDB. Adding a LevelDB reader is a whole second
//! storage backend (snappy, a log format, compaction), so the `auto` browser path is not
//! implemented. Paste the bundle in Settings instead: source `manual`, and the header
//! field takes the JSON or `key=value` form that `parseManualSessionInput` accepts, for
//! example
//! `{"devin_session_token":"...","devin_auth1_token":"...","devin_account_id":"...","devin_primary_org_id":"..."}`.
//!
//! The endpoint speaks Connect with a protobuf body, so the two messages are encoded and
//! decoded by hand below. Field numbers are CodexBar's, taken from Windsurf's bundled
//! protobuf metadata, not guessed.
//!
//! Never log the bundle or any token in it.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};

use super::api_token::TIMEOUT;
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow};
use crate::config::Config;

const PLAN_STATUS_URL: &str =
    "https://windsurf.com/_backend/exa.seat_management_pb.SeatManagementService/GetPlanStatus";
const ORIGIN: &str = "https://windsurf.com";
const REFERER: &str = "https://windsurf.com/profile";

const BUNDLE_HINT: &str = "Windsurf needs a session bundle: sign in at windsurf.com, copy \
    devin_session_token, devin_auth1_token, devin_account_id and devin_primary_org_id from \
    the site's local storage, and paste them as JSON in Settings";

pub struct Windsurf;

#[async_trait]
impl Provider for Windsurf {
    fn id(&self) -> &'static str {
        "windsurf"
    }

    fn name(&self) -> &'static str {
        "Windsurf"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::Cookie
    }

    fn doc_url(&self) -> &'static str {
        "https://windsurf.com/subscription"
    }

    fn is_configured(&self, config: &Config) -> bool {
        config.cookie_source(self.id()) != "off"
            && config
                .cookie_header(self.id())
                .and_then(parse_session)
                .is_some()
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let id = self.id();
        if ctx.config.cookie_source(id) == "off" {
            return Err(ProviderError::Auth(
                "Windsurf session import is turned off in Settings".into(),
            ));
        }
        let session = ctx
            .config
            .cookie_header(id)
            .and_then(parse_session)
            .ok_or_else(|| ProviderError::Auth(BUNDLE_HINT.into()))?;

        let response = ctx
            .http
            .post(PLAN_STATUS_URL)
            .header("Content-Type", "application/proto")
            .header("Connect-Protocol-Version", "1")
            .header("Origin", ORIGIN)
            .header("Referer", REFERER)
            .header("x-auth-token", &session.session_token)
            .header("x-devin-session-token", &session.session_token)
            .header("x-devin-auth1-token", &session.auth1_token)
            .header("x-devin-account-id", &session.account_id)
            .header("x-devin-primary-org-id", &session.primary_org_id)
            .body(encode_request(&session.session_token))
            .timeout(TIMEOUT)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            return Err(
                if status.as_u16() == 400 || status.as_u16() == 401 || status.as_u16() == 403 {
                    ProviderError::Auth(format!(
                    "Windsurf rejected the session bundle (HTTP {}), paste a fresh one in Settings",
                    status.as_u16()
                ))
                } else {
                    super::util::http_error(&response, || {
                        format!("Windsurf GetPlanStatus returned HTTP {}", status.as_u16())
                    })
                },
            );
        }

        let body = response
            .bytes()
            .await
            .map_err(|e| ProviderError::Http(e.to_string()))?;
        snapshot(&body)
    }
}

// ------------------------------------------------------------------ session bundle

#[derive(Debug, PartialEq)]
struct Session {
    session_token: String,
    auth1_token: String,
    account_id: String,
    primary_org_id: String,
}

/// JSON object, or `key=value` / `key: value` pairs separated by newlines, commas or
/// semicolons. Same shapes as `WindsurfWebFetcher.parseManualSessionInput`.
fn parse_session(raw: &str) -> Option<Session> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let values = json_pairs(raw).unwrap_or_else(|| text_pairs(raw));
    let pick = |aliases: &[&str]| -> Option<String> {
        aliases
            .iter()
            .find_map(|key| values.get(*key))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    Some(Session {
        session_token: pick(&["devin_session_token", "devinSessionToken", "sessionToken"])?,
        auth1_token: pick(&["devin_auth1_token", "devinAuth1Token", "auth1Token"])?,
        account_id: pick(&[
            "devin_account_id",
            "devinAccountId",
            "accountID",
            "accountId",
        ])?,
        primary_org_id: pick(&[
            "devin_primary_org_id",
            "devinPrimaryOrgId",
            "primaryOrgID",
            "primaryOrgId",
        ])?,
    })
}

fn json_pairs(raw: &str) -> Option<HashMap<String, String>> {
    let object = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    Some(
        object
            .as_object()?
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
            .collect(),
    )
}

fn text_pairs(raw: &str) -> HashMap<String, String> {
    raw.trim_matches(['{', '}'])
        .split(['\n', ',', ';'])
        .filter_map(|segment| {
            let segment = segment.trim();
            let index = segment.find(['=', ':'])?;
            let key = segment[..index].trim().trim_matches(['"', '\'']);
            let value = segment[index + 1..].trim().trim_matches(['"', '\'']);
            (!key.is_empty() && !value.is_empty()).then(|| (key.to_string(), value.to_string()))
        })
        .collect()
}

// ------------------------------------------------------------------ protobuf

/// `GetPlanStatusRequest { 1: auth_token, 2: include_top_up_status }`.
fn encode_request(auth_token: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(auth_token.len() + 8);
    put_varint(&mut out, (1 << 3) | 2);
    put_varint(&mut out, auth_token.len() as u64);
    out.extend_from_slice(auth_token.as_bytes());
    put_varint(&mut out, 2 << 3);
    put_varint(&mut out, 1);
    out
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7F) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Just enough of a protobuf reader for one response message. `None` anywhere means the
/// payload is malformed, which the caller reports as a parse error.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        let mut shift = 0u32;
        while let Some(byte) = self.bytes.get(self.at) {
            self.at += 1;
            value |= u64::from(byte & 0x7F).checked_shl(shift)?;
            if byte & 0x80 == 0 {
                return Some(value);
            }
            shift += 7;
            if shift >= 64 {
                return None;
            }
        }
        None
    }

    fn chunk(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.varint()?).ok()?;
        let end = self.at.checked_add(len)?;
        let out = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(out)
    }

    /// `(field number, wire type)`, or `None` at the end of the message.
    fn field(&mut self) -> Option<(u64, u64)> {
        if self.at >= self.bytes.len() {
            return None;
        }
        let key = self.varint()?;
        Some((key >> 3, key & 0x07))
    }

    /// Consume the body of a field we do not read.
    fn skip(&mut self, wire: u64) -> Option<()> {
        match wire {
            0 => self.varint().map(|_| ()),
            1 => self.advance(8),
            2 => self.chunk().map(|_| ()),
            5 => self.advance(4),
            _ => None,
        }
    }

    fn advance(&mut self, n: usize) -> Option<()> {
        let end = self.at.checked_add(n)?;
        (end <= self.bytes.len()).then(|| self.at = end)
    }
}

#[derive(Debug, Default, PartialEq)]
struct PlanStatus {
    plan_name: Option<String>,
    daily_remaining_percent: Option<u64>,
    weekly_remaining_percent: Option<u64>,
    daily_reset_at: Option<i64>,
    weekly_reset_at: Option<i64>,
}

/// `GetPlanStatusResponse { 1: plan_status }`.
fn decode_response(body: &[u8]) -> Option<PlanStatus> {
    let mut reader = Reader::new(body);
    let mut status = None;
    while let Some((number, wire)) = reader.field() {
        match (number, wire) {
            (1, 2) => status = Some(decode_plan_status(reader.chunk()?)?),
            _ => reader.skip(wire)?,
        }
    }
    status
}

fn decode_plan_status(body: &[u8]) -> Option<PlanStatus> {
    let mut reader = Reader::new(body);
    let mut out = PlanStatus::default();
    while let Some((number, wire)) = reader.field() {
        match (number, wire) {
            (1, 2) => out.plan_name = decode_plan_info(reader.chunk()?),
            (14, 0) => out.daily_remaining_percent = Some(reader.varint()?),
            (15, 0) => out.weekly_remaining_percent = Some(reader.varint()?),
            (17, 0) => out.daily_reset_at = Some(reader.varint()? as i64),
            (18, 0) => out.weekly_reset_at = Some(reader.varint()? as i64),
            _ => reader.skip(wire)?,
        }
    }
    Some(out)
}

/// `PlanInfo { 1: teams_tier, 2: plan_name }`.
fn decode_plan_info(body: &[u8]) -> Option<String> {
    let mut reader = Reader::new(body);
    let mut name = None;
    while let Some((number, wire)) = reader.field() {
        match (number, wire) {
            (2, 2) => {
                name = std::str::from_utf8(reader.chunk()?)
                    .ok()
                    .map(str::to_string)
            }
            _ => reader.skip(wire)?,
        }
    }
    name
}

// ------------------------------------------------------------------ mapping

fn window(label: &str, remaining_percent: u64, reset_at: Option<i64>) -> UsageWindow {
    UsageWindow::new(
        label,
        Some(100.0 - remaining_percent as f64),
        reset_at
            .filter(|secs| *secs > 0)
            .and_then(|secs| Utc.timestamp_opt(secs, 0).single()),
        None,
    )
}

fn snapshot(body: &[u8]) -> Result<UsageSnapshot, ProviderError> {
    let status = decode_response(body).ok_or_else(|| {
        ProviderError::Parse("Windsurf GetPlanStatus response is malformed".into())
    })?;

    let mut snap = UsageSnapshot::new("windsurf");
    snap.plan = status.plan_name.clone();
    snap.primary = status
        .daily_remaining_percent
        .map(|p| window("Daily", p, status.daily_reset_at));
    snap.secondary = status
        .weekly_remaining_percent
        .map(|p| window("Weekly", p, status.weekly_reset_at));
    if snap.primary.is_none() && snap.secondary.is_none() {
        return Err(ProviderError::Parse(
            "Windsurf response carries no daily or weekly quota".into(),
        ));
    }
    Ok(snap)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic response the way the server would. Never a real token.
    fn plan_status_response(
        plan: &str,
        daily: u64,
        weekly: u64,
        daily_reset: u64,
        weekly_reset: u64,
    ) -> Vec<u8> {
        let mut info = Vec::new();
        put_varint(&mut info, 1 << 3); // teams_tier, skipped by the decoder
        put_varint(&mut info, 3);
        put_varint(&mut info, (2 << 3) | 2);
        put_varint(&mut info, plan.len() as u64);
        info.extend_from_slice(plan.as_bytes());

        let mut status = Vec::new();
        put_varint(&mut status, (1 << 3) | 2);
        put_varint(&mut status, info.len() as u64);
        status.extend_from_slice(&info);
        for (number, value) in [
            (12u64, 0u64), // grace_period_status, skipped
            (14, daily),
            (15, weekly),
            (17, daily_reset),
            (18, weekly_reset),
        ] {
            put_varint(&mut status, number << 3);
            put_varint(&mut status, value);
        }

        let mut out = Vec::new();
        put_varint(&mut out, (1 << 3) | 2);
        put_varint(&mut out, status.len() as u64);
        out.extend_from_slice(&status);
        out
    }

    #[test]
    fn the_request_encodes_the_token_and_the_top_up_flag() {
        // field 1, length delimited: 0x0A, len 3, "abc"; field 2, varint: 0x10, 1.
        assert_eq!(
            encode_request("abc"),
            vec![0x0A, 3, b'a', b'b', b'c', 0x10, 1]
        );
    }

    #[test]
    fn remaining_percent_becomes_used_percent() {
        let body = plan_status_response("Pro", 65, 20, 2_000_000_000, 2_001_000_000);
        let snap = snapshot(&body).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Pro"));
        let primary = snap.primary.unwrap();
        assert_eq!(primary.label, "Daily");
        assert_eq!(primary.used_percent, Some(35.0));
        assert_eq!(
            primary.resets_at.map(|d| d.timestamp()),
            Some(2_000_000_000)
        );
        let secondary = snap.secondary.unwrap();
        assert_eq!(secondary.used_percent, Some(80.0));
        assert_eq!(
            secondary.resets_at.map(|d| d.timestamp()),
            Some(2_001_000_000)
        );
    }

    #[test]
    fn unknown_fields_and_missing_resets_are_tolerated() {
        let mut body = plan_status_response("Free", 100, 100, 0, 0);
        // An unknown length delimited field appended at the top level must be skipped.
        put_varint(&mut body, (9 << 3) | 2);
        put_varint(&mut body, 2);
        body.extend_from_slice(b"hi");
        let snap = snapshot(&body).unwrap();
        assert_eq!(snap.primary.unwrap().used_percent, Some(0.0));
        assert!(snap.secondary.unwrap().resets_at.is_none());
    }

    #[test]
    fn a_truncated_or_empty_body_is_a_parse_error() {
        assert!(snapshot(&[]).is_err());
        let body = plan_status_response("Pro", 65, 20, 0, 0);
        assert!(snapshot(&body[..body.len() - 2]).is_err());
        // A well formed message with no quota fields is not a usable snapshot.
        assert!(snapshot(&[0x0A, 0]).is_err());
    }

    #[test]
    fn the_bundle_parses_as_json_or_as_pairs() {
        let expected = Session {
            session_token: "s".into(),
            auth1_token: "a".into(),
            account_id: "acc".into(),
            primary_org_id: "org".into(),
        };
        assert_eq!(
            parse_session(
                r#"{"devin_session_token":"s","devin_auth1_token":"a",
                    "devin_account_id":"acc","devin_primary_org_id":"org"}"#
            ),
            Some(expected)
        );
        assert!(parse_session(
            "devin_session_token=s; devin_auth1_token=a; devin_account_id=acc; \
             devin_primary_org_id=org"
        )
        .is_some());
        // camelCase aliases and a colon separator, one pair per line.
        assert!(
            parse_session("sessionToken: s\nauth1Token: a\naccountId: acc\nprimaryOrgId: org")
                .is_some()
        );
    }

    #[test]
    fn an_incomplete_bundle_is_rejected_rather_than_half_used() {
        assert_eq!(parse_session(""), None);
        assert_eq!(parse_session("   "), None);
        assert_eq!(parse_session("not a bundle"), None);
        // Three of the four keys is still not a session.
        assert_eq!(
            parse_session(
                r#"{"devin_session_token":"s","devin_auth1_token":"a","devin_account_id":"acc"}"#
            ),
            None
        );
        // An empty value is as good as missing.
        assert_eq!(
            parse_session(
                r#"{"devin_session_token":"  ","devin_auth1_token":"a",
                    "devin_account_id":"acc","devin_primary_org_id":"org"}"#
            ),
            None
        );
    }

    /// cargo test -p agentsbar windsurf_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs a Windsurf session bundle pasted in Settings"]
    async fn windsurf_live() {
        let ctx = FetchContext {
            http: reqwest::Client::new(),
            config: Config::load(),
        };
        match Windsurf.fetch(&ctx).await {
            Ok(snap) => println!(
                "windsurf: plan={:?} primary={:?} secondary={:?}",
                snap.plan, snap.primary, snap.secondary
            ),
            Err(e) => println!("windsurf: {e}"),
        }
    }
}
