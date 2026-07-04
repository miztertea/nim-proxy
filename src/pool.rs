//! Key pool: one "lane" per NIM API key, each with an exact sliding-window
//! rate limiter (N requests per rolling 60s). NIM enforces ~40 RPM per key,
//! so a sliding window matches its semantics better than a token bucket
//! (which would allow a double-sized burst inside a single minute).
//!
//! The pool is immutable once built; settings changes build a replacement via
//! [`Pool::rebuild`] and swap it into the shared [`PoolHandle`]. The dispatcher
//! is the only `reserve` caller and holds the handle's read lock across each
//! reserve, so a rebuild (under the write lock) can never interleave with a
//! grant — a kept key's in-window timestamps carry over exactly once.
//!
//! Disabled keys stay in the pool as inactive *state carriers* (never
//! granted, invisible to `len`/capacity/stats — they sit past the `active`
//! boundary). Without them, a disable→enable cycle spans two rebuilds and
//! the second would resurrect the key with a fresh window while the
//! upstream's window still remembers it — load-tested to cause real
//! upstream 429s before this existed.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// Shared, swappable pool: readers snapshot an `Arc<Pool>`; the settings
/// layer swaps in a rebuilt pool under the write lock.
pub type PoolHandle = Arc<RwLock<Arc<Pool>>>;

/// NIM's rolling window is 60s; the extra second is a delivery-jitter safety
/// margin. We reserve slots at grant time but the upstream clocks arrivals,
/// so a boundary-timed request whose predecessor was delayed more than it can
/// land inside the upstream's window even though it left ours. Load-tested at
/// 100 concurrent clients: with 60s exactly, ~2% of requests tripped a strict
/// upstream window; with the pad, zero. Costs ~1.6% peak throughput.
const WINDOW: Duration = Duration::from_secs(61);

/// A lane blueprint. Disabled specs become state carriers: held for their
/// rate state, never granted.
pub struct LaneSpec {
    pub key: String,
    pub rpm: usize,
    pub enabled: bool,
}

struct Lane {
    key: String,
    /// This key's requests-per-minute budget (keys can differ: paid tiers,
    /// self-hosted NIM).
    rpm: usize,
    /// Timestamps of requests sent within the last WINDOW.
    sent: Mutex<VecDeque<Instant>>,
    /// Lane is benched until this instant (set after an upstream 429/5xx).
    cooldown_until: Mutex<Instant>,
}

pub struct Pool {
    /// Enabled lanes first (indexes 0..active — the only ones ever granted,
    /// counted, or reported), disabled state carriers after.
    lanes: Vec<Lane>,
    active: usize,
}

/// One lane's live state (see [`Pool::lane_stats`]).
pub struct LaneStat {
    pub key: String,
    pub rpm: usize,
    pub in_window: usize,
    pub cooldown_ms: u64,
}

pub enum Reservation {
    /// Slot reserved; send the request with this key. `stamp` identifies the
    /// reservation so an unused slot can be returned via [`Pool::release`].
    Ready {
        lane: usize,
        key: String,
        stamp: Instant,
        /// True when the caller's preferred lane won (conversation affinity hit).
        sticky: bool,
    },
    /// All lanes busy; soonest a slot frees up.
    Wait(Duration),
}

impl Pool {
    pub fn new(specs: Vec<LaneSpec>) -> Self {
        Self::assemble(specs, None)
    }

    /// Build a replacement pool from `specs`, carrying over the in-window
    /// timestamps and cooldown of every key kept from `self` (matched by key
    /// string, enabled or carrier). A kept key can never be double-spent
    /// across a swap; a lowered rpm is honored immediately (`try_take`
    /// checks the live count); a disabled key re-enables warm because its
    /// carrier lane kept the window.
    pub fn rebuild(&self, specs: Vec<LaneSpec>) -> Self {
        Self::assemble(specs, Some(self))
    }

