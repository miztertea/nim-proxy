//! Key pool: one "lane" per NIM API key, each with an exact sliding-window
//! rate limiter (N requests per rolling 60s). NIM enforces ~40 RPM per key,
//! so a sliding window matches its semantics better than a token bucket
//! (which would allow a double-sized burst inside a single minute).

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);

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
    /// Slot reserved; send the request with this key.
    Ready { lane: usize, key: String },
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

    /// Try to reserve a request slot on the lane that is available soonest.
    /// Reserving records the send timestamp immediately, so concurrent
    /// callers can't oversubscribe a lane.
    pub fn reserve(&self) -> Reservation {
        let now = Instant::now();
        let mut best_wait = Duration::MAX;

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
                sent.push_back(now);
                return Reservation::Ready {
                    lane: i,
                    key: lane.key.clone(),
                };
            }
            best_wait = best_wait.min(ready_at - now);
        }
        Reservation::Wait(best_wait.min(WINDOW))
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

    #[test]
    fn spreads_load_across_lanes_then_waits() {
        let pool = Pool::new(keys(2), 1);
        assert!(matches!(pool.reserve(), Reservation::Ready { lane: 0, .. }));
        assert!(matches!(pool.reserve(), Reservation::Ready { lane: 1, .. }));
        // Both lanes at their 1-per-minute cap: caller must wait ~60s.
        match pool.reserve() {
            Reservation::Wait(w) => assert!(w > Duration::from_secs(55) && w <= WINDOW),
            _ => panic!("expected Wait"),
        }
    }

    #[test]
    fn penalized_lane_is_skipped() {
        let pool = Pool::new(keys(2), 10);
        pool.penalize(0, Duration::from_secs(30));
        match pool.reserve() {
            Reservation::Ready { lane, key } => {
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
        match pool.reserve() {
            Reservation::Wait(w) => assert!(w <= Duration::from_secs(5)),
            _ => panic!("expected Wait"),
        }
    }
}
