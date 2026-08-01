//! Usage history for the popover sparklines.
//!
//! One ring buffer per series, 288 samples (24 hours at the default 5 minute refresh),
//! persisted to `%APPDATA%\AgentBar\history.json` with the same atomic temp-plus-rename
//! discipline as config.rs.
//!
//! A series is a provider AND an account (row 35, see [`series_key`]), not a provider.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::providers::UsageSnapshot;

/// 24 hours at the default 5 minute refresh. At ~36 bytes per sample and 23 providers the
/// file stays under 250 kB in the worst case, every provider configured and every series
/// full. A real install has a handful of providers and a fraction of that.
pub const MAX_SAMPLES: usize = 288;

/// Series kept per provider: the account in use plus one previous, so switching back and
/// forth between two accounts keeps both charts. History is a nicety, and an unbounded set
/// of account keys (a provider whose key turns out not to be stable) must not be able to
/// grow `history.json` without a ceiling.
pub const MAX_SERIES_PER_PROVIDER: usize = 2;

/// Row 35. The map key a snapshot's samples belong under: the provider id alone when the
/// provider does not know its account, `provider:account` when it does.
///
/// Two accounts under one key is how a `codex login` into a second account appends its 4
/// percent onto the first account's 91 percent and draws a cliff that never happened.
/// Splitting the key means the new account simply starts an empty series.
///
/// A snapshot with no `account_key` keeps the bare provider id, which is also what every
/// `history.json` written before this existed uses, so old files keep their series.
///
/// The account half is HASHED, not written through. Claude and Gemini have no stable
/// identity other than the profile email, so `account_key` holds one, and this key is
/// written to `history.json` on disk and travels to the frontend as `history_key`. All
/// either side needs is "same account or not", which a digest answers, so an email never
/// reaches disk in the clear. Truncated to 16 hex characters: the whole point is telling
/// one of a person's two accounts from the other, and 64 bits is not close to that limit.
pub fn series_key(provider_id: &str, account_key: Option<&str>) -> String {
    match account_key.map(str::trim).filter(|a| !a.is_empty()) {
        Some(account) => {
            use sha2::Digest;
            let digest = sha2::Sha256::digest(account.as_bytes());
            let mut hex = String::with_capacity(16);
            for byte in &digest[..8] {
                hex.push_str(&format!("{byte:02x}"));
            }
            format!("{provider_id}:{hex}")
        }
        None => provider_id.to_string(),
    }
}

/// The provider id half of a [`series_key`]. Provider ids never contain a colon, so the
/// first one is the boundary however the account half is spelled.
fn provider_of(key: &str) -> &str {
    key.split_once(':').map_or(key, |(id, _)| id)
}

/// Short field names on purpose: this file is written every refresh.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Unix seconds.
    pub t: i64,
    /// Used percent of the lane the tray speaks for, 0..=100.
    pub u: f64,
    /// Label of the lane `u` was measured against.
    ///
    /// Row 17: a weekly series becoming a 5h series is a different quota, so the
    /// sparkline breaks the line where this changes AND the value jumps, rather than
    /// drawing a cliff that never happened. The label names whichever lane is currently
    /// the most constrained, so it also flips when two lanes merely cross at nearly the
    /// same percentage, and that is not a break worth drawing: `Sparkline.svelte` requires
    /// both. `None` on samples written before this field existed, and
    /// `#[serde(default)]` is what lets an existing `history.json` load unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub l: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct History {
    series: HashMap<String, Vec<Sample>>,
}