    fn assemble(mut specs: Vec<LaneSpec>, old: Option<&Pool>) -> Self {
        // Enabled lanes first (stable — preserves relative order), carriers
        // after, so index-based semantics only ever see enabled lanes.
        specs.sort_by_key(|s| !s.enabled);
        let active = specs.iter().filter(|s| s.enabled).count();
        let now = Instant::now();
        let lanes = specs
            .into_iter()
            .map(
                |s| match old.and_then(|o| o.lanes.iter().find(|l| l.key == s.key)) {
                    Some(prev) => Lane {
                        sent: Mutex::new(prev.sent.lock().unwrap().clone()),
                        cooldown_until: Mutex::new(*prev.cooldown_until.lock().unwrap()),
                        key: s.key,
                        rpm: s.rpm,
                    },
                    None => Lane {
                        key: s.key,
                        rpm: s.rpm,
                        sent: Mutex::new(VecDeque::new()),
                        cooldown_until: Mutex::new(now),
                    },
                },
            )
            .collect();
        Self { lanes, active }
    }

    /// Enabled lanes only — carriers are invisible everywhere.
    pub fn len(&self) -> usize {
        self.active
    }

    /// Aggregate requests-per-minute across enabled lanes.
    pub fn capacity_rpm(&self) -> usize {
        self.lanes[..self.active].iter().map(|l| l.rpm).sum()
    }

    /// Per-lane rpm budgets, in lane order (feeds the dashboard config).
    pub fn rpms(&self) -> Vec<usize> {
        self.lanes[..self.active].iter().map(|l| l.rpm).collect()
    }

    /// Point-in-time per-lane view for the Settings key rows.
    pub fn lane_stats(&self) -> Vec<LaneStat> {
        let now = Instant::now();
        self.lanes[..self.active]
            .iter()
            .map(|l| {
                let in_window = {
                    let sent = l.sent.lock().unwrap();
                    sent.iter().filter(|t| now - **t < WINDOW).count()
                };
                let cooldown_ms = l
                    .cooldown_until
                    .lock()
                    .unwrap()
                    .saturating_duration_since(now)
                    .as_millis() as u64;
                LaneStat {
                    key: l.key.clone(),
                    rpm: l.rpm,
                    in_window,
                    cooldown_ms,
                }
            })
            .collect()
    }

    /// Take a slot on lane `i` if it has capacity right now. Reserving
    /// records the send timestamp immediately, so concurrent callers can't
    /// oversubscribe a lane.
    fn try_take(&self, i: usize, now: Instant, sticky: bool) -> Option<Reservation> {
        let lane = &self.lanes[i];
        if *lane.cooldown_until.lock().unwrap() > now {
            return None;
        }
        let mut sent = lane.sent.lock().unwrap();
        while sent.front().is_some_and(|t| now - *t >= WINDOW) {
            sent.pop_front();
        }
        if sent.len() < lane.rpm {
            sent.push_back(now);
            Some(Reservation::Ready {
                lane: i,
                key: lane.key.clone(),
                stamp: now,
                sticky,
            })
        } else {
            None
        }
    }

    /// Try to reserve a request slot. `prefer` pins a conversation to one
    /// lane while it has capacity (keeping any upstream prefix cache warm on
    /// a single key); otherwise the least-loaded ready lane wins, spreading
    /// concurrent in-flight requests evenly across keys. An out-of-range
    /// `prefer` (computed against a pool that has since shrunk) is ignored.
    pub fn reserve(&self, prefer: Option<usize>) -> Reservation {
        let now = Instant::now();
        if let Some(p) = prefer.filter(|&p| p < self.active) {
            if let Some(r) = self.try_take(p, now, true) {
                return r;
            }
        }

        let mut ready: Vec<(usize, usize)> = Vec::new(); // (in-window load, lane)
        let mut best_wait = WINDOW;
        for (i, lane) in self.lanes[..self.active].iter().enumerate() {
            let cooldown = *lane.cooldown_until.lock().unwrap();
            let mut sent = lane.sent.lock().unwrap();
            while sent.front().is_some_and(|t| now - *t >= WINDOW) {
                sent.pop_front();
            }
            let window_ready = if sent.len() < lane.rpm {
                now
            } else if lane.rpm == 0 {
                // validate() forbids rpm 0, but a panic here would kill the
                // dispatcher task and hang every request — never index in.
                now + WINDOW
            } else {
                sent[sent.len() - lane.rpm] + WINDOW
            };
            let ready_at = window_ready.max(cooldown);
            if ready_at <= now {
                ready.push((sent.len(), i));
            } else {
                best_wait = best_wait.min(ready_at - now);
            }
        }
        ready.sort_unstable();
        for (_, i) in ready {
            if let Some(r) = self.try_take(i, now, false) {
                return r;
            }
        }
        Reservation::Wait(best_wait)
    }

