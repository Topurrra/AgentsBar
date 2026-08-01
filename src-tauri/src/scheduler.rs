use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::providers::{all_providers, provider_by_id, FetchContext, ProviderError, UsageSnapshot};
use crate::state::AppState;

/// Unix seconds of the last completed full refresh, 0 when there was none.
static LAST_REFRESH: AtomicI64 = AtomicI64::new(0);
/// Only one refresh at a time. Codex, Claude and Gemini rotate a single use OAuth
/// refresh token during a fetch, so two overlapping refreshes would race to persist
/// it and leave a dead token behind.
/// ponytail: one global lock, per provider locks only if a slow provider starts
/// holding up the others.
static REFRESH: Mutex<()> = Mutex::const_new(());

const STALE_SECS: i64 = 60;

/// How often the loop compares the wall clock against its deadline. 2,880 no-op
/// comparisons a day, which is 400x less frequent than the idle polling loop CodexBar
/// measured at 0.35 percent CPU.
const TICK: Duration = Duration::from_secs(30);
/// Startup and all-fail backoff, seconds. After the last rung, back to normal cadence.
const RETRY_LADDER: [i64; 4] = [15, 45, 120, 300];
/// Refresh this long after a reset instant, so the provider has actually rolled it.
const BOUNDARY_GRACE_SECS: i64 = 30;
/// Reset instants already chased. Bounded, so a provider echoing one stale `resets_at`
/// forever cannot turn into a refresh storm or an unbounded set.
const CHASED_CAP: usize = 64;

/// Periodic refresh loop.
///
/// The deadline is a wall clock `DateTime<Utc>` polled on a short tick, not a flat
/// `tokio::time::sleep` after the previous batch. That removes cycle drift (a slow batch
/// used to push the next one out by its own duration) and makes suspend-and-resume a
/// non-event: a laptop that wakes past its deadline refreshes on the next tick instead of
/// finishing an eight hour sleep. No power broadcast listener needed.
///
/// The deadline is recomputed from the CURRENT config every tick, so changing
/// `refresh_minutes` takes effect within 30 seconds without anything having to notify us.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        // Anchor of the periodic cadence. Deliberately NOT `LAST_REFRESH`: a popover-open
        // or manual refresh is a freshness event and must not restart the cadence clock.
        let mut cycle_at: Option<DateTime<Utc>> = None;
        let mut fails: usize = 0;
        let mut boundary: Option<DateTime<Utc>> = None;
        let mut chased: VecDeque<i64> = VecDeque::new();
        loop {
            let cadence = {
                let state = app.state::<AppState>();
                let cfg = state.config.read().await;
                cfg.refresh_minutes.max(1).saturating_mul(60) as i64
            };
            let scheduled = match cycle_at {
                None => Utc::now(),
                Some(at) => at + chrono::Duration::seconds(delay_secs(cadence, fails)),
            };
            // Re-checking the cadence can only pull the deadline in, so keep the filter.
            let due = boundary.filter(|b| *b < scheduled).unwrap_or(scheduled);

            let now = Utc::now();
            if now < due {
                // Sleep to the deadline, capped at the tick so a config change, a resume
                // from suspend and a fresh boundary are all noticed within 30 seconds.
                let wait = (due - now).to_std().unwrap_or(TICK).min(TICK);
                tokio::time::sleep(wait).await;
                continue;
            }

            let all_failed = refresh_now(&app).await;
            fails = if all_failed {
                (fails + 1).min(RETRY_LADDER.len() + 1)
            } else {
                0
            };
            let at = Utc::now();
            cycle_at = Some(at);
            boundary = next_boundary(
                &app,
                &mut chased,
                at + chrono::Duration::seconds(delay_secs(cadence, fails)),
            )
            .await;
        }
    });
}

