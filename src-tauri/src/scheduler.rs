use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::config::Config;
use crate::providers::{
    all_providers, provider_by_id, FetchContext, Provider, ProviderError, ProviderErrorKind,
    UsageSnapshot,
};
use crate::state::AppState;

/// Unix seconds of the last completed full refresh, 0 when there was none.
static LAST_REFRESH: AtomicI64 = AtomicI64::new(0);
/// Unix seconds of the last popover open, 0 when it has not been opened this run. The
/// only interaction signal the adaptive cadence has, and the only one worth having: see
/// [`adaptive_secs`].
static POPOVER_AT: AtomicI64 = AtomicI64::new(0);
/// Row 21. Providers whose most recent failure was swallowed by the grace. Membership,
/// not a counter: the only question is "was the previous outcome an already forgiven
/// failure". A `Vec` rather than a `HashSet` because `Vec::new` is const and 23 providers
/// is not a lookup problem.
static GRACED: Mutex<Vec<String>> = Mutex::const_new(Vec::new());
/// Advisor. Providers whose lead window dipped to or below the alert threshold and has
/// not recovered since. Membership, not a counter, like [`GRACED`] and for the same
/// reasons: `Vec::new` is const, and the only question is "did this dip already announce
/// itself". Cleared when a provider recovers above the threshold (or the feature turns
/// off), so the next crossing fires again.
static ALERTED: Mutex<Vec<String>> = Mutex::const_new(Vec::new());
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
/// Row 23. How long a failing provider is left alone when it did not send a `Retry-After`.
/// CodexBar's `ClaudeCLIRateLimitGate` uses the same five minutes.
const BACKOFF_SECS: i64 = 300;
/// Row 23. The single retry, at the provider entry point rather than inside the loop.
/// CodexBar's shipped HTTP policy is `transientIdempotent` with maxRetries = 1 and a 1s
/// base delay; more attempts on a 429 is how you earn a longer ban.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Row 24. Seconds between batches when the cadence is adaptive: `(popover opened within
/// this many seconds, use this delay)`, first match wins.
///
/// Ported from CodexBar's `AdaptiveRefreshPolicyCore` minus the two inputs Windows either
/// cannot supply or should not pay for: thermal pressure has no equivalent, and their
/// local agent transcript scan measured 6.5 to 10.1 CPU-minutes a day to buy 2 extra
/// refreshes out of ~694. Replayed against a frozen 1,780 record trace, the table
/// scheduled 143.88 batches per 24h against 285.90 for fixed 5 minutes: 49.7 percent
/// fewer, slightly better p50 staleness and clearly worse p95 (1093s against 281s). The
/// p95 is what `refresh_if_stale` on popover open exists to hide.
const ADAPTIVE_TABLE: [(i64, i64); 3] =
    [(5 * 60, 2 * 60), (60 * 60, 5 * 60), (4 * 60 * 60, 15 * 60)];
/// Both ends of the adaptive range: no interaction in four hours, and battery saver.
const ADAPTIVE_IDLE_SECS: i64 = 30 * 60;

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
                cadence_secs(&cfg, Utc::now(), popover_at())
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

            // Row 24: adaptive pulls the cadence in the moment the popover opens, which
            // can put the deadline behind us while `refresh_if_stale` is still running the
            // batch it opened. Adopt a refresh that already landed at or after the
            // deadline as this cycle instead of sending a second one seconds later. The
            // anchor only ever moves forward to an instant already past the deadline, so
            // this is not the popover restarting the clock, which stays forbidden.
            let landed = LAST_REFRESH.load(Ordering::Relaxed);
            let at = match Utc
                .timestamp_opt(landed, 0)
                .single()
                .filter(|at| *at >= due)
            {
                Some(at) => at,
                None => {
                    let all_failed = refresh_scheduled(&app).await;
                    fails = if all_failed {
                        (fails + 1).min(RETRY_LADDER.len() + 1)
                    } else {
                        0
                    };
                    Utc::now()
                }
            };
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

// ------------------------------------------------------------------- row 24, cadence

/// Row 24. Whether the adaptive table drives the cadence instead of `refresh_minutes`.
///
/// A user choice, so it lives in `Config`. A fresh install has no `config.json`, gets
/// `Config::default()` and gets adaptive; an existing file has no `refresh_adaptive` key,
/// and the field level `#[serde(default)]` in `config.rs` reads that as false, so it keeps
/// the interval it had. That is the row 24 "fresh installs only" rule, enforced by serde
/// rather than by a migration.
fn is_adaptive(config: &Config) -> bool {
    config.refresh_adaptive
}

/// Seconds from one cycle to the next before the retry ladder gets a say: the user's fixed
/// interval, or the adaptive table. The fixed intervals stay explicit user choices.
fn cadence_secs(config: &Config, now: DateTime<Utc>, last_open: Option<DateTime<Utc>>) -> i64 {
    if is_adaptive(config) {
        adaptive_secs(now, last_open, battery_saver())
    } else {
        config.refresh_minutes.max(1).saturating_mul(60) as i64
    }
}

