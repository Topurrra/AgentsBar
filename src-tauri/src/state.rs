use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::Config;
pub use crate::health::{HealthLog, HealthPoint};
pub use crate::history::{History, Sample};
pub use crate::providers::{
    AuthKind, FetchContext, Provider, ProviderError, ProviderInfo, UsageSnapshot, UsageWindow,
};

pub struct AppState {
    pub snapshots: RwLock<HashMap<String, UsageSnapshot>>,
    pub config: RwLock<Config>,
    /// Sparkline samples, loaded from disk at startup and appended after each refresh.
    pub history: RwLock<History>,
    /// Provider reliability (down/up transitions), loaded from disk at startup and
    /// appended whenever a provider's status changes after a refresh.
    pub health: RwLock<HealthLog>,
    /// Row 23. Per provider "do not call before": set on a rate limit or a server error,
    /// cleared on success. Deliberately not persisted, since a restart is itself a fresh
    /// intent to fetch, and deliberately keyed by provider rather than shared, because a
    /// batch-wide backoff would need a contract for partial success that we do not have.
    skip_until: RwLock<HashMap<String, DateTime<Utc>>>,
    /// Behind a lock so a proxy change in Settings can swap in a rebuilt client without
    /// restarting the app. `reqwest::Client` is an Arc internally, so cloning it out of
    /// the read lock is cheap and every in-flight batch keeps the client it started with.
    pub http: RwLock<reqwest::Client>,
}