/// Seconds from one cycle to the next: the configured cadence, or a rung of the retry
/// ladder while consecutive batches fail the way a missing network fails.
fn delay_secs(cadence: i64, fails: usize) -> i64 {
    match fails.checked_sub(1).and_then(|rung| RETRY_LADDER.get(rung)) {
        Some(secs) => *secs,
        None => cadence,
    }
}

/// The earliest quota reset still ahead of us, plus a grace period, when it lands before
/// `next_cycle` and we have not already chased it.
///
/// Pulling the deadline forward to a reset boundary is the highest freshness per request
/// in the whole design: without it a window that rolls two minutes after a refresh shows
/// an expired reset for a whole cadence. A boundary further out than the next cycle is
/// left alone and deliberately NOT recorded as chased, because the cycle will cover it
/// and the cadence may still shorten before then.
async fn next_boundary(
    app: &AppHandle,
    chased: &mut VecDeque<i64>,
    next_cycle: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let now = Utc::now();
    let state = app.state::<AppState>();
    let snapshots = state.snapshots.read().await;
    let earliest = snapshots
        .values()
        .flat_map(|s| [&s.primary, &s.secondary, &s.tertiary])
        .flatten()
        .filter_map(|w| w.resets_at)
        .map(|at| at + chrono::Duration::seconds(BOUNDARY_GRACE_SECS))
        .filter(|at| *at > now && *at < next_cycle && !chased.contains(&at.timestamp()))
        .min()?;

    chased.push_back(earliest.timestamp());
    if chased.len() > CHASED_CAP {
        chased.pop_front();
    }
    Some(earliest)
}

/// Fetch every enabled and configured provider concurrently, then publish.
///
/// Returns true when the whole batch failed the way an unavailable network fails: every
/// attempted provider errored and at least one of them transiently. That is the signal
/// the retry ladder waits for; a batch where one provider is merely misconfigured is that
/// provider's problem, not the loop's.
pub async fn refresh_now(app: &AppHandle) -> bool {
    let _guard = REFRESH.lock().await;
    refresh_all_locked(app).await
}

async fn refresh_all_locked(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let http = state.http.clone();
    let config = state.config.read().await.clone();

    // `is_configured` does blocking file IO for the cookie providers (a cold scan copies
    // and decrypts each browser database), so the whole gate runs off the runtime.
    let gate = config.clone();
    let ready: Vec<&'static str> = tauri::async_runtime::spawn_blocking(move || {
        all_providers()
            .iter()
            .filter(|p| gate.is_enabled(p.id()) && p.is_configured(&gate))
            .map(|p| p.id())
            .collect()
    })
    .await
    .unwrap_or_default();

    let mut tasks = JoinSet::new();
    for provider in all_providers() {
        if !ready.contains(&provider.id()) {
            continue;
        }
        let ctx = FetchContext {
            http: http.clone(),
            config: config.clone(),
        };
        tasks.spawn(async move {
            let id = provider.id().to_string();
            (id, provider.fetch(&ctx).await)
        });
    }

    let mut fresh = Vec::new();
    let (mut attempted, mut failed, mut transient) = (0usize, 0usize, false);
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((id, result)) => {
                attempted += 1;
                if let Err(e) = &result {
                    failed += 1;
                    transient |= matches!(e, ProviderError::Http(_));
                }
                fresh.extend(sampleable(&result));
                store(app, &id, result).await;
            }
            Err(e) => log::warn!("provider task failed: {e}"),
        }
    }

    // A provider the user disabled, or one that has lost its credentials, must stop
    // driving the tray icon and tooltip. `ready` is exactly "enabled and configured".
    {
        let state = app.state::<AppState>();
        let mut snapshots = state.snapshots.write().await;
        snapshots.retain(|id, _| ready.contains(&id.as_str()));
    }

    LAST_REFRESH.store(Utc::now().timestamp(), Ordering::Relaxed);
    record_samples(app, fresh);
    publish(app).await;
    attempted > 0 && failed == attempted && transient
}