impl History {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("AgentBar")
            .join("history.json")
    }

    /// Never fails and never panics: a missing or corrupt file starts an empty history.
    /// Losing a sparkline is not worth refusing to start over.
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(Self::path()) else {
            return Self::default();
        };
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Self {
        match serde_json::from_str::<Self>(text) {
            Ok(mut h) => {
                h.prune();
                h
            }
            Err(e) => {
                log::warn!("history parse failed, starting empty: {e}");
                Self::default()
            }
        }
    }

    pub fn series(&self) -> &HashMap<String, Vec<Sample>> {
        &self.series
    }

    /// Append one sample for a successful fetch. Returns false when nothing was recorded,
    /// so the caller can skip the write.
    ///
    /// Errored fetches record nothing: a placeholder zero would draw a cliff in the
    /// sparkline that never happened.
    pub fn record(&mut self, snapshot: &UsageSnapshot, refresh_secs: i64) -> bool {
        if snapshot.error.is_some() {
            return false;
        }
        let Some((used, lane)) = lead_used_percent(snapshot) else {
            return false;
        };
        let mut sample = Sample {
            t: Utc::now().timestamp(),
            u: used,
            l: Some(lane),
        };
        let key = series_key(&snapshot.provider_id, snapshot.account_key.as_deref());
        let series = self.series.entry(key).or_default();
        // A backwards clock step (NTP correction, resume from sleep) must not push the
        // series out of order: the sparkline maps x from the first and last timestamps and
        // would draw outside its own viewBox. The sample keeps the newest known time.
        if let Some(last) = series.last() {
            sample.t = sample.t.max(last.t);
        }
        // Dedup: an unchanged value inside one refresh interval moves the newest sample
        // forward instead of adding a second flat point. The range starts at 0 on purpose:
        // a backwards clock step (NTP, resume from sleep) makes the delta negative, and
        // rewriting the newest sample with an older timestamp would leave the series
        // non-monotonic, which the sparkline cannot draw.
        match series.last_mut() {
            Some(last)
                if (last.u - sample.u).abs() < f64::EPSILON
                    // A lane change at the same percentage is still a new series, so it
                    // must not be collapsed into the sample it is meant to break from.
                    && last.l == sample.l
                    && (0..refresh_secs).contains(&sample.t.saturating_sub(last.t)) =>
            {
                *last = sample;
            }
            _ => series.push(sample),
        }
        if series.len() > MAX_SAMPLES {
            series.drain(..series.len() - MAX_SAMPLES);
        }
        // Cheap enough to run every sample (the map holds a few dozen keys) and it has to
        // run here, not only in `prune`, because a process that never restarts would
        // otherwise accumulate a series per account key seen.
        self.cap_accounts(&snapshot.provider_id);
        true
    }

    /// Keep only the [`MAX_SERIES_PER_PROVIDER`] most recently written series for one
    /// provider, dropping the accounts whose last sample is oldest.
    fn cap_accounts(&mut self, provider_id: &str) {
        let mut keys: Vec<(i64, String)> = self
            .series
            .iter()
            .filter(|(key, _)| provider_of(key) == provider_id)
            .map(|(key, samples)| (samples.last().map_or(0, |s| s.t), key.clone()))
            .collect();
        if keys.len() <= MAX_SERIES_PER_PROVIDER {
            return;
        }
        keys.sort_unstable();
        for (_, key) in &keys[..keys.len() - MAX_SERIES_PER_PROVIDER] {
            self.series.remove(key);
        }
    }

    /// Drop providers that are no longer in the registry and trim over-long series.
    pub fn prune(&mut self) {
        let known: Vec<&'static str> = crate::providers::all_providers()
            .iter()
            .map(|p| p.id())
            .collect();
        self.series.retain(|key, series| {
            if !known.contains(&provider_of(key)) {
                return false;
            }
            series.retain(|s| s.t > 0 && s.u.is_finite());
            // A file written before the clock guard below, or hand edited, can be out of
            // order. The sparkline assumes time order, so restore it here once.
            series.sort_by_key(|s| s.t);
            for s in series.iter_mut() {
                s.u = s.u.clamp(0.0, 100.0);
            }
            if series.len() > MAX_SAMPLES {
                series.drain(..series.len() - MAX_SAMPLES);
            }
            !series.is_empty()
        });
        // A hand edited or long lived file can hold more accounts per provider than we
        // keep. Same ceiling on load as on write.
        for id in known {
            self.cap_accounts(id);
        }
    }

    /// Atomic write: temp file next to the target, then rename over it.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(())
    }
}

