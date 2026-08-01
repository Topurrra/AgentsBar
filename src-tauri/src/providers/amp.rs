//! Amp free tier usage, ported from CodexBar `Providers/Amp/`:
//! `AmpUsageFetcher.swift` (`AmpCookieImporter`, `fetchLegacyHTMLWithDiagnostics`),
//! `AmpUsageParser.parse(html:)` and `AmpUsageSnapshot.toUsageSnapshot`.
//!
//! Amp publishes no usage JSON to a browser session: the settings page embeds a
//! `freeTierUsage` object in its HTML, and that is what CodexBar scrapes. The numbers
//! come out of that object by name, so a page restyle cannot silently change them.
//!
//! Cookie domain: ampcode.com
//! Cookie name: session
//!
//! CodexBar's second path, the `AMP_API_KEY` bearer call to
//! `/api/internal?userDisplayBalanceInfo`, is not ported: it returns the same numbers
//! rendered as CLI display text, which needs the whole regex parser to read back.
//!
//! Never log the header, only cookie names and counts.

use async_trait::async_trait;
use chrono::{Duration, Utc};

use super::api_token::TIMEOUT;
use super::util::{http_error, is_login_url, percent, redirect_target};
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow, Want};
use crate::config::Config;

const SETTINGS_URL: &str = "https://ampcode.com/settings";
const DOMAINS: [&str; 1] = ["ampcode.com"];
const NAMES: [&str; 1] = ["session"];

/// The settings page is server rendered for a browser, so it is fetched as one.
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";

pub struct Amp;

#[async_trait]
impl Provider for Amp {
    fn id(&self) -> &'static str {
        "amp"
    }

    fn name(&self) -> &'static str {
        "Amp"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::Cookie
    }

    fn doc_url(&self) -> &'static str {
        "https://ampcode.com"
    }

    fn is_configured(&self, config: &Config) -> bool {
        super::has_cookies(config, self.id(), &DOMAINS, Want::All(&NAMES))
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        ctx.with_cookies(
            self.id(),
            &DOMAINS,
            Want::All(&NAMES),
            SIGNIN_HINT,
            |cookie| async move {
                let response = ctx
                    .http
                    .get(SETTINGS_URL)
                    .header("Cookie", cookie)
                    .header(
                        "Accept",
                        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                    )
                    .header("Accept-Language", "en-US,en;q=0.9")
                    .header("Origin", "https://ampcode.com")
                    .header("Referer", SETTINGS_URL)
                    .header("User-Agent", USER_AGENT)
                    .timeout(TIMEOUT)
                    .send()
                    .await?;

                let status = response.status();
                // A redirect chain that ends on a sign-in page is an expired session, not
                // a 200 worth parsing. It has to be `Auth` so the next browser is tried.
                //
                // Amp bounces a dead session to auth.ampcode.com, which is off origin, so
                // the shared client stops the chain and hands us the 302 itself: the
                // landing URL is still /settings and only `Location` names the login page.
                // Without the second test that reads as a bare `Http(302)`, which keeps a
                // dead session's stale numbers on screen and never tries the next browser.
                let landed_on_login = is_login_url(response.url().as_str())
                    || redirect_target(&response).is_some_and(is_login_url);
                if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                    || landed_on_login
                {
                    return Err(expired());
                }
                if !status.is_success() {
                    return Err(http_error(&response, || {
                        format!("Amp settings page returned HTTP {}", status.as_u16())
                    }));
                }

                let html = response
                    .text()
                    .await
                    .map_err(|e| ProviderError::Http(e.to_string()))?;
                snapshot(&html)
            },
        )
        .await
    }
}

const SIGNIN_HINT: &str = "Sign in at ampcode.com, or paste a cookie header in Settings";

fn expired() -> ProviderError {
    ProviderError::Auth(
        "Amp session cookie is missing or expired, sign in at ampcode.com or paste a cookie \
         header in Settings"
            .into(),
    )
}

/// Amp renders "Sign in" only when the session did not authenticate the page.
fn looks_signed_out(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    ["sign in", "log in", "login", "/login"]
        .iter()
        .any(|needle| lower.contains(needle))
}

