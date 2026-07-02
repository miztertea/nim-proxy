//! Global FIFO slot dispatcher. Every client connection — one OpenCode, five
//! OpenCodes, an n8n flow, a Codex session — funnels through one queue, so
//! under contention slots are granted strictly in arrival order instead of
//! letting freshly-arrived requests win wakeup races against long waiters.

use std::sync::Arc;
use std::time::{Duration, Instant};

use metrics::gauge;
use tokio::sync::{mpsc, oneshot};

use crate::pool::{Pool, Reservation};

pub struct Dispatcher {
    queue: mpsc::UnboundedSender<Waiter>,
}

struct Waiter {
    reply: oneshot::Sender<(usize, String)>,
    deadline: Instant,
    prefer: Option<usize>,
}

impl Dispatcher {
    pub fn new(pool: Arc<Pool>) -> Self {
        let (queue, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(pool, rx));
        Self { queue }
    }

    /// Join the queue. The receiver resolves to a reserved (lane, key) slot,
    /// or errors if no slot can open before `deadline`. Dropping the receiver
    /// leaves the queue; a slot granted to an abandoned waiter is returned to
    /// the pool.
    pub fn acquire(
        &self,
        deadline: Instant,
        prefer: Option<usize>,
    ) -> oneshot::Receiver<(usize, String)> {
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

async fn run(pool: Arc<Pool>, mut queue: mpsc::UnboundedReceiver<Waiter>) {
    while let Some(waiter) = queue.recv().await {
        let _leave = scopeguard(|| gauge!("nimproxy_queue_depth").decrement(1.0));
        loop {
            if waiter.reply.is_closed() {
                break; // client hung up while queued
            }
            match pool.reserve(waiter.prefer) {
                Reservation::Ready { lane, key, stamp } => {
                    if waiter.reply.send((lane, key)).is_err() {
                        pool.release(lane, stamp);
                    }
                    break;
                }
                Reservation::Wait(wait) => {
                    if Instant::now() + wait > waiter.deadline {
                        break; // fail fast: dropped reply -> caller sees error
                    }
                    // Short sleep cap so we notice abandoned waiters promptly.
                    tokio::time::sleep(wait.min(Duration::from_millis(500))).await;
                }
            }
        }
    }
}

/// Minimal drop-guard so the queue-depth gauge stays honest on every exit
/// path (granted, expired, or abandoned).
fn scopeguard<F: FnMut()>(f: F) -> impl Drop {
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

    fn pool(lanes: usize, rpm: usize) -> Arc<Pool> {
        Arc::new(Pool::new(
            (0..lanes).map(|i| format!("key{i}")).collect(),
            rpm,
        ))
    }

    #[tokio::test]
    async fn grants_slots_in_order_while_capacity_remains() {
        let d = Dispatcher::new(pool(2, 1));
        let deadline = Instant::now() + Duration::from_secs(5);
        let a = d.acquire(deadline, None).await.expect("first slot");
        let b = d.acquire(deadline, None).await.expect("second slot");
        assert_eq!(a.0, 0);
        assert_eq!(b.0, 1);
    }

    #[tokio::test]
    async fn fails_fast_when_no_slot_can_open_before_deadline() {
        let d = Dispatcher::new(pool(1, 1));
        let deadline = Instant::now() + Duration::from_millis(200);
        d.acquire(deadline, None).await.expect("first slot");
        // Lane is at capacity for ~60s, far past the deadline.
        assert!(d.acquire(deadline, None).await.is_err());
    }
}