/// Build the shared HTTP client, routing through `proxy` when one is configured.
fn build_http_client(proxy: Option<&str>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("AgentsBar/", env!("CARGO_PKG_VERSION")))
        // These requests carry imported browser cookies and API keys, so a redirect
        // that leaves the original origin is an exfiltration path. Follow only HTTPS
        // redirects that keep the same scheme, host and port; refuse the rest rather
        // than trusting reqwest to strip a manually set Cookie header.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let next = attempt.url();
            let same_origin = attempt.previous().last().is_some_and(|prev| {
                next.scheme() == "https"
                    && prev.scheme() == next.scheme()
                    && prev.host_str() == next.host_str()
                    && prev.port_or_known_default() == next.port_or_known_default()
            });
            if !same_origin {
                // Ends the chain and returns the 3xx itself, so a provider that
                // genuinely needs an off-origin hop fails visibly instead of sending
                // the credential onward.
                attempt.stop()
            } else if attempt.previous().len() > 5 {
                attempt.error("too many redirects")
            } else {
                attempt.follow()
            }
        }));
    if let Some(url) = proxy {
        // `Proxy::all` covers both http and https targets. A bad URL is a config mistake
        // the user can fix, so fall back to a direct connection rather than refusing to
        // build the whole client; the fetch will then fail visibly per provider.
        match reqwest::Proxy::all(url) {
            Ok(p) => builder = builder.proxy(p),
            Err(e) => log::warn!("ignoring invalid proxy {url:?}: {e}"),
        }
    }
    builder.build().expect("failed to build http client")
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let http = build_http_client(config.proxy_url.as_deref());
        Self {
            snapshots: RwLock::new(HashMap::new()),
            config: RwLock::new(config),
            history: RwLock::new(History::load()),
            health: RwLock::new(HealthLog::load()),
            skip_until: RwLock::new(HashMap::new()),
            http: RwLock::new(http),
        }
    }

    /// Swap in a client rebuilt for the given proxy. Called from `set_config` when the
    /// proxy field changes, so the new route applies from the very next refresh.
    pub async fn set_proxy(&self, proxy: Option<&str>) {
        *self.http.write().await = build_http_client(proxy);
    }

    // ------------------------------------------------------------- row 23, backoff

    /// Whether the periodic loop should leave `id` alone for now.
    ///
    /// A manual refresh must NOT consult this: being stuck behind a cooldown with no way
    /// to force a retry is worse than the extra request, and it is the first thing a user
    /// does when a tile looks wrong.
    pub async fn is_skipped(&self, id: &str) -> bool {
        self.skip_until
            .read()
            .await
            .get(id)
            .is_some_and(|at| *at > Utc::now())
    }

    /// Hold `id` back until `until`. An earlier deadline never shortens one already set,
    /// so a provider that keeps failing cannot walk its own cooldown backwards.
    pub async fn skip_provider(&self, id: &str, until: DateTime<Utc>) {
        let mut map = self.skip_until.write().await;
        let entry = map.entry(id.to_string()).or_insert(until);
        *entry = (*entry).max(until);
    }

    /// Clear the cooldown. Called on any success.
    pub async fn clear_skip(&self, id: &str) {
        self.skip_until.write().await.remove(id);
    }

    /// Deadlines still in force, for the diagnostics report and for tests.
    pub async fn skips(&self) -> Vec<(String, DateTime<Utc>)> {
        let now = Utc::now();
        let mut live: Vec<(String, DateTime<Utc>)> = self
            .skip_until
            .read()
            .await
            .iter()
            .filter(|(_, at)| **at > now)
            .map(|(id, at)| (id.clone(), *at))
            .collect();
        live.sort();
        live
    }

    /// Append a sample per successful snapshot and persist if anything changed.
    /// Call after a refresh has stored its snapshots.
    pub async fn record_history(&self, snapshots: &[UsageSnapshot]) {
        // The dedup interval has to be the cadence actually in use. Under adaptive,
        // `refresh_minutes` is only the fixed interval sitting underneath: an idle machine
        // batches every 30 minutes, every gap would exceed a 5 minute window, and an
        // unchanged value would append forever, silently stretching what 2016 samples
        // cover from a week to several.
        let refresh_secs = {
            let config = self.config.read().await;
            crate::scheduler::max_cadence_secs(&config)
        };
        let mut history = self.history.write().await;
        let mut changed = false;
        for snapshot in snapshots {
            changed |= history.record(snapshot, refresh_secs);
        }
        if changed {
            if let Err(e) = history.save() {
                log::warn!("history save failed: {e}");
            }
        }
    }

    /// Record a provider's health outcome, persisting only when its status actually
    /// changed (see [`HealthLog::record`]). Call after a refresh has stored the snapshot,
    /// with the health of the STORED snapshot so a graced blip counts as healthy.
    pub async fn record_health(&self, provider_id: &str, ok: bool) {
        let mut health = self.health.write().await;
        if health.record(provider_id, ok) {
            if let Err(e) = health.save() {
                log::warn!("health save failed: {e}");
            }
        }
    }

    /// Snapshots sorted by registry order, for the frontend and the tray.
    ///
    /// Disabled providers are dropped here as well as pruned after a refresh: a snapshot
    /// can also arrive from [`crate::scheduler::refresh_one`], which is an explicit user
    /// action and ignores the enabled flag. Without this filter that snapshot would go on
    /// driving the tray icon and tooltip for a provider the user turned off.
    pub async fn snapshots_in_order(&self) -> Vec<UsageSnapshot> {
        let map = self.snapshots.read().await;
        let config = self.config.read().await;
        crate::providers::all_providers()
            .iter()
            .filter(|p| config.is_enabled(p.id()))
            .filter_map(|p| map.get(p.id()).cloned())
            .collect()
    }

    pub async fn fetch_context(&self) -> FetchContext {
        FetchContext {
            http: self.http.read().await.clone(),
            config: self.config.read().await.clone(),
        }
    }
}

/// Path the CLI `status` command reads: the current display snapshots, written on every
/// refresh so the CLI shows the same numbers as the tray without talking to the app.
pub fn snapshots_path() -> std::path::PathBuf {
    crate::config::dir().join("snapshots.json")
}

