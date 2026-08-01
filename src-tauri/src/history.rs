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

/// 24 hours at the default 5 minute refresh. At ~30 bytes per sample and 23 providers
/// the file stays around 200 kB even fully populated for every provider.
pub const MAX_SAMPLES: usize = 288;

/// Short field names on purpose: this file is written every refresh.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Unix seconds.
    pub t: i64,
    /// Used percent of the primary lane, 0..=100.
    pub u: f64,
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
        let Some(used) = lead_used_percent(snapshot) else {
            return false;
        };
        let mut sample = Sample {
            t: Utc::now().timestamp(),
            u: used,
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

/// Same lane the tray speaks for: primary, falling through to secondary then tertiary.
fn lead_used_percent(snapshot: &UsageSnapshot) -> Option<f64> {
    snapshot
        .primary
        .as_ref()
        .or(snapshot.secondary.as_ref())
        .or(snapshot.tertiary.as_ref())
        .map(|w| w.used_percent.clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::UsageWindow;

    fn snapshot(id: &str, used: f64) -> UsageSnapshot {
        let mut s = UsageSnapshot::new(id);
        s.primary = Some(UsageWindow {
            label: "5h".into(),
            used_percent: used,
            resets_at: None,
            window_minutes: None,
        });
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
        let long: Vec<String> = (0..400).map(|i| format!(r#"{{"t":{},"u":1.0}}"#, i + 1)).collect();
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
                s.primary.as_mut().unwrap().used_percent = (i % 100) as f64 + 0.25;
                h.record(&s, 0);
            }
        }
        let bytes = serde_json::to_vec(&h).unwrap().len();
        assert!(bytes < 200_000, "history.json would be {bytes} bytes");
    }
}
