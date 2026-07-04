//! Global FIFO slot dispatcher. Every client connection — one OpenCode, five
//! OpenCodes, an n8n flow, a Codex session — funnels through one queue, so
//! under contention slots are granted strictly in arrival order instead of
//! letting freshly-arrived requests win wakeup races against long waiters.
//!
//! The dispatcher is the only `Pool::reserve` caller in the app, and it holds
//! the [`PoolHandle`] read lock across each reserve — so a settings-driven
//! pool swap (which takes the write lock) can never interleave with a grant.
//! Grants carry the `Arc<Pool>` that made them (see [`Slot`]) so bench and
//! release always land on the granting pool; a late op on a retired pool is
//! benign because nothing consults it anymore.

use std::sync::Arc;
use std::time::{Duration, Instant};

use metrics::{counter, gauge};
use tokio::sync::{mpsc, oneshot};

use crate::pool::{Pool, PoolHandle, Reservation};

/// Minimum gap between consecutive slot grants. Caps burst *concurrency*
/// (a cold pool can grant its full aggregate RPM instantly — hundreds of
/// simultaneous connects look like a stampede to the upstream and skew
/// arrival timing) without capping throughput: 25ms = 2,400 grants/min,
/// far beyond any realistic key pool's aggregate RPM.
const GRANT_GAP: Duration = Duration::from_millis(25);

/// A granted reservation: the key to send with, and the pool that granted it
/// so follow-up bench/release ops route to the right generation.
pub struct Slot {
    pub pool: Arc<Pool>,
    pub lane: usize,
    pub key: String,
}

pub struct Dispatcher {
    queue: mpsc::UnboundedSender<Waiter>,
}

struct Waiter {
    reply: oneshot::Sender<Slot>,
    deadline: Instant,
    prefer: Option<usize>,
}

impl Dispatcher {
    pub fn new(pool: PoolHandle) -> Self {
        let (queue, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(pool, rx));
        Self { queue }
    }

    /// Join the queue. The receiver resolves to a reserved [`Slot`], or
    /// errors if no slot can open before `deadline`. Dropping the receiver
    /// leaves the queue; a slot granted to an abandoned waiter is returned to
    /// the pool.
    pub fn acquire(&self, deadline: Instant, prefer: Option<usize>) -> oneshot::Receiver<Slot> {
        let (reply, rx) = oneshot::channel();
        gauge!("nimproxy_queue_depth").increment(1.0);
        let _ = self.queue.send(Waiter {
            reply,
            deadline,
            prefer,
        });
        rx
    }
}

async fn run(handle: PoolHandle, mut queue: mpsc::UnboundedReceiver<Waiter>) {
    while let Some(waiter) = queue.recv().await {
        let _leave = scopeguard(|| gauge!("nimproxy_queue_depth").decrement(1.0));
        loop {
            if waiter.reply.is_closed() {
                break; // client hung up while queued
            }
            // Snapshot + reserve under the read guard (reserve is sync and
            // lock-cheap), then drop the guard before any await.
            let (pool, reservation) = {
                let guard = handle.read().unwrap();
                let pool = guard.clone();
                let r = pool.reserve(waiter.prefer);
                (pool, r)
            };
            match reservation {
                Reservation::Ready {
                    lane,
                    key,
                    stamp,
                    sticky,
                } => {
                    let affinity = match waiter.prefer {
                        None => "none",
                        Some(_) if sticky => "sticky",
                        Some(_) => "spill",
                    };
                    counter!("nimproxy_affinity_total", "result" => affinity).increment(1);
                    let slot = Slot {
                        pool: pool.clone(),
                        lane,
                        key,
                    };
                    if waiter.reply.send(slot).is_err() {
                        pool.release(lane, stamp);
                    } else {
                        tokio::time::sleep(GRANT_GAP).await;
                    }
                    break;
                }
                Reservation::Wait(wait) => {
                    if Instant::now() + wait > waiter.deadline {
                        break; // fail fast: dropped reply -> caller sees error
                    }
                    // Short sleep cap so we notice abandoned waiters promptly —
                    // and re-consult the handle, which may have been swapped
                    // for a pool with more capacity in the meantime.
                    tokio::time::sleep(wait.min(Duration::from_millis(500))).await;
                }
            }
        }
    }
}

/// Minimal drop-guard so gauges stay honest on every exit path (granted,
/// expired, abandoned, or panicked).
pub fn scopeguard<F: FnMut()>(f: F) -> impl Drop {
    struct Guard<F: FnMut()>(F);
    impl<F: FnMut()> Drop for Guard<F> {
        fn drop(&mut self) {
            (self.0)();
        }
    }
    Guard(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::RwLock;

    fn handle(lanes: usize, rpm: usize) -> PoolHandle {
        Arc::new(RwLock::new(Arc::new(Pool::new(
            (0..lanes).map(|i| (format!("key{i}"), rpm)).collect(),
        ))))
    }

    #[tokio::test]
    async fn grants_slots_in_order_while_capacity_remains() {
        let d = Dispatcher::new(handle(2, 1));
        let deadline = Instant::now() + Duration::from_secs(5);
        let a = d.acquire(deadline, None).await.expect("first slot");
        let b = d.acquire(deadline, None).await.expect("second slot");
        assert_eq!(a.lane, 0);
        assert_eq!(b.lane, 1);
    }

    #[tokio::test]
    async fn fails_fast_when_no_slot_can_open_before_deadline() {
        let d = Dispatcher::new(handle(1, 1));
        let deadline = Instant::now() + Duration::from_millis(200);
        d.acquire(deadline, None).await.expect("first slot");
        // Lane is at capacity for ~60s, far past the deadline.
        assert!(d.acquire(deadline, None).await.is_err());
    }

    #[tokio::test]
    async fn queued_waiter_is_served_after_a_capacity_raising_swap() {
        let h = handle(1, 1);
        let d = Dispatcher::new(h.clone());
        // Deadline beyond the ~61s window so the waiter queues instead of
        // failing fast.
        let deadline = Instant::now() + Duration::from_secs(120);
        let first = d.acquire(deadline, None).await.expect("first slot");
        let started = Instant::now();
        let pending = d.acquire(deadline, None);
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Swap in a rebuilt pool with double the rpm — carried state keeps the
        // spent slot, the new headroom serves the waiter.
        {
            let mut guard = h.write().unwrap();
            let rebuilt = guard.rebuild(vec![("key0".into(), 2)]);
            *guard = Arc::new(rebuilt);
        }
        let slot = pending.await.expect("slot after swap");
        assert_eq!(slot.lane, 0);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "waiter should be served promptly after the swap, waited {:?}",
            started.elapsed()
        );
        // The first grant's pool is the retired generation; releasing on it
        // must be harmless.
        first.pool.release(first.lane, Instant::now());
    }
}
