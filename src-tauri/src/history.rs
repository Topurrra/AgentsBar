//! Usage history for the popover sparklines.
//!
//! One ring buffer per provider id, 288 samples (24 hours at the default 5 minute
//! refresh), persisted to `%APPDATA%\AgentBar\history.json` with the same atomic
//! temp-plus-rename discipline as config.rs.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::providers::UsageSnapshot;

/// 24 hours at the default 5 minute refresh. At ~36 bytes per sample and 23 providers the
/// file stays under 250 kB in the worst case, every provider configured and every series
/// full. A real install has a handful of providers and a fraction of that.
pub const MAX_SAMPLES: usize = 288;

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
        let series = self.series.entry(snapshot.provider_id.clone()).or_default();
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
        true
    }

    /// Drop providers that are no longer in the registry and trim over-long series.
    pub fn prune(&mut self) {
        let known: Vec<&'static str> = crate::providers::all_providers()
            .iter()
            .map(|p| p.id())
            .collect();
        self.series.retain(|id, series| {
            if !known.contains(&id.as_str()) {
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