/// The longest gap the ACTIVE cadence policy can leave between two batches.
///
/// Anything sizing a staleness threshold wants this rather than `refresh_minutes`, which
/// under adaptive is not the interval in use: a fresh install keeps `refresh_minutes` at 5
/// while idling at 30 minute batches, so a threshold built from the fixed number would
/// call a perfectly current tray icon stale forever.
pub fn max_cadence_secs(config: &Config) -> i64 {
    if is_adaptive(config) {
        ADAPTIVE_IDLE_SECS
    } else {
        config.refresh_minutes.max(1).saturating_mul(60) as i64
    }
}

/// [`ADAPTIVE_TABLE`] against a clock, pure so every boundary is a unit test.
///
/// A popover timestamp in the future counts as "just now", matching CodexBar: a clock
/// step must not read as four hours of idleness.
fn adaptive_secs(now: DateTime<Utc>, last_open: Option<DateTime<Utc>>, saver: bool) -> i64 {
    if saver {
        return ADAPTIVE_IDLE_SECS;
    }
    let Some(open) = last_open else {
        return ADAPTIVE_IDLE_SECS;
    };
    let age = (now - open).num_seconds();
    ADAPTIVE_TABLE
        .iter()
        .find(|(within, _)| age <= *within)
        .map_or(ADAPTIVE_IDLE_SECS, |(_, delay)| *delay)
}

fn popover_at() -> Option<DateTime<Utc>> {
    match POPOVER_AT.load(Ordering::Relaxed) {
        0 => None,
        secs => Utc.timestamp_opt(secs, 0).single(),
    }
}

/// Layout is the contract, so every field is declared even though only one is read.
#[repr(C)]
#[allow(dead_code)]
struct SystemPowerStatus {
    ac_line_status: u8,
    battery_flag: u8,
    battery_life_percent: u8,
    system_status_flag: u8,
    battery_life_time: u32,
    battery_full_life_time: u32,
}

// ponytail: six lines of declared FFI instead of a new `windows` crate feature, in a file
// that owns no dependency manifest. This kernel32 signature has been fixed since Windows
// 2000 and `SystemStatusFlag` was appended in place for Windows 10.
extern "system" {
    fn GetSystemPowerStatus(status: *mut SystemPowerStatus) -> i32;
}

