//! Augment credits, ported from CodexBar
//! `Providers/Augment/AugmentStatusProbe.swift` (`AugmentCookieImporter` plus
//! `AugmentStatusProbe.fetchCredits/fetchSubscription`).
//!
//! Cookie domain: augmentcode.com (covers app.augmentcode.com and auth.augmentcode.com)
//! Cookie names: see [`SESSION_NAMES`], the Auth0 / NextAuth / AuthJS session cookies.
//!
//! Augment sends its whole cookie jar rather than named cookies: `SessionInfo.cookieHeader`
//! joins every cookie the domain holds, and the session cookie name varies by which auth
//! stack the account was created under. That is [`Want::Jar`], gated on at least one known
//! session name being present.
//!
//! Never log the header, only cookie names and counts.

use async_trait::async_trait;
use serde::Deserialize;

use super::api_token::{get_json, parse_rfc3339, Auth};
use super::util::percent;
use super::{AuthKind, FetchContext, Provider, ProviderError, UsageSnapshot, UsageWindow, Want};
use crate::config::Config;

const CREDITS_URL: &str = "https://app.augmentcode.com/api/credits";
const SUBSCRIPTION_URL: &str = "https://app.augmentcode.com/api/subscription";
const DOMAINS: [&str; 1] = ["augmentcode.com"];

/// Ported verbatim from `AugmentCookieImporter.sessionCookieNames`. Any one of them
/// means the jar is worth sending.
const SESSION_NAMES: [&str; 11] = [
    "session",
    "_session",
    "web_rpc_proxy_session",
    "auth0",
    "auth0.is.authenticated",
    "a0.spajs.txs",
    "__Secure-next-auth.session-token",
    "next-auth.session-token",
    "__Secure-authjs.session-token",
    "__Host-authjs.csrf-token",
    "authjs.session-token",
];

pub struct Augment;

