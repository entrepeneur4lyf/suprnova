//! In-memory queue driver.
//!
//! Canonical test surface. Backed by:
//! - a `VecDeque<Envelope>` for the visible queue,
//! - a `HashMap<ReservationToken, Envelope>` for reservations,
//! - a `tokio_util::time::DelayQueue<ReservationToken>` for visibility-timeout expiry,
//! - a `tokio_util::time::DelayQueue<Envelope>` for delayed jobs.
//!
//! # Design note — paused-clock compatibility
//!
//! Both DelayQueues run on Tokio's virtual clock. Under
//! `#[tokio::test(start_paused = true)]`, `tokio::time::advance(N)` correctly
//! fires their expirations, so paused-clock tests for delayed jobs work without
//! any wall-clock comparison.
//!
//! `pop` drains both DelayQueues synchronously (via a noop-waker context) before
//! checking the visible queue. This means that even when the background reaper's
//! `sleep(50ms)` never fires, reclaim and delayed-job promotion both happen on
//! the next `pop` call after the caller has advanced the virtual clock.
//!
//! The reaper is retained for production use where `pop` is infrequent
//! and background reclaim is needed.

use crate::error::FrameworkError;
use crate::lock;
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
}

pub struct MemoryQueueDriver {
    inner: Arc<Mutex<Inner>>,
    /// Async mutex guards the visibility DelayQueue so both `pop` and the reaper
    /// can poll it synchronously after acquiring the lock.
    visibility: Arc<AsyncMutex<DelayQueue<ReservationToken>>>,
    /// Async mutex guards the delayed DelayQueue — runs on Tokio's virtual clock
    /// so `tokio::time::advance` correctly fires expirations in paused-clock tests.
    delayed: Arc<AsyncMutex<DelayQueue<Envelope>>>,
    reaper: tokio::task::JoinHandle<()>,
}

impl Drop for MemoryQueueDriver {
    fn drop(&mut self) {
        self.reaper.abort();
    }
}

/// Drain all currently-expired visibility reservations from `dq` back into
/// the visible queue (push_front — reservation reclaim is priority).
/// The noop waker context must be created and dropped within this call —
/// callers must ensure it is not held across an await.
fn drain_expired(
    inner: &Mutex<Inner>,
    dq: &mut DelayQueue<ReservationToken>,
) -> Result<(), FrameworkError> {
    let waker = futures::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    let mut expired_tokens = Vec::new();
    while let Poll::Ready(Some(item)) = dq.poll_expired(&mut cx) {
        expired_tokens.push(item.into_inner());
    }
    // cx / waker are dropped here — no await has occurred.
    if !expired_tokens.is_empty() {
        let mut g = lock::lock(inner)?;
        for token in expired_tokens {
            if let Some(env) = g.reserved.remove(&token) {
                g.visible.push_front(env);
            }
        }
    }
    Ok(())
}

/// Drain all currently-expired delayed envelopes from `dq` into the visible
/// queue (push_back — delayed jobs join the back of the FIFO line).
/// The noop waker context must be created and dropped within this call —
/// callers must ensure it is not held across an await.
fn drain_delayed(
    inner: &Mutex<Inner>,
    dq: &mut DelayQueue<Envelope>,
) -> Result<(), FrameworkError> {
    let waker = futures::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    let mut ready = Vec::new();
    while let Poll::Ready(Some(item)) = dq.poll_expired(&mut cx) {
        ready.push(item.into_inner());
    }
    // cx / waker are dropped here — no await has occurred.
    if !ready.is_empty() {
        let mut g = lock::lock(inner)?;
        for env in ready {
            g.visible.push_back(env);
        }
    }
    Ok(())
}

impl MemoryQueueDriver {
    pub fn new() -> Self {
        let inner = Arc::new(Mutex::new(Inner::default()));
        let visibility = Arc::new(AsyncMutex::new(DelayQueue::new()));
        let delayed: Arc<AsyncMutex<DelayQueue<Envelope>>> =
            Arc::new(AsyncMutex::new(DelayQueue::new()));

        let inner2 = inner.clone();
        let visibility2 = visibility.clone();
        let delayed2 = delayed.clone();

        let reaper = tokio::spawn(async move {
            loop {
                // Promote expired delayed jobs into the visible queue.
                {
                    let mut dq = delayed2.lock().await;
                    drain_delayed(&inner2, &mut dq).expect("memory queue poisoned in reaper");
                    // Lock released here — before the sleep await.
                }

                // Reclaim expired visibility reservations. Acquire the async
                // mutex, then poll synchronously (no await while lock held).
                {
                    let mut dq = visibility2.lock().await;
                    drain_expired(&inner2, &mut dq).expect("memory queue poisoned in reaper");
                    // Lock released here — before the sleep await.
                }

                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        Self {
            inner,
            visibility,
            delayed,
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
        if env.available_at <= now {
            let mut g = lock::lock(&self.inner)?;
            g.visible.push_back(env);
        } else {
            // Compute delay on the Tokio virtual clock so paused-clock tests work.
            let delay = (env.available_at - now).to_std().unwrap_or(Duration::ZERO);
            let mut dq = self.delayed.lock().await;
            dq.insert(env, delay);
        }
        Ok(())
    }

    async fn pop(
        &self,
        visibility_timeout: Duration,
    ) -> Result<Option<Reservation>, FrameworkError> {
        // Drain expired delayed jobs into the visible queue (Tokio virtual clock).
        {
            let mut dq = self.delayed.lock().await;
            drain_delayed(&self.inner, &mut dq)?;
            // dq lock released here.
        }

        // Drain expired visibility reservations back into the visible queue.
        {
            let mut dq = self.visibility.lock().await;
            drain_expired(&self.inner, &mut dq)?;
            // dq lock released here.
        }

        let env_opt = {
            let mut g = lock::lock(&self.inner)?;
            g.visible.pop_front()
        };

        if let Some(env) = env_opt {
            let token = ReservationToken(Uuid::new_v4());
            {
                let mut g = lock::lock(&self.inner)?;
                g.reserved.insert(token.clone(), env.clone());
            }
            self.visibility
                .lock()
                .await
                .insert(token.clone(), visibility_timeout);
            Ok(Some(Reservation {
                envelope: env,
                token,
            }))
        } else {
            Ok(None)
        }
    }

    async fn ack(&self, token: &ReservationToken) -> Result<(), FrameworkError> {
        let mut g = lock::lock(&self.inner)?;
        g.reserved.remove(token);
        Ok(())
    }

    async fn nack(
        &self,
        token: &ReservationToken,
        requeue_delay: Duration,
    ) -> Result<(), FrameworkError> {
        let env = {
            let mut g = lock::lock(&self.inner)?;
            g.reserved.remove(token)
        };
        if let Some(mut env) = env {
            env.attempts += 1;
            if requeue_delay.is_zero() {
                let mut g = lock::lock(&self.inner)?;
                g.visible.push_front(env);
            } else {
                env.available_at = Utc::now()
                    + chrono::Duration::from_std(requeue_delay).map_err(|e| {
                        FrameworkError::internal(format!("requeue delay overflow: {e}"))
                    })?;
                // Insert into the Tokio-virtual-clock DelayQueue.
                let mut dq = self.delayed.lock().await;
                dq.insert(env, requeue_delay);
            }
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "memory"
    }
}