/// Refresh a single provider, ignoring the enabled flag (explicit user action).
pub async fn refresh_one(app: &AppHandle, id: &str) {
    let Some(provider) = provider_by_id(id) else {
        return;
    };
    let _guard = REFRESH.lock().await;
    let ctx = {
        let state = app.state::<AppState>();
        state.fetch_context().await
    };
    let result = provider.fetch(&ctx).await;
    record_samples(app, sampleable(&result).into_iter().collect());
    store(app, id, result).await;
    publish(app).await;
}

/// Used when the popover opens: only hits the network if the data is old.
///
/// This is a freshness event, not a cadence event: it does not touch the periodic loop's
/// deadline, so opening the popover cannot postpone the next scheduled refresh.
pub async fn refresh_if_stale(app: &AppHandle) {
    // Staleness is rechecked under the lock: reopening the popover while a refresh is
    // still running must wait for it, not queue a second one behind it.
    let _guard = REFRESH.lock().await;
    if Utc::now().timestamp() - LAST_REFRESH.load(Ordering::Relaxed) >= STALE_SECS {
        refresh_all_locked(app).await;
    }
}

async fn store(app: &AppHandle, id: &str, result: Result<UsageSnapshot, ProviderError>) {
    if let Err(e) = &result {
        log::warn!("{id} refresh failed: {e}");
    }
    let state = app.state::<AppState>();
    let mut snapshots = state.snapshots.write().await;
    let merged = merge(id, snapshots.get(id), result);
    snapshots.insert(id.to_string(), merged);
}

/// What a fetch outcome does to the stored snapshot.
///
/// A failure is classified, not blanket-preserved. `Auth` and `NotConfigured` mean the
/// numbers we hold belong to a session or an account we no longer have, so they go: after
/// `claude logout` or an account switch the previous account's percentages must stop being
/// shown as live. `Http` and `Parse` are transient, so the last good windows stay under
/// the error rather than the tile going blank on one 502.
///
/// An `Ok` carrying no windows and no credits is not data either. A provider that answers
/// with nothing (a plan with no quota to report, a partial payload) must not wipe real
/// numbers that have no error to retry from.
fn merge(
    id: &str,
    previous: Option<&UsageSnapshot>,
    result: Result<UsageSnapshot, ProviderError>,
) -> UsageSnapshot {
    match result {
        Ok(snapshot) => match previous {
            Some(prev) if is_empty(&snapshot) && !is_empty(prev) => {
                let mut kept = prev.clone();
                kept.error = None;
                kept
            }
            _ => snapshot,
        },
        Err(e) => {
            let keep = matches!(e, ProviderError::Http(_) | ProviderError::Parse(_));
            let mut snapshot = match previous {
                Some(prev) if keep => prev.clone(),
                _ => UsageSnapshot::new(id),
            };
            snapshot.error = Some(e.to_string());
            snapshot
        }
    }
}

fn is_empty(snapshot: &UsageSnapshot) -> bool {
    snapshot.primary.is_none()
        && snapshot.secondary.is_none()
        && snapshot.tertiary.is_none()
        && snapshot.credits.is_none()
}

/// Only a successful fetch becomes a sparkline sample. A failed fetch keeps the previous
/// snapshot on screen, and re-recording that stale value would draw a flat stretch that
/// never happened.
fn sampleable(result: &Result<UsageSnapshot, ProviderError>) -> Option<UsageSnapshot> {
    result.as_ref().ok().cloned()
}

/// Persist samples off the refresh path: the history write hits the disk and the refresh
/// loop must not wait on it. Concurrent writes serialize on the history lock inside
/// [`AppState::record_history`], so the file cannot tear.
fn record_samples(app: &AppHandle, fresh: Vec<UsageSnapshot>) {
    if fresh.is_empty() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        app.state::<AppState>().record_history(&fresh).await;
    });
}