fn snapshot(html: &str) -> Result<UsageSnapshot, ProviderError> {
    let usage = free_tier_usage(html).ok_or_else(|| {
        if looks_signed_out(html) {
            expired()
        } else {
            ProviderError::Parse("Amp settings page has no freeTierUsage data".into())
        }
    })?;

    let mut snap = UsageSnapshot::new("amp");
    snap.plan = Some("Amp Free".into());
    let now = Utc::now();
    snap.primary = Some(UsageWindow::at(
        "Amp Free",
        Some(percent(usage.used, usage.quota)),
        // The free tier refills continuously, so it is empty again once the used credits
        // have been replenished at the hourly rate.
        (usage.quota > 0.0 && usage.hourly_replenishment > 0.0)
            .then(|| {
                let seconds = (usage.used / usage.hourly_replenishment * 3600.0).max(0.0);
                now.checked_add_signed(Duration::seconds(seconds as i64))
            })
            .flatten(),
        usage.window_minutes(),
        now,
    ));
    Ok(snap)
}

#[derive(Debug, PartialEq)]
struct FreeTierUsage {
    quota: f64,
    used: f64,
    hourly_replenishment: f64,
    window_hours: Option<f64>,
}

impl FreeTierUsage {
    /// Explicit `windowHours` when the page carries it, otherwise how long a full quota
    /// takes to replenish, exactly as `AmpUsageParser` derives it.
    fn window_minutes(&self) -> Option<u64> {
        let hours = match self.window_hours {
            Some(hours) if hours > 0.0 => hours,
            _ if self.hourly_replenishment > 0.0 => {
                (self.quota / self.hourly_replenishment).round().max(1.0)
            }
            _ => return None,
        };
        Some((hours * 60.0).round() as u64)
    }
}

fn free_tier_usage(html: &str) -> Option<FreeTierUsage> {
    for token in ["freeTierUsage", "getFreeTierUsage"] {
        let Some(object) = extract_object(html, token) else {
            continue;
        };
        let (Some(quota), Some(used), Some(hourly)) = (
            number_for("quota", object),
            number_for("used", object),
            number_for("hourlyReplenishment", object),
        ) else {
            continue;
        };
        return Some(FreeTierUsage {
            quota,
            used,
            hourly_replenishment: hourly,
            window_hours: number_for("windowHours", object),
        });
    }
    None
}