/// Windows Battery Saver: `SYSTEM_POWER_STATUS.SystemStatusFlag == 1`, the Windows analogue
/// of the Low Power Mode branch that forces CodexBar's slowest cadence. Thermal pressure,
/// the other half of their branch, has no equivalent here and is deliberately not faked.
///
/// False on any failure. A signal we could not read must never be guessed as "save power",
/// or a desktop with no battery would quietly refresh once every half hour.
fn battery_saver() -> bool {
    let mut status = SystemPowerStatus {
        ac_line_status: 0,
        battery_flag: 0,
        battery_life_percent: 0,
        system_status_flag: 0,
        battery_life_time: 0,
        battery_full_life_time: 0,
    };
    unsafe { GetSystemPowerStatus(&mut status) != 0 && status.system_status_flag == 1 }
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
/// The user asked, so the row 23 cooldowns are bypassed: being stuck behind a timer with
/// no way to force a retry is worse than the extra request, and forcing a retry is the
/// first thing anyone does when a tile looks wrong. Tray menu and command entry point.
pub async fn refresh_now(app: &AppHandle) -> bool {
    let _guard = REFRESH.lock().await;
    refresh_all_locked(app, true).await
}

/// The periodic loop's entry point: honours the cooldowns.
async fn refresh_scheduled(app: &AppHandle) -> bool {
    let _guard = REFRESH.lock().await;
    refresh_all_locked(app, false).await
}

async fn refresh_all_locked(app: &AppHandle, user_initiated: bool) -> bool {
    let state = app.state::<AppState>();
    let http = state.http.read().await.clone();
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

    let due = due_providers(&state, &ready, user_initiated).await;

    let mut tasks = JoinSet::new();
    for provider in all_providers() {
        if !due.contains(&provider.id()) {
            continue;
        }
        let ctx = FetchContext {
            http: http.clone(),
            config: config.clone(),
        };
        tasks.spawn(async move {
            let id = provider.id().to_string();
            (id, fetch_retried(provider.as_ref(), &ctx).await)
        });
    }

    let mut fresh = Vec::new();
    let (mut attempted, mut failed) = (0usize, 0usize);
    // The ids that failed the way a missing network fails, kept rather than counted
    // because the ladder has to hand them back. A rate limit is deliberately NOT one of
    // these: the ladder exists for "the network is down", and a 429 is the opposite, the
    // provider answered, in detail. Letting it climb the ladder would drag every other
    // provider onto a 15 second cadence to be told 429 faster.
    let mut transient: Vec<String> = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((id, result)) => {
                attempted += 1;
                if let Err(e) = &result {
                    failed += 1;
                    if matches!(e, ProviderError::Http(_)) {
                        transient.push(id.clone());
                    }
                }
                fresh.extend(sampleable(&result));
                store(app, &id, result).await;
            }
            Err(e) => log::warn!("provider task failed: {e}"),
        }
    }

    // Row 23 meets the retry ladder. `store` just gave every one of these a five minute
    // cooldown, and with the whole batch behind one the 15/45/120/300 rungs would find
    // nothing due, read that as "not all failed" and reset themselves: recovery from a
    // wifi blip would take five minutes instead of fifteen seconds, and opening the
    // popover could not shorten it either. When the batch failed the way an unavailable
    // network fails, the ladder IS the backoff for those providers, so hand them back to
    // it. A provider that named its own `Retry-After` is a `RateLimited` and is never in
    // this list, so it keeps the cooldown it asked for.
    let network_down = network_down(attempted, failed, transient.len());
    if network_down {
        for id in &transient {
            state.clear_skip(id).await;
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
    network_down
}

/// Whether a batch failed the way an unavailable network fails: everything attempted
/// errored, and at least one of them transiently. A batch where one provider is merely
/// misconfigured is that provider's problem, not the loop's.
fn network_down(attempted: usize, failed: usize, transient: usize) -> bool {
    attempted > 0 && failed == attempted && transient > 0
}

// ------------------------------------------------------------------- row 23, backoff

/// Which of the enabled and configured providers to actually call this pass.
///
/// A cooldown only holds back the periodic loop. `user_initiated` is CodexBar's
/// `interaction != .userInitiated` bypass, and it is the reason a backoff can never strand
/// a provider: the refresh button always goes out.
async fn due_providers(
    state: &AppState,
    ready: &[&'static str],
    user_initiated: bool,
) -> Vec<&'static str> {
    if user_initiated {
        return ready.to_vec();
    }
    let mut due = Vec::with_capacity(ready.len());
    for id in ready {
        if !state.is_skipped(id).await {
            due.push(*id);
        }
    }
    due
}

/// How long to leave a failing provider alone: what it asked for, or five minutes.
///
/// `retry_after` is already parsed and clamped to an hour by `providers::parse_retry_after`,
/// and is `None` for a `Retry-After: 0` or junk, so a 429 always earns a real cooldown.
/// Every failure earns one, not just a rate limit: a provider that is 500ing or whose
/// credentials died gets asked 288 times a day today, and the answer will not have changed
/// in 30 seconds.
fn backoff_until(e: &ProviderError, now: DateTime<Utc>) -> DateTime<Utc> {
    let wait = e
        .retry_after()
        .and_then(|d| chrono::Duration::from_std(d).ok())
        .unwrap_or_else(|| chrono::Duration::seconds(BACKOFF_SECS));
    now + wait
}

/// One retry, one second later, and only for the two answers that mean "ask again".
///
/// At the provider entry point, never inside the loop: CodexBar's `predictive-refresh-policy`
/// notes rule out a scheduler level retry because a partial batch has no defined outcome.
/// A rate limit that named its own longer wait is not retried at all, since coming back in
/// one second after being told to wait a minute is how a cooldown becomes a ban.
async fn fetch_retried(
    provider: &dyn Provider,
    ctx: &FetchContext,
) -> Result<UsageSnapshot, ProviderError> {
    let first = provider.fetch(ctx).await;
    match &first {
        Err(e) if worth_retrying(e) => {
            tokio::time::sleep(RETRY_DELAY).await;
            provider.fetch(ctx).await
        }
        _ => first,
    }
}

fn worth_retrying(e: &ProviderError) -> bool {
    match e {
        ProviderError::RateLimited { retry_after } => {
            retry_after.map_or(true, |d| d <= RETRY_DELAY)
        }
        ProviderError::Http(message) => is_transient_status(message),
        _ => false,
    }
}

/// `ProviderError::Http` carries a message, not a status, and providers spell it several
/// ways ("HTTP 503", "Claude API error 503"), so the status is read back out of the digits.
/// A four digit port or id parses as itself and cannot collide with the set, and the worst
/// a false positive can do is send one extra request a second later.
fn is_transient_status(message: &str) -> bool {
    message
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|token| token.parse::<u16>().ok())
        .any(|code| code == 408 || code == 429 || (500..600).contains(&code))
}

/// Row 21. Whether a failure should be swallowed this once rather than shown.
///
/// Auth and NotConfigured never are: they need the user to sign in or paste a key, and a
/// tile that hides that for a cadence is a tile that lies. Everything else gets exactly one
/// free failure, and only when there is real data still worth showing under it, so a
/// transient 502 does not flash a healthy tile red on its way back to normal.
fn graceable(e: &ProviderError, previous: Option<&UsageSnapshot>) -> bool {
    keeps_windows(e) && previous.is_some_and(|prev| !is_empty(prev))
}

/// Whether the last good windows survive this failure. `Auth` and `NotConfigured` mean the
/// numbers belong to a session we no longer have; the rest are transient.
fn keeps_windows(e: &ProviderError) -> bool {
    matches!(
        e.kind(),
        ProviderErrorKind::Http | ProviderErrorKind::Parse | ProviderErrorKind::RateLimited
    )
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
    let result = fetch_retried(provider.as_ref(), &ctx).await;
    record_samples(app, sampleable(&result).into_iter().collect());
    store(app, id, result).await;
    publish(app).await;
}