/// Write the display snapshots for the CLI. Atomic temp-plus-rename, the same discipline
/// as config and history. A failure is logged and dropped: the CLI reading slightly stale
/// data is fine, a failed refresh is not.
pub fn persist_display(display: &[DisplaySnapshot]) {
    let path = snapshots_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    let result = (|| -> std::io::Result<()> {
        std::fs::write(&tmp, serde_json::to_vec(display)?)?;
        std::fs::rename(&tmp, &path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        log::warn!("snapshot persist failed: {e}");
    }
}

// ------------------------------------------------------------- display-time windows

/// A lane as it should be DISPLAYED: the stored [`UsageWindow`] after the
/// weekly-caps-session clamp, plus where the clamp came from.
///
/// Serializes with the same field names as `UsageWindow`, so anything that already reads
/// a window reads this too, and gains one extra key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayWindow {
    pub label: String,
    pub used_percent: Option<f64>,
    pub resets_at: Option<DateTime<Utc>>,
    pub window_minutes: Option<u64>,
    /// Label of the longer, exhausted window that binds this one. `None` when the window
    /// stands on its own. The UI captions a clamped row with "Capped by {label}".
    pub capped_by: Option<String>,
}

/// What the frontend receives: the stored snapshot plus its lanes already clamped.
///
/// The three lane fields still travel because everything else in the payload does, but
/// `windows` is what the tile must render. Row 25 asks for one implementation of the
/// clamp, and this is how the single Rust one reaches JavaScript instead of being
/// re-derived there from the raw lanes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplaySnapshot {
    #[serde(flatten)]
    pub snapshot: UsageSnapshot,
    pub windows: Vec<DisplayWindow>,
    /// Row 35. Which key in the `get_history` map holds this snapshot's samples: the
    /// provider id, or `provider:account` once the provider knows its account. The
    /// frontend must read `history[history_key]`, never `history[provider_id]`, or a
    /// user with an identified account gets an empty sparkline. Derived in Rust so the
    /// rule lives in one place instead of being re-spelled in JavaScript.
    pub history_key: String,
}

impl From<&UsageSnapshot> for DisplaySnapshot {
    fn from(snapshot: &UsageSnapshot) -> Self {
        Self {
            windows: display_windows(snapshot),
            history_key: crate::history::series_key(
                &snapshot.provider_id,
                snapshot.account_key.as_deref(),
            ),
            snapshot: snapshot.clone(),
        }
    }
}

fn is_exhausted(w: &UsageWindow) -> bool {
    w.used_percent.is_some_and(|u| u >= 100.0)
}

/// The snapshot's lanes with the weekly-caps-session clamp applied.
///
/// When a longer window is exhausted it is the binding cap: the shorter window's quota
/// cannot be spent until the longer one resets, however much room the API still claims in
/// it. So a shorter window under an exhausted longer one reads as exhausted and inherits
/// the binding reset. This is upstream Codex's "5h 99% remaining" above "weekly 0%
/// remaining", and it lives in Rust so the tray and the tile cannot disagree about it.
///
/// A window with no `window_minutes` has no comparable duration and is left alone.
pub fn display_windows(snapshot: &UsageSnapshot) -> Vec<DisplayWindow> {
    let lanes: Vec<&UsageWindow> = [&snapshot.primary, &snapshot.secondary, &snapshot.tertiary]
        .into_iter()
        .flatten()
        .collect();

    lanes
        .iter()
        .map(|w| {
            let binder = w.window_minutes.and_then(|mine| {
                lanes
                    .iter()
                    .filter(|o| is_exhausted(o) && o.window_minutes.is_some_and(|m| m > mine))
                    .max_by_key(|o| o.window_minutes.unwrap_or(0))
            });
            DisplayWindow {
                label: w.label.clone(),
                used_percent: match binder {
                    // An unknown percentage under a binding cap is knowable after all:
                    // nothing can be spent, so it is exhausted.
                    Some(_) => Some(w.used_percent.unwrap_or(100.0).max(100.0)),
                    None => w.used_percent,
                },
                resets_at: match binder {
                    Some(b) => binding_reset(w, b),
                    None => w.resets_at,
                },
                window_minutes: w.window_minutes,
                capped_by: binder.map(|b| b.label.clone()),
            }
        })
        .collect()
}