    /// Return a reserved slot that was never spent on an upstream request
    /// (e.g. the client hung up while queued).
    pub fn release(&self, lane: usize, stamp: Instant) {
        let mut sent = self.lanes[lane].sent.lock().unwrap();
        if let Some(pos) = sent.iter().rposition(|t| *t == stamp) {
            sent.remove(pos);
        }
    }

    /// Bench a lane after the upstream told us to back off.
    pub fn penalize(&self, lane: usize, backoff: Duration) {
        let until = Instant::now() + backoff;
        let mut cd = self.lanes[lane].cooldown_until.lock().unwrap();
        if *cd < until {
            *cd = until;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(key: &str, rpm: usize, enabled: bool) -> LaneSpec {
        LaneSpec {
            key: key.into(),
            rpm,
            enabled,
        }
    }

    fn keys(n: usize, rpm: usize) -> Vec<LaneSpec> {
        (0..n)
            .map(|i| spec(&format!("key{i}"), rpm, true))
            .collect()
    }

    fn take(pool: &Pool, prefer: Option<usize>) -> usize {
        match pool.reserve(prefer) {
            Reservation::Ready { lane, .. } => lane,
            Reservation::Wait(_) => panic!("expected Ready"),
        }
    }

    #[test]
    fn spreads_load_across_lanes_then_waits() {
        let pool = Pool::new(keys(2, 1));
        assert_eq!(take(&pool, None), 0);
        assert_eq!(take(&pool, None), 1);
        // Both lanes at their 1-per-minute cap: caller must wait ~60s.
        match pool.reserve(None) {
            Reservation::Wait(w) => assert!(w > Duration::from_secs(55) && w <= WINDOW),
            _ => panic!("expected Wait"),
        }
    }

    #[test]
    fn burst_lands_on_least_loaded_lane() {
        let pool = Pool::new(keys(3, 10));
        let mut per_lane = [0usize; 3];
        for _ in 0..9 {
            per_lane[take(&pool, None)] += 1;
        }
        assert_eq!(per_lane, [3, 3, 3]);
    }

    #[test]
    fn per_lane_rpm_budgets_are_honored() {
        // Lane 0 allows 1/min, lane 1 allows 3/min: four grants total.
        let pool = Pool::new(vec![spec("small", 1, true), spec("big", 3, true)]);
        let mut per_lane = [0usize; 2];
        for _ in 0..4 {
            per_lane[take(&pool, None)] += 1;
        }
        assert_eq!(per_lane, [1, 3]);
        assert!(matches!(pool.reserve(None), Reservation::Wait(_)));
        assert_eq!(pool.capacity_rpm(), 4);
        assert_eq!(pool.rpms(), vec![1, 3]);
    }

    #[test]
    fn sticky_lane_wins_until_full_then_spills_over() {
        let pool = Pool::new(keys(2, 2));
        assert_eq!(take(&pool, Some(1)), 1);
        assert_eq!(take(&pool, Some(1)), 1);
        // Preferred lane is at capacity: spill to the other lane.
        assert_eq!(take(&pool, Some(1)), 0);
    }

    #[test]
    fn sticky_flag_reports_affinity_outcome() {
        let pool = Pool::new(keys(2, 1));
        match pool.reserve(Some(0)) {
            Reservation::Ready {
                lane: 0,
                sticky: true,
                ..
            } => {}
            _ => panic!("expected sticky hit on lane 0"),
        }
        match pool.reserve(Some(0)) {
            Reservation::Ready {
                lane: 1,
                sticky: false,
                ..
            } => {}
            _ => panic!("expected spill to lane 1"),
        }
    }

    #[test]
    fn out_of_range_prefer_is_ignored() {
        // An affinity index computed against a bigger, since-replaced pool
        // must not panic — it just falls through to least-loaded.
        let pool = Pool::new(keys(1, 2));
        assert_eq!(take(&pool, Some(7)), 0);
    }

    #[test]
    fn released_slot_becomes_available_again() {
        let pool = Pool::new(keys(1, 1));
        let Reservation::Ready { lane, stamp, .. } = pool.reserve(None) else {
            panic!("expected Ready");
        };
        assert!(matches!(pool.reserve(None), Reservation::Wait(_)));
        pool.release(lane, stamp);
        assert!(matches!(pool.reserve(None), Reservation::Ready { .. }));
    }

    #[test]
    fn penalized_lane_is_skipped() {
        let pool = Pool::new(keys(2, 10));
        pool.penalize(0, Duration::from_secs(30));
        match pool.reserve(None) {
            Reservation::Ready { lane, key, .. } => {
                assert_eq!(lane, 1);
                assert_eq!(key, "key1");
            }
            _ => panic!("expected Ready on lane 1"),
        }
    }

    #[test]
    fn all_lanes_penalized_reports_soonest_recovery() {
        let pool = Pool::new(keys(2, 10));
        pool.penalize(0, Duration::from_secs(30));
        pool.penalize(1, Duration::from_secs(5));
        match pool.reserve(None) {
            Reservation::Wait(w) => assert!(w <= Duration::from_secs(5)),
            _ => panic!("expected Wait"),
        }
    }

    #[test]
    fn rebuild_carries_window_state_for_kept_keys() {
        // A slot spent before the swap counts against the key after it: the
        // same key can never be double-spent across a rebuild.
        let pool = Pool::new(keys(1, 1));
        take(&pool, None);
        let rebuilt = pool.rebuild(keys(1, 1));
        assert!(matches!(rebuilt.reserve(None), Reservation::Wait(_)));
    }

    #[test]
    fn rebuild_carries_cooldown_for_kept_keys() {
        let pool = Pool::new(keys(2, 10));
        pool.penalize(0, Duration::from_secs(30));
        let rebuilt = pool.rebuild(keys(2, 10));
        assert_eq!(take(&rebuilt, Some(0)), 1, "benched lane stays benched");
    }

    #[test]
    fn rebuild_new_key_starts_fresh_and_removed_key_is_gone() {
        let pool = Pool::new(vec![spec("old", 1, true)]);
        take(&pool, None);
        let rebuilt = pool.rebuild(vec![spec("new", 1, true)]);
        assert_eq!(rebuilt.len(), 1);
        match rebuilt.reserve(None) {
            Reservation::Ready { key, .. } => assert_eq!(key, "new"),
            _ => panic!("fresh key should be ready"),
        }
    }

    #[test]
    fn disabled_lanes_are_carriers_never_granted_never_counted() {
        let pool = Pool::new(vec![spec("on", 2, true), spec("off", 40, false)]);
        assert_eq!(pool.len(), 1);
        assert_eq!(pool.capacity_rpm(), 2);
        assert_eq!(pool.rpms(), vec![2]);
        assert_eq!(pool.lane_stats().len(), 1);
        for _ in 0..2 {
            match pool.reserve(None) {
                Reservation::Ready { key, .. } => assert_eq!(key, "on"),
                _ => panic!("enabled lane should grant"),
            }
        }
        // Capacity spent: the carrier must not pick up the slack.
        assert!(matches!(pool.reserve(None), Reservation::Wait(_)));
    }

    #[test]
    fn disable_enable_cycle_cannot_double_spend_the_window() {
        // The exact sequence that produced real upstream 429s in the load
        // test: spend the key, disable it (rebuild 1), re-enable it
        // (rebuild 2). The carrier lane keeps the window across both.
        let pool = Pool::new(vec![spec("k", 1, true)]);
        take(&pool, None);
        let disabled = pool.rebuild(vec![spec("k", 1, false)]);
        assert!(matches!(disabled.reserve(None), Reservation::Wait(_)));
        let re_enabled = disabled.rebuild(vec![spec("k", 1, true)]);
        assert!(
            matches!(re_enabled.reserve(None), Reservation::Wait(_)),
            "the pre-disable send must still count against the window"
        );
    }

    #[test]
    fn rebuild_honors_lowered_and_raised_rpm() {
        // Two spent on rpm=2; lowering to 1 means no capacity until the
        // window drains — the live count check does this for free.
        let pool = Pool::new(keys(1, 2));
        take(&pool, None);
        take(&pool, None);
        let lowered = pool.rebuild(keys(1, 1));
        assert!(matches!(lowered.reserve(None), Reservation::Wait(_)));
        // Raising grants the extra headroom immediately.
        let raised = pool.rebuild(keys(1, 3));
        assert!(matches!(raised.reserve(None), Reservation::Ready { .. }));
    }
}