/// Used when the popover opens: only hits the network if the data is old.
///
/// This is a freshness event, not a cadence event: it does not touch the periodic loop's
/// deadline, so opening the popover cannot postpone the next scheduled refresh.
pub async fn refresh_if_stale(app: &AppHandle) {
    // Row 24: the one interaction signal the adaptive cadence has. Recorded on every open,
    // including the ones too fresh to fetch, because "the user is looking at this" is true
    // either way. It feeds the delay the loop computes from its existing anchor, so the
    // cadence tightens without the clock restarting.
    POPOVER_AT.store(Utc::now().timestamp(), Ordering::Relaxed);
    // Staleness is rechecked under the lock: reopening the popover while a refresh is
    // still running must wait for it, not queue a second one behind it.
    let _guard = REFRESH.lock().await;
    if Utc::now().timestamp() - LAST_REFRESH.load(Ordering::Relaxed) >= STALE_SECS {
        // Not user_initiated: opening the popover is not asking a rate limited provider to
        // be asked again. The Refresh button is, and it bypasses.
        refresh_all_locked(app, false).await;
    }
}

async fn store(app: &AppHandle, id: &str, result: Result<UsageSnapshot, ProviderError>) {
    let state = app.state::<AppState>();
    // Row 23. Every path that stores an outcome also moves the cooldown, so a manual
    // refresh that succeeds releases the provider at once and one that fails still spares
    // the periodic loop. Done before the snapshot lock: two locks, never nested.
    match &result {
        Ok(_) => state.clear_skip(id).await,
        Err(e) => {
            log::warn!("{id} refresh failed: {e}");
            state.skip_provider(id, backoff_until(e, Utc::now())).await;
        }
    }

    let (notify_on_exhaustion, alert_below, quiet_now) = {
        let cfg = state.config.read().await;
        (
            cfg.notify_on_exhaustion,
            cfg.alert_below_percent,
            cfg.quiet_now(),
        )
    };

    let mut snapshots = state.snapshots.write().await;
    let previous = snapshots.get(id).cloned();
    // Row 21. `graced` is true only for the FIRST consecutive forgivable failure: the push
    // both records the decision and answers whether it had already been made.
    let graced = {
        let mut seen = GRACED.lock().await;
        let already = seen.iter().any(|graced| graced == id);
        match &result {
            Ok(_) => {
                seen.retain(|graced| graced != id);
                false
            }
            Err(e) if !already && graceable(e, previous.as_ref()) => {
                seen.push(id.to_string());
                true
            }
            Err(_) => false,
        }
    };
    let merged = merge(id, previous.as_ref(), result, graced);
    // Health of the STORED snapshot: a graced blip keeps the previous good numbers, so it
    // reads as healthy here exactly as it does on the tile.
    let healthy = merged.error.is_none();

    let notify_info = if notify_on_exhaustion
        && !previous.as_ref().is_some_and(is_lead_exhausted)
        && is_lead_exhausted(&merged)
    {
        let name = provider_by_id(id).map(|p| p.name()).unwrap_or(id);
        let resets_at = crate::state::lead_window(&merged).and_then(|w| w.resets_at);
        Some((name, resets_at))
    } else {
        None
    };

    // Advisor. The exhaustion toast's softer sibling: the same trigger point, a stored
    // snapshot transition, and the same toast mechanism, but it fires when the lead
    // window's remaining quota crosses DOWN through the user's threshold, and it honours
    // quiet hours. Runs even when the feature is off, because the membership record must
    // keep tracking recoveries: a stale "already announced" left behind while the feature
    // was off would swallow the first real crossing after it turns back on.
    let alert_info = {
        let mut alerted = ALERTED.lock().await;
        let fired = advisor_should_fire(
            alert_below,
            quiet_now,
            previous.as_ref(),
            &merged,
            id,
            &mut alerted,
        );
        fired.then(|| {
            let name = provider_by_id(id).map(|p| p.name()).unwrap_or(id);
            let resets_at = crate::state::lead_window(&merged).and_then(|w| w.resets_at);
            (
                name,
                alert_below.expect("an alert only fires with a threshold set"),
                resets_at,
            )
        })
    };

    snapshots.insert(id.to_string(), merged);
    drop(snapshots);

    state.record_health(id, healthy).await;

    if let Some((name, resets_at)) = notify_info {
        notify_exhausted(app, name, resets_at);
    }
    if let Some((name, threshold, resets_at)) = alert_info {
        notify_below_threshold(app, name, threshold, resets_at);
    }
}

/// Whether the binding window of a snapshot is exhausted (0% remaining).
fn is_lead_exhausted(snapshot: &UsageSnapshot) -> bool {
    crate::state::lead_window(snapshot).is_some_and(|w| w.used_percent.is_some_and(|u| u >= 100.0))
}

/// Advisor. Remaining percent of the lead window, when the provider reports one. The
/// threshold math runs on remaining, which is what the user thinks in, not on used.
fn lead_remaining(snapshot: &UsageSnapshot) -> Option<f64> {
    crate::state::lead_window(snapshot)
        .and_then(|w| w.used_percent)
        .map(|used| 100.0 - used)
}