/// A capped window recovers when its cap does, unless it is exhausted in its own right
/// and stays that way for longer.
fn binding_reset(window: &UsageWindow, binder: &UsageWindow) -> Option<DateTime<Utc>> {
    if is_exhausted(window) {
        match (window.resets_at, binder.resets_at) {
            (Some(own), Some(cap)) => Some(own.max(cap)),
            (own, cap) => own.or(cap),
        }
    } else {
        binder.resets_at
    }
}

/// The one window the tray icon and the tooltip speak for: the MOST CONSTRAINED lane.
///
/// Ranking, highest first: known percentages beat unknown ones (an unknown window carries
/// no constraint level and must not read as a healthy 0), then the highest `used_percent`,
/// which puts an exhausted lane on top because the constructor clamps at 100, then the
/// shortest `window_minutes`, so two exhausted lanes resolve to the one you feel first.
/// A complete tie resolves to the earlier lane.
///
/// The old rule was `primary.or(secondary).or(tertiary)`, which hid an exhausted weekly
/// behind a freshly rolled 5h bucket and painted the tray green when nothing could be sent.
pub fn lead_window(snapshot: &UsageSnapshot) -> Option<DisplayWindow> {
    let mut windows = display_windows(snapshot);
    // `max_by` keeps the LAST maximum, so walk the lanes backwards to break a full tie
    // towards primary.
    windows.reverse();
    windows.into_iter().max_by(more_constrained)
}