/// The `{ ... }` that follows `token`, brace matched and aware of string literals so a
/// brace inside a quoted value cannot end it early.
fn extract_object<'a>(text: &'a str, token: &str) -> Option<&'a str> {
    let after = text.find(token)? + token.len();
    let start = after + text[after..].find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in text[start..].char_indices() {
        if in_string {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=start + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

/// `key: 12.5` or `"key":12`, where `key` is a whole word. No regex crate needed for
/// four numbers.
fn number_for(key: &str, object: &str) -> Option<f64> {
    let mut from = 0usize;
    while let Some(found) = object[from..].find(key) {
        let start = from + found;
        let end = start + key.len();
        from = end;
        let is_word_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
        if object[..start]
            .chars()
            .next_back()
            .is_some_and(is_word_char)
            || object[end..].chars().next().is_some_and(is_word_char)
        {
            continue;
        }
        let mut rest = object[end..].trim_start();
        // Skip the closing quote of a quoted key.
        rest = rest.strip_prefix('"').unwrap_or(rest).trim_start();
        let Some(value) = rest.strip_prefix(':') else {
            continue;
        };
        let value = value.trim_start();
        let digits: String = value
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if let Ok(number) = digits.parse::<f64>() {
            return Some(number);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like the real settings page, with synthetic numbers.
    const PAGE: &str = r#"<!doctype html><html><body><script>
        window.__data = {"user":{"name":"a{b}c"},
        "freeTierUsage":{"quota":100,"used":25,"hourlyReplenishment":10,"windowHours":10},
        "other":{"quota":999}};
        </script></body></html>"#;

    #[test]
    fn the_free_tier_object_is_read_by_name() {
        let usage = free_tier_usage(PAGE).unwrap();
        assert_eq!(
            usage,
            FreeTierUsage {
                quota: 100.0,
                used: 25.0,
                hourly_replenishment: 10.0,
                window_hours: Some(10.0),
            }
        );
        // The brace inside the quoted name must not end the object early.
        assert!(extract_object(PAGE, "freeTierUsage")
            .unwrap()
            .ends_with('}'));
    }

    #[test]
    fn the_getter_spelling_and_unquoted_keys_also_parse() {
        let page = "var x = getFreeTierUsage() { quota: 40.5, used: 10, hourlyReplenishment: 0 }";
        let usage = free_tier_usage(page).unwrap();
        assert_eq!(usage.quota, 40.5);
        assert_eq!(usage.window_hours, None);
        // No replenishment rate and no window: nothing to claim about a window.
        assert_eq!(usage.window_minutes(), None);
    }

    #[test]
    fn window_minutes_falls_back_to_the_replenishment_rate() {
        let usage = FreeTierUsage {
            quota: 100.0,
            used: 25.0,
            hourly_replenishment: 10.0,
            window_hours: None,
        };
        assert_eq!(usage.window_minutes(), Some(600));
        assert_eq!(
            FreeTierUsage {
                window_hours: Some(24.0),
                ..usage
            }
            .window_minutes(),
            Some(1440)
        );
    }

    #[test]
    fn a_substring_key_is_not_mistaken_for_the_real_one() {
        let object = r#"{"maxQuota":9,"quotaLabel":"x","quota":7}"#;
        assert_eq!(number_for("quota", object), Some(7.0));
        assert_eq!(number_for("missing", object), None);
    }

    #[test]
    fn the_page_maps_to_a_used_percent_and_a_refill_time() {
        let snap = snapshot(PAGE).unwrap();
        let primary = snap.primary.unwrap();
        assert_eq!(primary.used_percent, Some(25.0));
        assert_eq!(primary.window_minutes, Some(600));
        // 25 used at 10 per hour is 2.5 hours to a full refill.
        let seconds = primary.resets_at.unwrap().timestamp() - Utc::now().timestamp();
        assert!(
            (seconds - 9000).abs() <= 2,
            "unexpected refill time {seconds}"
        );
    }

    #[test]
    fn a_sign_in_page_is_an_auth_error_not_a_parse_error() {
        let err = snapshot("<html><body><a href=\"/login\">Sign in</a></body></html>").unwrap_err();
        assert!(matches!(err, ProviderError::Auth(_)));
        // A page that simply lost the usage block is a parse error worth reporting.
        assert!(matches!(
            snapshot("<html><body>Settings</body></html>").unwrap_err(),
            ProviderError::Parse(_)
        ));
    }

    /// The sign-out bounce leaves ampcode.com, so the shared client stops it and the 302
    /// arrives here with the landing URL still on /settings. Only `Location` says login,
    /// and reading it is what keeps a dead session an `Auth` error rather than an
    /// `Http(302)` that hides behind the last good numbers.
    #[test]
    fn a_stopped_sign_out_redirect_is_read_from_its_location_header() {
        let bounce = http::Response::builder()
            .status(302)
            .header(
                "location",
                "https://auth.ampcode.com/authorize?next=/settings",
            )
            .body(Vec::new())
            .unwrap();
        let bounce = reqwest::Response::from(bounce);
        assert!(
            !is_login_url(bounce.url().as_str()),
            "the landing URL alone proves nothing"
        );
        assert!(redirect_target(&bounce).is_some_and(is_login_url));

        // A 200 carries no Location, so nothing here fires on a healthy page.
        let ok = reqwest::Response::from(http::Response::new(Vec::new()));
        assert_eq!(redirect_target(&ok), None);
    }

    /// cargo test -p agentbar amp_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs a real ampcode.com browser session"]
    async fn amp_live() {
        let ctx = FetchContext {
            http: reqwest::Client::new(),
            config: Config::default(),
        };
        match Amp.fetch(&ctx).await {
            Ok(snap) => println!("amp: primary={:?}", snap.primary),
            Err(e) => println!("amp: {e}"),
        }
    }
}