/// Advisor. One provider's alert decision for a stored snapshot, pure so every branch is
/// a unit test. `alerted` is the [`ALERTED`] membership; the call maintains it (records a
/// dip, clears a recovery) and returns whether a toast should fire.
///
/// Firing needs a genuine crossing: the previous remaining ABOVE the threshold and the
/// new remaining at or below it. A first observation already below has nothing it
/// crossed, and a quiet hour consumes the crossing rather than delivering it stale, which
/// is what quiet hours are for. An exhausted lead is exempt: the exhaustion toast owns
/// that transition and fires from the same one.
fn advisor_should_fire(
    threshold: Option<u8>,
    quiet: bool,
    previous: Option<&UsageSnapshot>,
    current: &UsageSnapshot,
    id: &str,
    alerted: &mut Vec<String>,
) -> bool {
    // Off is off, but the membership still tracks recoveries, so turning the feature
    // back on never inherits a stale "already announced".
    let Some(limit) = threshold.map(f64::from) else {
        alerted.retain(|seen| seen != id);
        return false;
    };
    let below = lead_remaining(current).is_some_and(|r| r <= limit);
    if !below || is_lead_exhausted(current) {
        // Above the threshold again, exhausted, or no percent to judge by: re-arm.
        alerted.retain(|seen| seen != id);
        return false;
    }
    if alerted.iter().any(|seen| seen == id) {
        return false; // this dip already announced itself
    }
    alerted.push(id.to_string());
    let crossed = previous.is_some_and(|prev| lead_remaining(prev).is_some_and(|r| r > limit));
    crossed && !quiet
}

/// The countdown phrasing both toasts share: how long until the quota rolls over, or
/// nothing when there is no reset ahead (unknown, or already past).
fn resets_in(resets_at: Option<DateTime<Utc>>) -> Option<String> {
    let secs = resets_at.map(|at| (at - Utc::now()).num_seconds())?;
    if secs <= 0 {
        return None;
    }
    Some(if secs >= 86_400 {
        format!("Resets in {}d {}h.", secs / 86_400, (secs % 86_400) / 3_600)
    } else if secs >= 3_600 {
        format!("Resets in {}h {}m.", secs / 3_600, (secs % 3_600) / 60)
    } else {
        format!("Resets in {}m.", secs / 60)
    })
}

/// Show a Windows toast notification when a provider hits 0%.
fn notify_exhausted(app: &AppHandle, name: &str, resets_at: Option<DateTime<Utc>>) {
    use tauri_plugin_notification::NotificationExt;
    let body = match resets_in(resets_at) {
        Some(countdown) => format!("Usage limit reached. {countdown}"),
        None => "Usage limit reached.".to_string(),
    };
    if let Err(e) = app
        .notification()
        .builder()
        .title(format!("{name} is exhausted"))
        .body(&body)
        .show()
    {
        log::warn!("notification failed: {e}");
    }
}