fn more_constrained(a: &DisplayWindow, b: &DisplayWindow) -> Ordering {
    match (a.used_percent, b.used_percent) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        // Both are finite: the constructor guarantees it.
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal).then_with(|| {
            b.window_minutes
                .unwrap_or(u64::MAX)
                .cmp(&a.window_minutes.unwrap_or(u64::MAX))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(lanes: [Option<UsageWindow>; 3]) -> UsageSnapshot {
        let [primary, secondary, tertiary] = lanes;
        UsageSnapshot {
            primary,
            secondary,
            tertiary,
            ..UsageSnapshot::new("codex")
        }
    }

    fn win(label: &str, used: f64, minutes: u64) -> Option<UsageWindow> {
        Some(UsageWindow::new(label, Some(used), None, Some(minutes)))
    }

    /// Row 1, the headline: the tray must not read green when the weekly is spent.
    #[test]
    fn an_exhausted_weekly_beats_a_fresh_five_hour() {
        let s = snapshot([win("5h", 1.0, 300), win("Weekly", 100.0, 10080), None]);
        let lead = lead_window(&s).expect("a lead window");
        // The 5h lane wins on the tie at 100 because it is the shorter one, but only
        // because the weekly capped it. Either way the tray shows 0 left, not 99.
        assert_eq!(lead.used_percent, Some(100.0));
        assert_eq!(lead.capped_by.as_deref(), Some("Weekly"));
    }

    /// Without a cap in play the most used lane wins wherever it sits.
    #[test]
    fn the_most_used_lane_wins_regardless_of_lane_order() {
        let s = snapshot([win("5h", 12.0, 300), win("Weekly", 80.0, 10080), None]);
        assert_eq!(lead_window(&s).unwrap().label, "Weekly");
        let s = snapshot([win("5h", 91.0, 300), win("Weekly", 80.0, 10080), None]);
        assert_eq!(lead_window(&s).unwrap().label, "5h");
    }

    #[test]
    fn ties_break_towards_the_shortest_window() {
        let s = snapshot([win("Weekly", 60.0, 10080), win("5h", 60.0, 300), None]);
        assert_eq!(lead_window(&s).unwrap().label, "5h");
    }

    /// An unknown percentage is not a healthy zero: it ranks below every known window.
    #[test]
    fn an_unknown_window_never_outranks_a_known_one() {
        let unknown = Some(UsageWindow::new("Session", None, None, Some(300)));
        let s = snapshot([unknown.clone(), win("Weekly", 3.0, 10080), None]);
        assert_eq!(lead_window(&s).unwrap().label, "Weekly");
        // With nothing else it is still the lead, just an unknown one.
        let s = snapshot([unknown, None, None]);
        assert_eq!(lead_window(&s).unwrap().used_percent, None);
        assert!(lead_window(&snapshot([None, None, None])).is_none());
    }

    /// Row 25: the shorter lane shows the cap and the cap's reset, and says so.
    #[test]
    fn a_capped_window_inherits_the_binding_reset() {
        let weekly_reset = Utc::now() + chrono::Duration::hours(30);
        let s = snapshot([
            Some(UsageWindow::new(
                "5h",
                Some(1.0),
                Some(Utc::now() + chrono::Duration::hours(2)),
                Some(300),
            )),
            Some(UsageWindow::new(
                "Weekly",
                Some(100.0),
                Some(weekly_reset),
                Some(10080),
            )),
            None,
        ]);
        let windows = display_windows(&s);
        assert_eq!(windows[0].used_percent, Some(100.0));
        assert_eq!(windows[0].resets_at, Some(weekly_reset));
        assert_eq!(windows[0].capped_by.as_deref(), Some("Weekly"));
        // The binding window itself is never capped.
        assert_eq!(windows[1].capped_by, None);
        assert_eq!(windows[1].resets_at, Some(weekly_reset));
    }

    /// Row 35: the frontend is told where its samples live, so it never has to guess.
    #[test]
    fn the_display_snapshot_carries_the_history_key() {
        let mut s = snapshot([win("5h", 4.0, 300), None, None]);
        assert_eq!(DisplaySnapshot::from(&s).history_key, "codex");
        s.account_key = Some("acct-a".into());
        assert_eq!(
            DisplaySnapshot::from(&s).history_key,
            crate::history::series_key("codex", Some("acct-a")),
            "the wire key is the same hashed series key the file uses"
        );
    }

    /// Row 23 scaffolding. The policy is the scheduler's; this is the state it drives.
    #[test]
    fn a_skip_holds_until_its_deadline_and_never_shortens() {
        let state = AppState::new(Config::default());
        let block_on = |f| {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(f)
        };
        block_on(async {
            assert!(!state.is_skipped("codex").await);

            state
                .skip_provider("codex", Utc::now() + chrono::Duration::minutes(5))
                .await;
            assert!(state.is_skipped("codex").await);
            assert!(!state.is_skipped("claude").await, "one provider only");

            // A shorter deadline must not walk the cooldown backwards.
            state
                .skip_provider("codex", Utc::now() + chrono::Duration::seconds(1))
                .await;
            assert!(state.skips().await[0].1 > Utc::now() + chrono::Duration::minutes(4));

            // An elapsed deadline is not a skip, and success clears it outright.
            state
                .skip_provider("claude", Utc::now() - chrono::Duration::seconds(1))
                .await;
            assert!(!state.is_skipped("claude").await);
            assert_eq!(state.skips().await.len(), 1);

            state.clear_skip("codex").await;
            assert!(!state.is_skipped("codex").await);
            assert!(state.skips().await.is_empty());
        });
    }

    /// A window with no duration cannot be compared, so it is left exactly as it came.
    #[test]
    fn an_undated_window_is_not_capped() {
        let s = snapshot([
            Some(UsageWindow::new("Credits", Some(4.0), None, None)),
            win("Weekly", 100.0, 10080),
            None,
        ]);
        let windows = display_windows(&s);
        assert_eq!(windows[0].used_percent, Some(4.0));
        assert_eq!(windows[0].capped_by, None);
    }
}