#[async_trait]
impl Provider for Augment {
    fn id(&self) -> &'static str {
        "augment"
    }

    fn name(&self) -> &'static str {
        "Augment"
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::Cookie
    }

    fn doc_url(&self) -> &'static str {
        "https://app.augmentcode.com"
    }

    fn is_configured(&self, config: &Config) -> bool {
        super::has_cookies(config, self.id(), &DOMAINS, Want::Jar(&SESSION_NAMES))
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        ctx.with_cookies(
            self.id(),
            &DOMAINS,
            Want::Jar(&SESSION_NAMES),
            "Sign in at app.augmentcode.com, or paste a cookie header in Settings",
            |jar| async move {
                let auth = Auth::Header("Cookie", &jar);
                // `get_json` maps 401 and 403 to `Auth`, which is what lets the next
                // browser session be tried.
                let credits: CreditsResponse = get_json(&ctx.http, CREDITS_URL, &auth, &[]).await?;
                // The subscription call only adds the plan name, billing cycle and email,
                // so a failure there must not lose the credits we already have.
                let subscription: Option<SubscriptionResponse> =
                    get_json(&ctx.http, SUBSCRIPTION_URL, &auth, &[]).await.ok();

                Ok(snapshot(&credits, subscription.as_ref()))
            },
        )
        .await
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreditsResponse {
    #[serde(default)]
    usage_units_remaining: Option<f64>,
    #[serde(default)]
    usage_units_consumed_this_billing_cycle: Option<f64>,
    #[serde(default)]
    usage_units_available: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubscriptionResponse {
    #[serde(default)]
    plan_name: Option<String>,
    #[serde(default)]
    billing_period_end: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

impl CreditsResponse {
    /// `usageUnitsAvailable` when the API sends it, otherwise remaining plus consumed.
    fn limit(&self) -> Option<f64> {
        match self.usage_units_available {
            Some(available) if available > 0.0 => return Some(available),
            _ => {}
        }
        Some(self.usage_units_remaining? + self.usage_units_consumed_this_billing_cycle?)
    }

    fn used_percent(&self) -> f64 {
        let Some(limit) = self.limit().filter(|l| *l > 0.0) else {
            return 0.0;
        };
        if let Some(used) = self.usage_units_consumed_this_billing_cycle {
            return percent(used, limit);
        }
        match self.usage_units_remaining {
            Some(remaining) => percent(limit - remaining, limit),
            None => 0.0,
        }
    }
}

fn snapshot(
    credits: &CreditsResponse,
    subscription: Option<&SubscriptionResponse>,
) -> UsageSnapshot {
    let resets_at = subscription
        .and_then(|s| s.billing_period_end.as_deref())
        .and_then(parse_rfc3339);

    let mut snap = UsageSnapshot::new("augment");
    snap.primary = Some(UsageWindow::new(
        "Credits",
        Some(credits.used_percent()),
        resets_at,
        None,
    ));
    snap.credits = credits.usage_units_remaining;
    snap.plan = subscription.and_then(|s| s.plan_name.clone());
    snap.account = subscription.and_then(|s| s.email.clone());
    snap
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credits(json: &str) -> CreditsResponse {
        serde_json::from_str(json).unwrap()
    }

    // The jar building rules (gate on a session name, drop expired rows) now live in
    // cookies::pairs_for and are tested there, once, for every provider that uses them.

    #[test]
    fn consumed_over_available_is_the_used_percent() {
        let snap = snapshot(
            &credits(
                r#"{"usageUnitsRemaining":150,"usageUnitsConsumedThisBillingCycle":450,
                    "usageUnitsAvailable":600,"usageBalanceStatus":"active"}"#,
            ),
            Some(
                &serde_json::from_str(
                    r#"{"planName":"Pro","billingPeriodEnd":"2026-09-01T00:00:00Z",
                    "email":"user@example.com"}"#,
                )
                .unwrap(),
            ),
        );
        let primary = snap.primary.unwrap();
        assert_eq!(primary.used_percent, Some(75.0));
        assert_eq!(
            primary.resets_at.map(|d| d.timestamp()),
            Some(1_788_220_800)
        );
        assert_eq!(snap.credits, Some(150.0));
        assert_eq!(snap.plan.as_deref(), Some("Pro"));
        assert_eq!(snap.account.as_deref(), Some("user@example.com"));
    }

    #[test]
    fn a_missing_available_field_falls_back_to_remaining_plus_consumed() {
        let response =
            credits(r#"{"usageUnitsRemaining":25,"usageUnitsConsumedThisBillingCycle":75}"#);
        assert_eq!(response.limit(), Some(100.0));
        assert_eq!(response.used_percent(), 75.0);

        // Nothing to divide by reads as 0% rather than as NaN.
        let empty = credits("{}");
        assert_eq!(empty.limit(), None);
        assert_eq!(empty.used_percent(), 0.0);
        let zero = credits(r#"{"usageUnitsRemaining":0,"usageUnitsConsumedThisBillingCycle":0}"#);
        assert_eq!(zero.used_percent(), 0.0);
    }

    #[test]
    fn the_subscription_call_is_optional() {
        let snap = snapshot(
            &credits(r#"{"usageUnitsRemaining":40,"usageUnitsAvailable":100}"#),
            None,
        );
        // Only remaining and a limit: 60% used.
        assert_eq!(snap.primary.unwrap().used_percent, Some(60.0));
        assert!(snap.plan.is_none() && snap.account.is_none());
    }

    /// cargo test -p agentbar augment_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs a real app.augmentcode.com browser session"]
    async fn augment_live() {
        let ctx = FetchContext {
            http: reqwest::Client::new(),
            config: Config::default(),
        };
        match Augment.fetch(&ctx).await {
            Ok(snap) => println!(
                "augment: plan={:?} has_account={} primary={:?} credits={:?}",
                snap.plan,
                snap.account.is_some(),
                snap.primary,
                snap.credits
            ),
            Err(e) => println!("augment: {e}"),
        }
    }
}