/// The percentage and the lane it belongs to, from the one window the tray speaks for.
///
/// It has to be the same call the tray makes, not `primary.or(secondary).or(tertiary)`:
/// the tray follows the MOST CONSTRAINED lane, so the old fallthrough could plot a
/// different window from the one the glyph reports. Taking both values from a single
/// `lead_window` is also what keeps `Sample::l` describing `Sample::u`.
fn lead_used_percent(snapshot: &UsageSnapshot) -> Option<(f64, String)> {
    let lead = crate::state::lead_window(snapshot)?;
    Some((lead.used_percent?, lead.label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::UsageWindow;

    fn snapshot(id: &str, used: f64) -> UsageSnapshot {
        let mut s = UsageSnapshot::new(id);
        s.primary = Some(UsageWindow::new("5h", Some(used), None, None));
        s
    }

    #[test]
    fn errors_and_empty_snapshots_record_nothing() {
        let mut h = History::default();
        let mut errored = snapshot("codex", 10.0);
        errored.error = Some("boom".into());
        assert!(!h.record(&errored, 300));
        assert!(!h.record(&UsageSnapshot::new("codex"), 300));
        assert!(h.series().is_empty());
    }

    /// Row 17. The series must follow the same lane the tray glyph reports, and carry its
    /// label, or the sparkline and the tray describe different quotas.
    #[test]
    fn the_sample_follows_the_lead_lane_and_names_it() {
        // The old `primary.or(secondary)` fallthrough would have plotted the 5h at 3%
        // while the tray glyph showed the weekly at 60%.
        let mut s = UsageSnapshot::new("codex");
        s.primary = Some(UsageWindow::new("5h", Some(3.0), None, Some(300)));
        s.secondary = Some(UsageWindow::new("Weekly", Some(60.0), None, Some(10080)));

        let mut h = History::default();
        assert!(h.record(&s, 300));
        let sample = &h.series()["codex"][0];
        assert_eq!(sample.u, 60.0, "the most constrained lane is the lead lane");
        assert_eq!(sample.l.as_deref(), Some("Weekly"));

        // A lane change at the same percentage is a different quota, so it appends rather
        // than collapsing into the sample the sparkline has to break from.
        s.secondary = None;
        s.primary = Some(UsageWindow::new("5h", Some(60.0), None, Some(300)));
        assert!(h.record(&s, 300));
        let series = &h.series()["codex"];
        assert_eq!(
            series.len(),
            2,
            "a lane change was collapsed into the last sample"
        );
        assert_eq!(series[1].l.as_deref(), Some("5h"));
    }

    /// An existing history.json predates the lane label and must still load.
    #[test]
    fn a_sample_without_a_lane_label_still_parses() {
        let h = History::parse(r#"{"codex":[{"t":1,"u":5.0}]}"#);
        assert_eq!(h.series()["codex"][0].l, None);
    }

    // -------------------------------------------------- row 35, account identity

    fn for_account(id: &str, account: &str, used: f64) -> UsageSnapshot {
        let mut s = snapshot(id, used);
        s.account_key = Some(account.to_string());
        s
    }

    #[test]
    fn a_snapshot_with_no_account_keeps_the_bare_provider_id() {
        assert_eq!(series_key("codex", None), "codex");
        assert_eq!(series_key("codex", Some("  ")), "codex");
        assert_eq!(
            series_key("codex", Some(" acct-a ")),
            series_key("codex", Some("acct-a")),
            "the key is trimmed before it is hashed"
        );

        let mut h = History::default();
        assert!(h.record(&snapshot("codex", 12.0), 300));
        assert!(h.series().contains_key("codex"));
    }

    /// Row 35, the headline: a second `codex login` must not append its 4 percent onto the
    /// first account's 91 percent. The old series stays exactly as it was and the new
    /// account starts its own.
    #[test]
    fn a_new_account_starts_a_new_series_instead_of_drawing_a_cliff() {
        let mut h = History::default();
        assert!(h.record(&for_account("codex", "acct-a", 91.0), 300));
        assert!(h.record(&for_account("codex", "acct-b", 4.0), 300));

        let a = series_key("codex", Some("acct-a"));
        let b = series_key("codex", Some("acct-b"));
        assert_eq!(h.series()[&a].len(), 1);
        assert_eq!(h.series()[&a][0].u, 91.0);
        assert_eq!(h.series()[&b].len(), 1);
        assert_eq!(h.series()[&b][0].u, 4.0);

        // Switching back continues the first account rather than restarting it.
        assert!(h.record(&for_account("codex", "acct-a", 93.0), 300));
        assert_eq!(h.series()[&a].len(), 2);
    }

    /// Row 35. Claude and Gemini have no stable identity other than the profile email, so
    /// `account_key` holds one. It must not reach `history.json` or the frontend in the
    /// clear, and the hash must still tell two accounts apart.
    #[test]
    fn an_account_key_is_hashed_and_never_written_through() {
        let email = "sam@example.com";
        let key = series_key("claude", Some(email));
        assert!(
            !key.contains(email),
            "an email must not reach disk in clear"
        );
        assert!(!key.contains('@'));
        assert_eq!(key.len(), "claude:".len() + 16);
        assert!(key["claude:".len()..]
            .chars()
            .all(|c| c.is_ascii_hexdigit()));
        assert_ne!(key, series_key("claude", Some("other@example.com")));
        assert_eq!(
            key,
            series_key("claude", Some(email)),
            "stable across calls"
        );
    }

    /// Two providers with the same account key are still two series.
    #[test]
    fn the_provider_half_of_the_key_still_separates_providers() {
        let mut h = History::default();
        assert!(h.record(&for_account("codex", "same", 10.0), 300));
        assert!(h.record(&for_account("claude", "same", 20.0), 300));
        assert_eq!(h.series().len(), 2);
        assert_eq!(h.series()[&series_key("claude", Some("same"))][0].u, 20.0);
    }

    /// An unstable account key must not grow the file without a ceiling: the provider
    /// keeps the account in use plus one previous.
    #[test]
    fn accounts_per_provider_are_capped_oldest_first() {
        let mut h = History::default();
        for account in ["a", "b", "c"] {
            assert!(h.record(&for_account("codex", account, 5.0), 300));
            // Distinct last-sample times, so "oldest" is unambiguous.
            h.series
                .get_mut(&series_key("codex", Some(account)))
                .unwrap()[0]
                .t -= match account {
                "a" => 200,
                "b" => 100,
                _ => 0,
            };
        }
        assert_eq!(h.series().len(), MAX_SERIES_PER_PROVIDER);
        assert!(
            !h.series().contains_key(&series_key("codex", Some("a"))),
            "the oldest account stayed"
        );
        assert!(h.series().contains_key(&series_key("codex", Some("c"))));
    }

    /// A composite key survives a load, and an unknown provider is still dropped by its
    /// provider half.
    #[test]
    fn pruning_reads_the_provider_out_of_a_composite_key() {
        let h = History::parse(
            r#"{"codex:acct-a":[{"t":100,"u":5.0}],"gone:acct-a":[{"t":100,"u":5.0}]}"#,
        );
        assert_eq!(h.series()["codex:acct-a"].len(), 1);
        assert!(!h.series().contains_key("gone:acct-a"));
    }

    #[test]
    fn unchanged_values_inside_the_interval_replace_instead_of_appending() {
        let mut h = History::default();
        assert!(h.record(&snapshot("codex", 42.0), 300));
        assert!(h.record(&snapshot("codex", 42.0), 300));
        assert_eq!(h.series()["codex"].len(), 1);

        // A changed value always appends.
        assert!(h.record(&snapshot("codex", 43.0), 300));
        assert_eq!(h.series()["codex"].len(), 2);

        // An unchanged value older than the interval also appends.
        h.series.get_mut("codex").unwrap().last_mut().unwrap().t -= 400;
        assert!(h.record(&snapshot("codex", 43.0), 300));
        assert_eq!(h.series()["codex"].len(), 3);
    }

    #[test]
    fn a_backwards_clock_step_never_makes_the_series_non_monotonic() {
        let mut h = History::default();
        assert!(h.record(&snapshot("codex", 42.0), 300));
        assert!(h.record(&snapshot("codex", 43.0), 300));
        // The clock steps an hour back, so the stored samples are now in the future.
        for s in h.series.get_mut("codex").unwrap() {
            s.t += 3600;
        }
        // Same value: dedup, and it must not rewrite the newest sample to an older time.
        assert!(h.record(&snapshot("codex", 43.0), 300));
        // Changed value: appends, and it must not append before the sample above.
        assert!(h.record(&snapshot("codex", 44.0), 300));

        let series = &h.series()["codex"];
        assert_eq!(series.len(), 3);
        assert!(
            series.windows(2).all(|w| w[1].t >= w[0].t),
            "timestamps went backwards: {series:?}"
        );
    }

    #[test]
    fn the_ring_buffer_is_capped() {
        let mut h = History::default();
        for i in 0..(MAX_SAMPLES + 50) {
            h.record(&snapshot("codex", i as f64 % 100.0), 0);
        }
        let series = &h.series()["codex"];
        assert_eq!(series.len(), MAX_SAMPLES);
        // The oldest samples are the ones dropped.
        assert_eq!(series.last().unwrap().u, (MAX_SAMPLES + 49) as f64 % 100.0);
    }

    #[test]
    fn parsing_never_panics_and_prunes() {
        assert!(History::parse("{ not json").series().is_empty());
        assert!(History::parse("[]").series().is_empty());

        // Unknown providers are dropped, known ones kept and clamped.
        let h = History::parse(
            r#"{"codex":[{"t":100,"u":250.0},{"t":0,"u":5.0}],"gone":[{"t":100,"u":5.0}]}"#,
        );
        assert!(!h.series().contains_key("gone"));
        assert_eq!(h.series()["codex"].len(), 1);
        assert_eq!(h.series()["codex"][0].u, 100.0);

        // Over-long series from a hand-edited file are trimmed on load.
        let long: Vec<String> = (0..400)
            .map(|i| format!(r#"{{"t":{},"u":1.0}}"#, i + 1))
            .collect();
        let h = History::parse(&format!(r#"{{"codex":[{}]}}"#, long.join(",")));
        assert_eq!(h.series()["codex"].len(), MAX_SAMPLES);
        assert_eq!(h.series()["codex"][0].t, 400 - MAX_SAMPLES as i64 + 1);
    }

    #[test]
    fn a_full_file_stays_small() {
        let mut h = History::default();
        for p in crate::providers::all_providers() {
            let mut s = snapshot(p.id(), 1.0);
            for i in 0..MAX_SAMPLES {
                s.primary.as_mut().unwrap().used_percent = Some((i % 100) as f64 + 0.25);
                h.record(&s, 0);
            }
        }
        // Row 17 added the lane label to every sample, which cost about 6 bytes each in
        // this worst case (23 providers, every series full). Still a rounding error next
        // to the app's own footprint, and the file is only rewritten once a refresh.
        let bytes = serde_json::to_vec(&h).unwrap().len();
        assert!(bytes < 250_000, "history.json would be {bytes} bytes");
    }
}
