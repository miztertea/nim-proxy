//! Key pool: one "lane" per NIM API key, each with an exact sliding-window
//! rate limiter (N requests per rolling 60s). NIM enforces ~40 RPM per key,
//! so a sliding window matches its semantics better than a token bucket
//! (which would allow a double-sized burst inside a single minute).

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// NIM's rolling window is 60s; the extra second is a delivery-jitter safety
/// margin. We reserve slots at grant time but the upstream clocks arrivals,
/// so a boundary-timed request whose predecessor was delayed more than it can
/// land inside the upstream's window even though it left ours. Load-tested at
/// 100 concurrent clients: with 60s exactly, ~2% of requests tripped a strict
/// upstream window; with the pad, zero. Costs ~1.6% peak throughput.
const WINDOW: Duration = Duration::from_secs(61);

struct Lane {
    key: String,
    /// Timestamps of requests sent within the last WINDOW.
    sent: Mutex<VecDeque<Instant>>,
    /// Lane is benched until this instant (set after an upstream 429/5xx).
    cooldown_until: Mutex<Instant>,
}

pub struct Pool {
    lanes: Vec<Lane>,
    rpm: usize,
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
    pub fn new(keys: Vec<String>, rpm: usize) -> Self {
        let now = Instant::now();
        let lanes = keys
            .into_iter()
            .map(|key| Lane {
                key,
                sent: Mutex::new(VecDeque::new()),
                cooldown_until: Mutex::new(now),
            })
            .collect();
        Self { lanes, rpm }
    }

    pub fn len(&self) -> usize {
        self.lanes.len()
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
        if sent.len() < self.rpm {
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
    /// concurrent in-flight requests evenly across keys.
    pub fn reserve(&self, prefer: Option<usize>) -> Reservation {
        let now = Instant::now();
        if let Some(p) = prefer {
            if let Some(r) = self.try_take(p, now, true) {
                return r;
            }
        }

        let mut ready: Vec<(usize, usize)> = Vec::new(); // (in-window load, lane)
        let mut best_wait = WINDOW;
        for (i, lane) in self.lanes.iter().enumerate() {
            let cooldown = *lane.cooldown_until.lock().unwrap();
            let mut sent = lane.sent.lock().unwrap();
            while sent.front().is_some_and(|t| now - *t >= WINDOW) {
                sent.pop_front();
            }
            let window_ready = if sent.len() < self.rpm {
                now
            } else {
                sent[sent.len() - self.rpm] + WINDOW
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

    fn keys(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("key{i}")).collect()
    }

    fn take(pool: &Pool, prefer: Option<usize>) -> usize {
        match pool.reserve(prefer) {
            Reservation::Ready { lane, .. } => lane,
            Reservation::Wait(_) => panic!("expected Ready"),
        }
    }

    #[test]
    fn spreads_load_across_lanes_then_waits() {
        let pool = Pool::new(keys(2), 1);
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
        let pool = Pool::new(keys(3), 10);
        let mut per_lane = [0usize; 3];
        for _ in 0..9 {
            per_lane[take(&pool, None)] += 1;
        }
        assert_eq!(per_lane, [3, 3, 3]);
    }

    #[test]
    fn sticky_lane_wins_until_full_then_spills_over() {
        let pool = Pool::new(keys(2), 2);
        assert_eq!(take(&pool, Some(1)), 1);
        assert_eq!(take(&pool, Some(1)), 1);
        // Preferred lane is at capacity: spill to the other lane.
        assert_eq!(take(&pool, Some(1)), 0);
    }

    #[test]
    fn sticky_flag_reports_affinity_outcome() {
        let pool = Pool::new(keys(2), 1);
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
    fn released_slot_becomes_available_again() {
        let pool = Pool::new(keys(1), 1);
        let Reservation::Ready { lane, stamp, .. } = pool.reserve(None) else {
            panic!("expected Ready");
        };
        assert!(matches!(pool.reserve(None), Reservation::Wait(_)));
        pool.release(lane, stamp);
        assert!(matches!(pool.reserve(None), Reservation::Ready { .. }));
    }

    #[test]
    fn penalized_lane_is_skipped() {
        let pool = Pool::new(keys(2), 10);
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
        let pool = Pool::new(keys(2), 10);
        pool.penalize(0, Duration::from_secs(30));
        pool.penalize(1, Duration::from_secs(5));
        match pool.reserve(None) {
            Reservation::Wait(w) => assert!(w <= Duration::from_secs(5)),
            _ => panic!("expected Wait"),
        }
    }
}
