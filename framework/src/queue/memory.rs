//! In-memory queue driver.
//!
//! Canonical test surface. Backed by:
//! - a `VecDeque<Envelope>` for the visible queue,
//! - a `HashMap<ReservationToken, Envelope>` for reservations,
//! - a `tokio_util::time::DelayQueue<ReservationToken>` for visibility-timeout expiry,
//! - a `Vec<(DateTime<Utc>, Envelope)>` for delayed jobs.
//!
//! # Design note — paused-clock compatibility
//!
//! `pop` itself drains expired visibility reservations synchronously
//! (via a noop-waker context) before checking the visible queue. This
//! means that even in `#[tokio::test(start_paused = true)]` tests —
//! where the background reaper's `sleep(50ms)` never fires — reclaim
//! still happens on the next `pop` call after the caller has advanced
//! the clock past the reservation deadline.
//!
//! The reaper is retained for production use where `pop` is infrequent
//! and background reclaim is needed.

use crate::error::FrameworkError;
use crate::queue::driver::{QueueDriver, Reservation, ReservationToken};
use crate::queue::envelope::Envelope;
use async_trait::async_trait;
use chrono::Utc;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::time::DelayQueue;
use uuid::Uuid;

#[derive(Default)]
struct Inner {
    visible: VecDeque<Envelope>,
    reserved: HashMap<ReservationToken, Envelope>,
    delayed: Vec<(chrono::DateTime<chrono::Utc>, Envelope)>,
}

pub struct MemoryQueueDriver {
    inner: Arc<Mutex<Inner>>,
    /// Async mutex guards the DelayQueue so both `pop` and the reaper
    /// can poll it synchronously after acquiring the lock.
    visibility: Arc<AsyncMutex<DelayQueue<ReservationToken>>>,
    reaper: tokio::task::JoinHandle<()>,
}

impl Drop for MemoryQueueDriver {
    fn drop(&mut self) {
        self.reaper.abort();
    }
}

/// Drain all currently-expired entries from `dq` into the `inner` visible
/// queue. The noop waker context must be created and dropped within this
/// call — callers must ensure it is not held across an await.
fn drain_expired(
    inner: &Mutex<Inner>,
    dq: &mut DelayQueue<ReservationToken>,
) {
    let waker = futures::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    let mut expired_tokens = Vec::new();
    while let Poll::Ready(Some(item)) = dq.poll_expired(&mut cx) {
        expired_tokens.push(item.into_inner());
    }
    // cx / waker are dropped here — no await has occurred.
    if !expired_tokens.is_empty() {
        let mut g = inner.lock().expect("memory queue poisoned");
        for token in expired_tokens {
            if let Some(env) = g.reserved.remove(&token) {
                g.visible.push_front(env);
            }
        }
    }
}

impl MemoryQueueDriver {
    pub fn new() -> Self {
        let inner = Arc::new(Mutex::new(Inner::default()));
        let visibility = Arc::new(AsyncMutex::new(DelayQueue::new()));

        let inner2 = inner.clone();
        let visibility2 = visibility.clone();

        let reaper = tokio::spawn(async move {
            loop {
                // Promote "available now" delayed jobs.
                {
                    let mut g = inner2.lock().expect("memory queue poisoned");
                    let now = Utc::now();
                    let mut i = 0;
                    while i < g.delayed.len() {
                        if g.delayed[i].0 <= now {
                            let (_, env) = g.delayed.swap_remove(i);
                            g.visible.push_back(env);
                        } else {
                            i += 1;
                        }
                    }
                }

                // Reclaim expired visibility reservations. Acquire the async
                // mutex, then poll synchronously (no await while lock held).
                {
                    let mut dq = visibility2.lock().await;
                    drain_expired(&inner2, &mut dq);
                    // Lock released here — before the sleep await.
                }

                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        Self {
            inner,
            visibility,
            reaper,
        }
    }
}

impl Default for MemoryQueueDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl QueueDriver for MemoryQueueDriver {
    async fn push(&self, env: Envelope) -> Result<(), FrameworkError> {
        let now = Utc::now();
        let mut g = self.inner.lock().expect("memory queue poisoned");
        if env.available_at <= now {
            g.visible.push_back(env);
        } else {
            g.delayed.push((env.available_at, env));
        }
        Ok(())
    }

    async fn pop(&self, visibility_timeout: Duration) -> Result<Option<Reservation>, FrameworkError> {
        // Acquire visibility lock, drain expired reservations + promote delayed jobs.
        {
            let mut dq = self.visibility.lock().await;
            drain_expired(&self.inner, &mut dq);
            // dq lock released here.
        }

        // Promote any "available now" delayed entries.
        {
            let mut g = self.inner.lock().expect("memory queue poisoned");
            let now = Utc::now();
            let mut i = 0;
            while i < g.delayed.len() {
                if g.delayed[i].0 <= now {
                    let (_, env) = g.delayed.swap_remove(i);
                    g.visible.push_back(env);
                } else {
                    i += 1;
                }
            }
        }

        let env_opt = {
            let mut g = self.inner.lock().expect("memory queue poisoned");
            g.visible.pop_front()
        };

        if let Some(env) = env_opt {
            let token = ReservationToken(Uuid::new_v4());
            {
                let mut g = self.inner.lock().expect("memory queue poisoned");
                g.reserved.insert(token.clone(), env.clone());
            }
            self.visibility
                .lock()
                .await
                .insert(token.clone(), visibility_timeout);
            Ok(Some(Reservation { envelope: env, token }))
        } else {
            Ok(None)
        }
    }

    async fn ack(&self, token: &ReservationToken) -> Result<(), FrameworkError> {
        let mut g = self.inner.lock().expect("memory queue poisoned");
        g.reserved.remove(token);
        Ok(())
    }

    async fn nack(
        &self,
        token: &ReservationToken,
        requeue_delay: Duration,
    ) -> Result<(), FrameworkError> {
        let env = {
            let mut g = self.inner.lock().expect("memory queue poisoned");
            g.reserved.remove(token)
        };
        if let Some(mut env) = env {
            if requeue_delay.is_zero() {
                let mut g = self.inner.lock().expect("memory queue poisoned");
                g.visible.push_front(env);
            } else {
                env.available_at = Utc::now()
                    + chrono::Duration::from_std(requeue_delay).map_err(|e| {
                        FrameworkError::internal(format!("requeue delay overflow: {e}"))
                    })?;
                let mut g = self.inner.lock().expect("memory queue poisoned");
                g.delayed.push((env.available_at, env));
            }
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "memory"
    }
}
