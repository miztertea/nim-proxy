//! Metrics history: every 5 minutes the sampler appends a full Prometheus
//! snapshot (the same text /metrics serves) to memory and, when a data dir is
//! writable, to history.jsonl. The dashboard's range queries replay these
//! snapshots through the same client-side parser it uses for live polls.
//! Snapshots are ~4 KB, so 30 days is ~35 MB — retention is a days knob
//! (HISTORY_DAYS, 0 = keep forever), not a size-management subsystem.

use std::fs;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub const SAMPLE_SECS: u64 = 300;

pub struct History {
    points: Mutex<Vec<(u64, String)>>,
    file: Option<PathBuf>,
    /// Retention in days (0 = keep forever). Atomic so the settings layer can
    /// retune it live; the sampler reads it on every append.
    days: AtomicU64,
    dropped_since_compact: Mutex<usize>,
}

impl History {
    pub fn load(dir: Option<PathBuf>, days: u64) -> Self {
        let mut points = Vec::new();
        let file = dir.and_then(|d| {
            if let Err(e) = fs::create_dir_all(&d) {
                tracing::warn!("history disabled: cannot create {}: {e}", d.display());
                return None;
            }
            let path = d.join("history.jsonl");
            if let Ok(f) = fs::File::open(&path) {
                for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                        if let (Some(t), Some(m)) = (v["t"].as_u64(), v["m"].as_str()) {
                            points.push((t, m.to_owned()));
                        }
                    }
                }
            }
            // Verify writability up front so we warn once at boot, not on
            // every sample.
            match fs::OpenOptions::new().create(true).append(true).open(&path) {
                Ok(_) => Some(path),
                Err(e) => {
                    tracing::warn!(
                        "history persistence disabled ({}: {e}); keeping in-memory only",
                        path.display()
                    );
                    None
                }
            }
        });
        points.sort_by_key(|p| p.0);
        tracing::info!(
            "history           {} snapshots loaded, retention {}",
            points.len(),
            if days == 0 {
                "infinite".to_owned()
            } else {
                format!("{days} days")
            }
        );
        Self {
            points: Mutex::new(points),
            file,
            days: AtomicU64::new(days),
            dropped_since_compact: Mutex::new(0),
        }
    }

    /// Retune retention live (settings-driven); applies on the next append.
    pub fn set_days(&self, days: u64) {
        self.days.store(days, Ordering::Relaxed);
    }

    pub fn append(&self, t: u64, snapshot: String) {
        let mut points = self.points.lock().unwrap();
        let days = self.days.load(Ordering::Relaxed);
        if days > 0 {
            let cutoff = t.saturating_sub(days * 86400);
            let before = points.len();
            points.retain(|p| p.0 >= cutoff);
            *self.dropped_since_compact.lock().unwrap() += before - points.len();
        }
        let line = serde_json::json!({"t": t, "m": snapshot}).to_string();
        points.push((t, snapshot));

        if let Some(path) = &self.file {
            let mut dropped = self.dropped_since_compact.lock().unwrap();
            // Compact once a day's worth of expired snapshots has built up;
            // otherwise just append.
            let result = if *dropped > 288 {
                *dropped = 0;
                let all = points
                    .iter()
                    .map(|(t, m)| serde_json::json!({"t": t, "m": m}).to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                fs::write(path, all + "\n")
            } else {
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .and_then(|mut f| writeln!(f, "{line}"))
            };
            if let Err(e) = result {
                tracing::warn!("history write failed: {e}");
            }
        }
    }

    /// Snapshots in [from, to], stride-sampled down to at most `max` plus the
    /// range's endpoints.
    pub fn range(&self, from: u64, to: u64, max: usize) -> Vec<(u64, String)> {
        let points = self.points.lock().unwrap();
        let hits: Vec<&(u64, String)> =
            points.iter().filter(|p| p.0 >= from && p.0 <= to).collect();
        let stride = hits.len().div_ceil(max.max(2));
        hits.iter()
            .enumerate()
            .filter(|(i, _)| i % stride == 0 || *i == hits.len() - 1)
            .map(|(_, p)| (*p).clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_prunes_and_range_filters() {
        let h = History {
            points: Mutex::new(Vec::new()),
            file: None,
            days: AtomicU64::new(1),
            dropped_since_compact: Mutex::new(0),
        };
        h.append(1_000, "old".into());
        h.append(200_000, "new".into()); // 1-day cutoff drops t=1000
        assert_eq!(h.range(0, u64::MAX, 100).len(), 1);
        h.append(200_300, "newer".into());
        let r = h.range(200_000, 200_100, 100);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].1, "new");
    }

    #[test]
    fn range_downsamples_to_max() {
        let h = History {
            points: Mutex::new((0..1000u64).map(|i| (i, i.to_string())).collect()),
            file: None,
            days: AtomicU64::new(0),
            dropped_since_compact: Mutex::new(0),
        };
        let r = h.range(0, 999, 100);
        assert!(r.len() <= 101, "got {}", r.len());
        assert_eq!(r.last().unwrap().0, 999, "endpoint kept");
    }
}