/// Show a Windows toast when the lead window's remaining quota first crosses below the
/// user's alert threshold. The title carries the number, the body the countdown.
fn notify_below_threshold(
    app: &AppHandle,
    name: &str,
    threshold: u8,
    resets_at: Option<DateTime<Utc>>,
) {
    use tauri_plugin_notification::NotificationExt;
    let body = resets_in(resets_at).unwrap_or_else(|| "Reset time unknown.".to_string());
    if let Err(e) = app
        .notification()
        .builder()
        .title(format!("{name} is below {threshold}%"))
        .body(&body)
        .show()
    {
        log::warn!("notification failed: {e}");
    }
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
///
/// Row 21: `graced` suppresses the message entirely for one failure, so the tile keeps
/// rendering the last good numbers as if nothing had happened. The caller decides that,
/// because it is the one that knows whether this failure is the first of a run.
fn merge(
    id: &str,
    previous: Option<&UsageSnapshot>,
    result: Result<UsageSnapshot, ProviderError>,
    graced: bool,
) -> UsageSnapshot {
    match result {
        Ok(snapshot) => match previous {
            Some(prev) if is_empty(&snapshot) && !is_empty(prev) => {
                let mut kept = prev.clone();
                kept.clear_error();
                kept
            }
            _ => snapshot,
        },
        Err(e) => {
            let mut snapshot = match previous {
                Some(prev) if keeps_windows(&e) => prev.clone(),
                _ => UsageSnapshot::new(id),
            };
            // `set_error` and `clear_error` move the message and the kind together, so the
            // frontend can never read a kind that belongs to a message that is gone.
            if graced {
                snapshot.clear_error();
            } else {
                snapshot.set_error(&e);
            }
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
    // Persist for the CLI `status` command, which reads this file rather than the app.
    crate::state::persist_display(&display);
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
            ProviderError::RateLimited { retry_after: None },
        ] {
            let kind = keep.kind();
            let merged = merge("codex", Some(&good), Err(keep), false);
            assert_eq!(
                merged.primary.and_then(|w| w.used_percent),
                Some(40.0),
                "transient errors keep the last good windows"
            );
            assert!(merged.error.is_some());
            assert_eq!(merged.error_kind, Some(kind), "the kind travels with it");
        }

        for clear in [
            ProviderError::Auth("refresh token already used".into()),
            ProviderError::NotConfigured,
        ] {
            let expected = clear.to_string();
            let merged = merge("codex", Some(&good), Err(clear), false);
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
        let merged = merge("codex", Some(&good), Ok(UsageSnapshot::new("codex")), false);
        assert_eq!(merged.primary.and_then(|w| w.used_percent), Some(40.0));
        assert!(merged.error.is_none(), "the provider did answer");
        assert!(merged.error_kind.is_none());

        // Credits alone still count as data, and with nothing stored the empty Ok stands.
        let mut credits_only = UsageSnapshot::new("codex");
        credits_only.credits = Some(3.0);
        assert!(!is_empty(&credits_only));
        assert!(merge("codex", None, Ok(UsageSnapshot::new("codex")), false)
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

    // ------------------------------------------------------------- row 23, backoff

    /// The provider's own `Retry-After` wins; without one it is five minutes, and a
    /// failure of any kind earns a cooldown.
    #[test]
    fn the_cooldown_honours_retry_after_and_otherwise_defaults_to_five_minutes() {
        let now = Utc::now();
        let default = now + chrono::Duration::seconds(BACKOFF_SECS);

        for no_hint in [
            ProviderError::RateLimited { retry_after: None },
            ProviderError::Http("HTTP 503".into()),
            ProviderError::Auth("expired".into()),
            ProviderError::Parse("junk".into()),
        ] {
            assert_eq!(backoff_until(&no_hint, now), default, "{no_hint}");
        }

        let asked = ProviderError::RateLimited {
            retry_after: Some(Duration::from_secs(90)),
        };
        assert_eq!(
            backoff_until(&asked, now),
            now + chrono::Duration::seconds(90)
        );

        // The header is already clamped to an hour upstream, so the longest a provider can
        // park itself is one cadence-independent hour, not a day.
        let clamped = ProviderError::RateLimited {
            retry_after: Some(crate::providers::MAX_RETRY_AFTER),
        };
        assert_eq!(
            backoff_until(&clamped, now),
            now + chrono::Duration::hours(1)
        );
    }

    /// Row 23. A cooldown holds the loop back and never the user.
    #[test]
    fn a_manual_refresh_bypasses_the_cooldown() {
        let state = AppState::new(crate::config::Config::default());
        let ready = ["codex", "claude"];
        let block_on = |f| {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(f)
        };
        block_on(async {
            assert_eq!(due_providers(&state, &ready, false).await, ready.to_vec());

            let e = ProviderError::RateLimited { retry_after: None };
            state
                .skip_provider("codex", backoff_until(&e, Utc::now()))
                .await;

            assert_eq!(
                due_providers(&state, &ready, false).await,
                vec!["claude"],
                "the periodic loop leaves a rate limited provider alone"
            );
            assert_eq!(
                due_providers(&state, &ready, true).await,
                ready.to_vec(),
                "the user asked, so it goes out anyway"
            );

            state.clear_skip("codex").await;
            assert_eq!(due_providers(&state, &ready, false).await, ready.to_vec());
        });
    }

    /// Row 23 must not disarm the retry ladder. A batch that failed the way a missing
    /// network fails hands its providers back to the ladder instead of parking them behind
    /// the flat five minutes, or rung 1 would fetch nothing, `fails` would reset and the
    /// remaining rungs could never run.
    #[test]
    fn an_all_transient_batch_hands_its_providers_back_to_the_ladder() {
        assert!(network_down(2, 2, 2), "the whole batch, all transiently");
        assert!(network_down(2, 2, 1), "one transient failure is enough");
        assert!(
            !network_down(2, 2, 0),
            "no transient failure is not a network"
        );
        assert!(!network_down(2, 1, 1), "one provider still answered");
        assert!(!network_down(0, 0, 0), "nothing was attempted");

        let state = AppState::new(crate::config::Config::default());
        let ready = ["codex", "claude"];
        let block_on = |f| {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(f)
        };
        block_on(async {
            // What one all-failed batch leaves behind: a cooldown per provider, the 429
            // carrying the length it asked for.
            let http = ProviderError::Http("HTTP 502".into());
            let limited = ProviderError::RateLimited {
                retry_after: Some(Duration::from_secs(900)),
            };
            state
                .skip_provider("codex", backoff_until(&http, Utc::now()))
                .await;
            state
                .skip_provider("claude", backoff_until(&limited, Utc::now()))
                .await;
            assert!(due_providers(&state, &ready, false).await.is_empty());

            // The release the batch performs for its transiently failed providers only.
            state.clear_skip("codex").await;
            assert_eq!(
                due_providers(&state, &ready, false).await,
                vec!["codex"],
                "the next ladder rung must actually fetch something"
            );
        });
    }

    /// One retry, and only for the answers that mean "ask again".
    #[test]
    fn only_a_rate_limit_or_a_server_error_is_retried() {
        assert!(worth_retrying(&ProviderError::RateLimited {
            retry_after: None
        }));
        assert!(worth_retrying(&ProviderError::Http("HTTP 503".into())));
        assert!(worth_retrying(&ProviderError::Http(
            "Claude API error 500".into()
        )));
        assert!(worth_retrying(&ProviderError::Http("HTTP 408".into())));
        assert!(worth_retrying(&ProviderError::Http("HTTP 429".into())));

        // A rate limit that named a longer wait is not retried in one second.
        assert!(!worth_retrying(&ProviderError::RateLimited {
            retry_after: Some(Duration::from_secs(60))
        }));
        assert!(!worth_retrying(&ProviderError::Http("HTTP 404".into())));
        assert!(!worth_retrying(&ProviderError::Http(
            "connection refused to 127.0.0.1:8080".into()
        )));
        assert!(!worth_retrying(&ProviderError::Auth("HTTP 401".into())));
        assert!(!worth_retrying(&ProviderError::NotConfigured));
        assert!(!worth_retrying(&ProviderError::Parse("empty body".into())));
    }

    // --------------------------------------------------------------- row 21, grace

    /// A transient blip does not flash a healthy tile red, but only once, and never when
    /// the fix is the user's to make.
    #[test]
    fn the_first_transient_failure_is_suppressed_and_auth_is_not() {
        let good = ok(40.0).unwrap();
        let blip = ProviderError::Http("HTTP 502".into());

        assert!(graceable(&blip, Some(&good)));
        let merged = merge("codex", Some(&good), Err(blip), true);
        assert_eq!(merged.primary.and_then(|w| w.used_percent), Some(40.0));
        assert!(merged.error.is_none(), "the first blip is invisible");
        assert!(merged.error_kind.is_none());

        // Nothing to keep showing: an error with no data under it has to be shown.
        assert!(!graceable(
            &ProviderError::Http("HTTP 502".into()),
            Some(&UsageSnapshot::new("codex"))
        ));
        assert!(!graceable(&ProviderError::Http("HTTP 502".into()), None));

        // Auth and NotConfigured need the user to act, so they surface immediately.
        for loud in [
            ProviderError::Auth("session expired".into()),
            ProviderError::NotConfigured,
        ] {
            assert!(!graceable(&loud, Some(&good)), "{loud}");
        }
    }

    // ------------------------------------------------------------- row 24, cadence

    /// The table, boundary by boundary. 2 / 5 / 15 / 30 minutes on time since the popover
    /// was opened, and 30 under battery saver whatever the interaction says.
    #[test]
    fn the_adaptive_table_matches_on_time_since_the_popover_opened() {
        let now = Utc::now();
        let ago = |mins: i64| Some(now - chrono::Duration::minutes(mins));

        assert_eq!(adaptive_secs(now, ago(0), false), 120);
        assert_eq!(adaptive_secs(now, ago(5), false), 120, "5 min is inclusive");
        assert_eq!(adaptive_secs(now, ago(6), false), 300);
        assert_eq!(adaptive_secs(now, ago(60), false), 300, "1 h is inclusive");
        assert_eq!(adaptive_secs(now, ago(61), false), 900);
        assert_eq!(adaptive_secs(now, ago(240), false), 900, "4 h is inclusive");
        assert_eq!(adaptive_secs(now, ago(241), false), 1800);
        assert_eq!(adaptive_secs(now, None, false), 1800, "never opened");

        // A clock step forward must not read as idleness.
        assert_eq!(adaptive_secs(now, ago(-30), false), 120);

        // Battery saver wins over everything, including a popover open one second ago.
        assert_eq!(adaptive_secs(now, ago(0), true), 1800);
        assert_eq!(adaptive_secs(now, None, true), 1800);

        // Contractual bounds: nothing below 2 minutes, nothing above 30.
        for (_, delay) in ADAPTIVE_TABLE {
            assert!((120..=1800).contains(&delay));
        }
    }

    /// A fixed interval is still a fixed interval, and a config that predates the adaptive
    /// choice keeps exactly the cadence it had.
    #[test]
    fn a_fixed_interval_ignores_the_table() {
        let mut config = crate::config::Config::default();
        let now = Utc::now();
        // A default Config is a fresh install, and a fresh install is adaptive. The value
        // it produces is not asserted here because `battery_saver()` is a live probe;
        // `the_adaptive_table_matches_the_measured_policy` covers the table itself.
        assert!(is_adaptive(&config), "a fresh install is adaptive");

        // An existing config.json has no `refresh_adaptive` key, which reads as false.
        config.refresh_adaptive = false;
        assert_eq!(cadence_secs(&config, now, Some(now)), 300);
        config.refresh_minutes = 15;
        assert_eq!(cadence_secs(&config, now, None), 900);
        // `normalize` clamps to 1, but a hand built value of 0 must not divide the wait
        // down to nothing.
        config.refresh_minutes = 0;
        assert_eq!(cadence_secs(&config, now, None), 60);
    }

    /// The battery saver probe answers without panicking on any machine, and a desktop
    /// with no battery reads as "not saving power" rather than as the slowest cadence.
    #[test]
    fn the_battery_saver_probe_is_answerable() {
        assert_eq!(
            battery_saver(),
            battery_saver(),
            "the probe is a read, not a toggle"
        );
    }

    // --------------------------------------------------------------- advisor, alerts

    /// The advisor fires on the way down through the threshold and exactly once per dip:
    /// staying below is silent, recovering above the threshold clears the record and
    /// re-arms, and the next crossing fires again. Exactly-at-the-threshold counts as
    /// below, because the setting says "drops below or to".
    #[test]
    fn the_advisor_fires_once_per_dip_and_rearms_on_recovery() {
        let mut alerted = Vec::new();
        let threshold = Some(20);
        let above = ok(70.0).unwrap(); // 30% remaining
        let below = ok(85.0).unwrap(); // 15% remaining
        let deeper = ok(95.0).unwrap(); // 5% remaining

        assert!(advisor_should_fire(
            threshold,
            false,
            Some(&above),
            &below,
            "codex",
            &mut alerted
        ));
        assert_eq!(alerted, vec!["codex".to_string()]);

        assert!(
            !advisor_should_fire(
                threshold,
                false,
                Some(&below),
                &deeper,
                "codex",
                &mut alerted
            ),
            "staying below does not re-fire"
        );

        assert!(
            !advisor_should_fire(
                threshold,
                false,
                Some(&deeper),
                &above,
                "codex",
                &mut alerted
            ),
            "the recovery itself is silent"
        );
        assert!(alerted.is_empty(), "recovery clears the record");

        assert!(
            advisor_should_fire(
                threshold,
                false,
                Some(&above),
                &below,
                "codex",
                &mut alerted
            ),
            "the next dip fires again"
        );

        // Landing exactly on the threshold is at-or-below: 30% remaining crossing into 20%.
        let mut exact = Vec::new();
        let at = ok(80.0).unwrap(); // 20% remaining
        assert!(advisor_should_fire(
            threshold,
            false,
            Some(&above),
            &at,
            "codex",
            &mut exact
        ));
    }

    /// Everything that must stay silent: quiet hours consume a crossing (and the consumed
    /// dip does not fire once they end), the feature off fires nothing and clears stale
    /// records, a first observation has nothing it crossed, an exhausted lead belongs to
    /// the exhaustion toast, and a lead without a percent is not judged at all.
    #[test]
    fn the_advisor_stays_silent_off_quiet_exhausted_and_without_a_crossing() {
        let (above, below) = (ok(70.0).unwrap(), ok(85.0).unwrap());

        // Quiet hours consume the crossing rather than delivering it stale at the end of
        // the window, and the record it leaves stops the re-fire afterwards.
        let mut quieted = Vec::new();
        assert!(!advisor_should_fire(
            Some(20),
            true,
            Some(&above),
            &below,
            "codex",
            &mut quieted
        ));
        assert_eq!(quieted.len(), 1, "consumed, not lost: the dip is recorded");
        assert!(!advisor_should_fire(
            Some(20),
            false,
            Some(&above),
            &below,
            "codex",
            &mut quieted
        ));

        // Off is off, and turning it off clears a record left underneath.
        let mut stale = vec!["codex".to_string()];
        assert!(!advisor_should_fire(
            None,
            false,
            Some(&above),
            &below,
            "codex",
            &mut stale
        ));
        assert!(stale.is_empty(), "off re-arms everything");

        // A first observation already below never crossed anything, but it is recorded,
        // so it cannot fire retroactively either.
        let mut fresh = Vec::new();
        assert!(!advisor_should_fire(
            Some(20),
            false,
            None,
            &below,
            "codex",
            &mut fresh
        ));
        assert_eq!(fresh.len(), 1);

        // Exhausted is the exhaustion toast's transition, even though 0% is below any
        // threshold, and the exempt transition records nothing.
        let mut whole = Vec::new();
        let exhausted = ok(100.0).unwrap();
        assert!(!advisor_should_fire(
            Some(20),
            false,
            Some(&above),
            &exhausted,
            "codex",
            &mut whole
        ));
        assert!(whole.is_empty());

        // No known percent, no judgement; an unknown percent re-arms like a recovery.
        let unknown = UsageSnapshot::new("codex");
        let mut rearm = vec!["codex".to_string()];
        assert!(!advisor_should_fire(
            Some(20),
            false,
            Some(&above),
            &unknown,
            "codex",
            &mut rearm
        ));
        assert!(rearm.is_empty());

        // Providers are tracked separately: one provider's dip does not silence another.
        let mut multi = Vec::new();
        assert!(advisor_should_fire(
            Some(20),
            false,
            Some(&above),
            &below,
            "codex",
            &mut multi
        ));
        assert!(advisor_should_fire(
            Some(20),
            false,
            Some(&above),
            &below,
            "claude",
            &mut multi
        ));
        assert_eq!(multi.len(), 2);
    }

    /// The shared countdown phrasing: the same rungs the exhaustion toast always had.
    /// Each value sits in the MIDDLE of its displayed bucket, because `resets_in` reads
    /// the clock a few milliseconds after the test builds its instant, and a value on a
    /// bucket edge would truncate into the bucket below.
    #[test]
    fn the_reset_countdown_uses_the_largest_unit_that_fits() {
        let ahead = |secs: i64| Some(Utc::now() + chrono::Duration::seconds(secs));
        assert_eq!(
            resets_in(ahead(91_800)).as_deref(),
            Some("Resets in 1d 1h.")
        );
        assert_eq!(resets_in(ahead(7_410)).as_deref(), Some("Resets in 2h 3m."));
        assert_eq!(resets_in(ahead(330)).as_deref(), Some("Resets in 5m."));
        assert_eq!(resets_in(ahead(0)), None, "a reset now is no countdown");
        assert_eq!(resets_in(ahead(-60)), None, "a past reset is no countdown");
        assert_eq!(resets_in(None), None);
    }
}
