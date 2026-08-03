//! Provider reliability over time: when each provider went down and came back.
//!
//! A point is written only when a provider's status CHANGES (healthy <-> errored), so a
//! provider that is down for ten consecutive refreshes records a single "went down"
//! point, not ten. Counting the `ok == false` points in a window therefore counts
//! OUTAGES, which is exactly the "Cursor was down 3 times this week" question.
//!
//! Persisted to `%APPDATA%\AgentsBar\health.json` with the same atomic temp-plus-rename
//! discipline as config and history.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Retain a little over a month so a monthly view has its full window plus margin.
const RETAIN_SECS: i64 = 35 * 86_400;
/// Safety cap on points per provider. Points are only written on a status change, so a
/// stable provider barely grows; this just bounds a pathologically flaky one.
const MAX_POINTS: usize = 512;

/// Short field names on purpose: this file is written whenever a provider changes state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthPoint {
    /// Unix seconds.
    pub t: i64,
    /// Whether the provider was healthy: the fetch succeeded and the stored snapshot
    /// carries no error. A GRACED transient blip keeps the previous good snapshot, so it
    /// records as healthy — consistent with the tile, which also hid it.
    pub ok: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HealthLog {
    series: HashMap<String, Vec<HealthPoint>>,
}

impl HealthLog {
    pub fn path() -> PathBuf {
        crate::config::dir().join("health.json")
    }

    /// Never fails and never panics: a missing or corrupt file starts an empty log.
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(Self::path()) else {
            return Self::default();
        };
        Self::parse(&text)
    }

    /// Parse and prune a serialized log. A corrupt string yields an empty log rather than
    /// a panic.
    pub fn parse(text: &str) -> Self {
        match serde_json::from_str::<Self>(text) {
            Ok(mut h) => {
                h.prune();
                h
            }
            Err(e) => {
                log::warn!("health parse failed, starting empty: {e}");
                Self::default()
            }
        }
    }

    pub fn series(&self) -> &HashMap<String, Vec<HealthPoint>> {
        &self.series
    }

    /// Record a health outcome, appending a point only when the status differs from the
    /// last recorded one. Returns true when a point was written (and the file should be
    /// saved).
    pub fn record(&mut self, provider_id: &str, ok: bool) -> bool {
        let series = self.series.entry(provider_id.to_string()).or_default();
        // Unchanged status: nothing new to record.
        if series.last().is_some_and(|last| last.ok == ok) {
            return false;
        }
        series.push(HealthPoint {
            t: Utc::now().timestamp(),
            ok,
        });
        if series.len() > MAX_POINTS {
            series.drain(..series.len() - MAX_POINTS);
        }
        true
    }

    /// Count outages (transitions into error) at or after `since`.
    pub fn outages_since(&self, provider_id: &str, since: i64) -> usize {
        self.series
            .get(provider_id)
            .map(|series| series.iter().filter(|p| !p.ok && p.t >= since).count())
            .unwrap_or(0)
    }

    /// Drop points older than the retention window and any provider left with nothing.
    pub fn prune(&mut self) {
        let cutoff = Utc::now().timestamp() - RETAIN_SECS;
        self.series.retain(|_, series| {
            series.retain(|p| p.t >= cutoff);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn log_with(points: Vec<HealthPoint>) -> HealthLog {
        let mut series = HashMap::new();
        series.insert("cursor".to_string(), points);
        HealthLog { series }
    }

    #[test]
    fn unchanged_status_is_not_recorded_again() {
        let mut h = HealthLog::default();
        assert!(h.record("cursor", true), "first point is always written");
        assert!(
            !h.record("cursor", true),
            "same status again writes nothing"
        );
        assert!(h.record("cursor", false), "a change writes");
        assert!(!h.record("cursor", false));
        assert_eq!(h.series()["cursor"].len(), 2);
    }

    #[test]
    fn a_consecutive_outage_is_one_point_not_many() {
        let mut h = HealthLog::default();
        h.record("cursor", true);
        // Down for three refreshes: still a single "went down" point.
        h.record("cursor", false);
        h.record("cursor", false);
        h.record("cursor", false);
        let series = &h.series()["cursor"];
        assert_eq!(series.iter().filter(|p| !p.ok).count(), 1);
    }

    #[test]
    fn outages_are_counted_within_the_window() {
        let now = Utc::now().timestamp();
        let day = 86_400;
        let h = log_with(vec![
            HealthPoint {
                t: now - 10 * day,
                ok: false,
            }, // outside a 7 day window
            HealthPoint {
                t: now - 5 * day,
                ok: false,
            }, // inside
            HealthPoint {
                t: now - 2 * day,
                ok: false,
            }, // inside
        ]);
        assert_eq!(h.outages_since("cursor", now - 7 * day), 2);
        assert_eq!(h.outages_since("cursor", now - 30 * day), 3);
        assert_eq!(h.outages_since("missing", now - 7 * day), 0);
    }

    #[test]
    fn healthy_points_are_not_outages() {
        let now = Utc::now().timestamp();
        let h = log_with(vec![
            HealthPoint {
                t: now - 100,
                ok: true,
            },
            HealthPoint {
                t: now - 50,
                ok: false,
            },
        ]);
        assert_eq!(h.outages_since("cursor", now - 1000), 1);
    }

    #[test]
    fn prune_drops_old_points_and_empty_providers() {
        let now = Utc::now().timestamp();
        let mut h = HealthLog::default();
        h.series.insert(
            "old".to_string(),
            vec![HealthPoint {
                t: now - 40 * 86_400,
                ok: false,
            }],
        );
        h.series.insert(
            "recent".to_string(),
            vec![HealthPoint {
                t: now - 86_400,
                ok: false,
            }],
        );
        h.prune();
        assert!(
            !h.series().contains_key("old"),
            "older than retention is gone"
        );
        assert!(h.series().contains_key("recent"));
    }

    #[test]
    fn parsing_never_panics() {
        assert!(HealthLog::parse("{ not json").series().is_empty());
    }
}