async fn publish(app: &AppHandle) {
    let state = app.state::<AppState>();
    let snapshots = state.snapshots_in_order().await;
    let config = state.config.read().await.clone();
    crate::tray::update(app, &snapshots, &config);
    // Row 25: same shape as `commands::get_snapshots`, so the tile renders the clamped
    // lanes whether it polled or was pushed to. These two must always change together.
    let display: Vec<crate::state::DisplaySnapshot> = snapshots.iter().map(Into::into).collect();
    if let Err(e) = app.emit("usage-updated", &display) {
        log::warn!("usage-updated emit failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::History;
    use crate::providers::UsageWindow;

    fn ok(used: f64) -> Result<UsageSnapshot, ProviderError> {
        let mut s = UsageSnapshot::new("codex");
        s.primary = Some(UsageWindow::new("5h", Some(used), None, None));
        Ok(s)
    }

    /// The scheduler half of the sampling rule: only fetches that came back Ok reach the
    /// history. History's own tests cover dedup, clamping and the ring cap.
    #[test]
    fn only_successful_fetches_are_sampled() {
        let mut history = History::default();
        let results = [
            ok(12.0),
            Err(ProviderError::Http("502".into())),
            Err(ProviderError::NotConfigured),
        ];
        for result in &results {
            if let Some(snapshot) = sampleable(result) {
                history.record(&snapshot, 300);
            }
        }
        assert_eq!(history.series().len(), 1);
        assert_eq!(history.series()["codex"].len(), 1);
        assert_eq!(history.series()["codex"][0].u, 12.0);
    }

    /// Row 2: the whole point of classifying. A logged out account must not keep showing
    /// its last percentages, and a single 502 must not blank a healthy tile.
    #[test]
    fn auth_failures_clear_the_windows_and_transport_failures_keep_them() {
        let good = ok(40.0).unwrap();

        for keep in [
            ProviderError::Http("502".into()),
            ProviderError::Parse("unexpected field".into()),
        ] {
            let merged = merge("codex", Some(&good), Err(keep));
            assert_eq!(
                merged.primary.and_then(|w| w.used_percent),
                Some(40.0),
                "transient errors keep the last good windows"
            );
            assert!(merged.error.is_some());
        }

        for clear in [
            ProviderError::Auth("refresh token already used".into()),
            ProviderError::NotConfigured,
        ] {
            let expected = clear.to_string();
            let merged = merge("codex", Some(&good), Err(clear));
            assert!(merged.primary.is_none(), "dead credentials clear the data");
            assert!(merged.secondary.is_none());
            assert!(merged.credits.is_none());
            assert_eq!(merged.error.as_deref(), Some(expected.as_str()));
        }
    }

    /// Row 7: a provider answering with nothing is not a reason to lose real numbers.
    #[test]
    fn an_empty_ok_does_not_overwrite_a_good_snapshot() {
        let good = ok(40.0).unwrap();
        let merged = merge("codex", Some(&good), Ok(UsageSnapshot::new("codex")));
        assert_eq!(merged.primary.and_then(|w| w.used_percent), Some(40.0));
        assert!(merged.error.is_none(), "the provider did answer");

        // Credits alone still count as data, and with nothing stored the empty Ok stands.
        let mut credits_only = UsageSnapshot::new("codex");
        credits_only.credits = Some(3.0);
        assert!(!is_empty(&credits_only));
        assert!(merge("codex", None, Ok(UsageSnapshot::new("codex")))
            .primary
            .is_none());
    }

    /// Row 14: 15s, 45s, 120s, 300s, then back to the configured cadence.
    #[test]
    fn the_retry_ladder_climbs_once_and_then_gives_up() {
        assert_eq!(delay_secs(300, 0), 300);
        assert_eq!(delay_secs(300, 1), 15);
        assert_eq!(delay_secs(300, 2), 45);
        assert_eq!(delay_secs(300, 3), 120);
        assert_eq!(delay_secs(300, 4), 300);
        assert_eq!(delay_secs(900, 5), 900, "off the ladder, normal cadence");
        assert_eq!(delay_secs(60, 99), 60);
    }
}
