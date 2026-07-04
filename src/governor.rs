//! Model-pressure governor: a per-model concurrency gate beside the RPM
//! dispatcher.
//!
//! NIM's serving stack has a per-model worker-concurrency cap that is
//! orthogonal to the per-key RPM limit ("ResourceExhausted: Worker local
//! total request limit reached (32/32)"). It is **model-scoped and shared
//! across all keys**, so failing over to another key can't help — the old
//! behavior of benching the lane just burned healthy key capacity. Instead,
//! worker exhaustion backs off the *model*:
//!
//! - Every model starts ungoverned (no cap, zero config).
//! - On a worker-exhaustion error the governor engages at half the observed
//!   in-flight count, and briefly blocks new admissions while workers drain.
//! - It then grows the cap by one per stable minute (additive increase), and
//!   dissolves back to ungoverned after a long clean period — the worker pool
//!   is shared infrastructure, so the real ceiling moves with other tenants'
//!   load and a static cap would be wrong in both directions.
//! - An operator override pins a fixed cap for a model (no adaptation).
//!
//! Admission is poll-based (waiters re-check every [`POLL`]), not FIFO like
//! the RPM queue: worker slots free stochastically as generations end, and
//! the RPM dispatcher downstream still serializes actual sends.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use metrics::{counter, gauge};

/// How often a waiter re-checks for a free permit.
pub const POLL: Duration = Duration::from_millis(250);
/// Post-exhaustion pause on the model: workers free up as generations
/// finish, so short — this is a drain gap, not a lane-style bench.
const EXHAUST_BACKOFF: Duration = Duration::from_secs(2);
/// Additive increase: +1 concurrency per stable minute.
const GROW_INTERVAL: Duration = Duration::from_secs(60);
/// A governed model with no exhaustion for this long returns to ungoverned.
const DISSOLVE_AFTER: Duration = Duration::from_secs(30 * 60);

/// The worker-exhaustion signature in an upstream error body. Must be
/// checked *before* generic retry handling: this failure is model-scoped,
/// never a reason to bench the lane that carried it.
pub fn is_worker_exhausted(body: &str) -> bool {
    body.contains("Worker local total request limit")
}

#[derive(Default)]
struct ModelState {
    /// Current concurrency cap; 0 = ungoverned.
    limit: usize,
    /// Permits currently held (requests in flight upstream).
    inflight: usize,
    /// New admissions blocked until this instant (post-exhaustion drain).
    blocked_until: Option<Instant>,
    last_exhausted: Option<Instant>,
    /// Last limit change, paces additive growth.
    last_adjusted: Option<Instant>,
}

#[derive(Default)]
pub struct Governor {
    models: Mutex<HashMap<String, ModelState>>,
}

/// A held admission: the request is (about to be) in flight on this model.
/// Dropping it releases the slot on every exit path.
pub struct ModelPermit {
    gov: Arc<Governor>,
    model: String,
}

impl Drop for ModelPermit {
    fn drop(&mut self) {
        self.gov.release(&self.model);
    }
}

impl Governor {
    /// Try to admit a request on `model`; returns a permit or None when the
    /// model is at its cap / draining after an exhaustion.
    pub fn admit(
        self: &Arc<Self>,
        model: &str,
        override_limit: Option<usize>,
    ) -> Option<ModelPermit> {
        self.admit_at(model, override_limit, Instant::now())
            .then(|| ModelPermit {
                gov: self.clone(),
                model: model.to_owned(),
            })
    }

    fn admit_at(&self, model: &str, override_limit: Option<usize>, now: Instant) -> bool {
        let mut models = self.models.lock().unwrap();
        let s = models.entry(model.to_owned()).or_default();
        // Adaptive lifecycle (skipped for operator-pinned models): dissolve
        // after a long clean period, else grow one per stable minute. Lazy —
        // evaluated under demand, which is the only time the cap matters.
        if override_limit.is_none() && s.limit > 0 {
            if s.last_exhausted
                .is_some_and(|t| now.duration_since(t) >= DISSOLVE_AFTER)
            {
                s.limit = 0;
                s.last_adjusted = Some(now);
                gauge!("nimproxy_model_limit", "model" => model.to_owned()).set(0.0);
                tracing::info!(model, "model pressure cleared; governor dissolved");
            } else if s
                .last_adjusted
                .is_some_and(|t| now.duration_since(t) >= GROW_INTERVAL)
            {
                s.limit += 1;
                s.last_adjusted = Some(now);
                gauge!("nimproxy_model_limit", "model" => model.to_owned()).set(s.limit as f64);
            }
        }
        if s.blocked_until.is_some_and(|b| b > now) {
            return false;
        }
        let limit = override_limit.unwrap_or(s.limit);
        if limit > 0 && s.inflight >= limit {
            return false;
        }
        s.inflight += 1;
        gauge!("nimproxy_model_inflight", "model" => model.to_owned()).set(s.inflight as f64);
        true
    }

    fn release(&self, model: &str) {
        let mut models = self.models.lock().unwrap();
        if let Some(s) = models.get_mut(model) {
            s.inflight = s.inflight.saturating_sub(1);
            gauge!("nimproxy_model_inflight", "model" => model.to_owned()).set(s.inflight as f64);
        }
    }

    /// Record a worker-exhaustion error on `model`. Call while the failing
    /// request's permit is still held, so the observed in-flight count
    /// includes it. Engages (or tightens) the cap at half that count and
    /// opens a short drain gap; operator-pinned models only get the gap.
    pub fn note_exhausted(&self, model: &str, override_limit: Option<usize>) {
        self.note_exhausted_at(model, override_limit, Instant::now());
    }

    fn note_exhausted_at(&self, model: &str, override_limit: Option<usize>, now: Instant) {
        counter!("nimproxy_worker_exhausted_total", "model" => model.to_owned()).increment(1);
        let mut models = self.models.lock().unwrap();
        let s = models.entry(model.to_owned()).or_default();
        s.blocked_until = Some(now + EXHAUST_BACKOFF);
        s.last_exhausted = Some(now);
        if override_limit.is_none() {
            s.limit = (s.inflight / 2).max(1);
            s.last_adjusted = Some(now);
            gauge!("nimproxy_model_limit", "model" => model.to_owned()).set(s.limit as f64);
            tracing::warn!(
                model,
                inflight = s.inflight,
                limit = s.limit,
                "worker exhaustion upstream; governing model concurrency"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gov() -> Governor {
        Governor::default()
    }

    /// Admit `n` permits at `now`, panicking if any is refused.
    fn admit_n(g: &Governor, model: &str, n: usize, now: Instant) {
        for i in 0..n {
            assert!(g.admit_at(model, None, now), "admission {i} refused");
        }
    }

    #[test]
    fn detects_the_worker_exhaustion_signature() {
        assert!(is_worker_exhausted(
            r#"{"detail":"ResourceExhausted: Worker local total request limit reached (32/32)"}"#
        ));
        assert!(!is_worker_exhausted(r#"{"error":"rate limited"}"#));
        assert!(!is_worker_exhausted(""));
    }

    #[test]
    fn ungoverned_model_admits_without_bound() {
        let g = gov();
        admit_n(&g, "m", 500, Instant::now());
    }

    #[test]
    fn exhaustion_engages_at_half_observed_inflight() {
        let g = gov();
        let now = Instant::now();
        admit_n(&g, "m", 6, now);
        g.note_exhausted_at("m", None, now);
        // Limit is 3; 6 are still in flight, so nothing new is admitted even
        // after the drain gap.
        let later = now + EXHAUST_BACKOFF;
        assert!(!g.admit_at("m", None, later));
        // Draining below the cap re-opens admission.
        for _ in 0..4 {
            g.release("m");
        }
        assert!(g.admit_at("m", None, later));
        assert!(!g.admit_at("m", None, later), "cap of 3 reached again");
    }

    #[test]
    fn single_inflight_exhaustion_engages_at_one() {
        let g = gov();
        let now = Instant::now();
        admit_n(&g, "m", 1, now);
        g.note_exhausted_at("m", None, now);
        g.release("m");
        let later = now + EXHAUST_BACKOFF;
        assert!(g.admit_at("m", None, later));
        assert!(!g.admit_at("m", None, later));
    }

    #[test]
    fn drain_gap_blocks_all_admissions_briefly() {
        let g = gov();
        let now = Instant::now();
        admit_n(&g, "m", 1, now);
        g.note_exhausted_at("m", None, now);
        g.release("m");
        assert!(
            !g.admit_at("m", None, now + Duration::from_millis(500)),
            "inside the drain gap"
        );
        assert!(g.admit_at("m", None, now + EXHAUST_BACKOFF));
    }

    #[test]
    fn grows_one_per_stable_minute() {
        let g = gov();
        let now = Instant::now();
        admit_n(&g, "m", 4, now);
        g.note_exhausted_at("m", None, now); // limit 2
        for _ in 0..4 {
            g.release("m");
        }
        let t1 = now + EXHAUST_BACKOFF;
        admit_n(&g, "m", 2, t1);
        assert!(!g.admit_at("m", None, t1), "at the engaged cap of 2");
        // A stable minute later the cap grows to 3.
        let t2 = now + GROW_INTERVAL;
        assert!(g.admit_at("m", None, t2));
        assert!(!g.admit_at("m", None, t2), "cap 3 reached");
    }

    #[test]
    fn dissolves_after_a_long_clean_period() {
        let g = gov();
        let now = Instant::now();
        admit_n(&g, "m", 2, now);
        g.note_exhausted_at("m", None, now); // limit 1
        g.release("m");
        g.release("m");
        let clean = now + DISSOLVE_AFTER;
        // Ungoverned again: admits far past any cap the AIMD could have grown.
        admit_n(&g, "m", 100, clean);
    }

    #[test]
    fn override_pins_the_cap_and_skips_adaptation() {
        let g = gov();
        let now = Instant::now();
        assert!(g.admit_at("m", Some(2), now));
        assert!(g.admit_at("m", Some(2), now));
        assert!(!g.admit_at("m", Some(2), now), "pinned cap of 2");
        // Exhaustion opens the drain gap but never rewrites the pinned cap.
        g.note_exhausted_at("m", Some(2), now);
        g.release("m");
        assert!(!g.admit_at("m", Some(2), now), "drain gap");
        let later = now + EXHAUST_BACKOFF;
        assert!(g.admit_at("m", Some(2), later), "pinned cap unchanged");
        // No growth either, minutes later.
        let much_later = now + GROW_INTERVAL * 5;
        assert!(!g.admit_at("m", Some(2), much_later));
    }

    #[test]
    fn models_are_independent() {
        let g = gov();
        let now = Instant::now();
        admit_n(&g, "hot", 2, now);
        g.note_exhausted_at("hot", None, now);
        // The untouched model admits freely during hot's drain gap.
        admit_n(&g, "cold", 50, now);
    }
}
