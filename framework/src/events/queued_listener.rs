//! Bridge from an in-process event to a durable queue job.
//!
//! [`QueuedListener`] is the crash-durable tier of event handling. The event
//! itself stays in-process (unbounded, not serializable); when it fires, the
//! listener builds a [`Job`] from it and enqueues that job. Durability,
//! retries, and backoff then come from the queue — the job is persisted, so it
//! survives a process crash and is picked up by a worker after restart.
//!
//! Contrast the in-process queued-listener path ([`Event::queued`](super::Event::queued)
//! returning `true`): that is best-effort — bounded and retrying, and drained
//! on graceful shutdown, but its work does NOT survive a crash. Reach for
//! `QueuedListener` when the work must happen no matter what.
//!
//! ```rust,no_run
//! use suprnova::events::{Event, EventFacade, QueuedListener};
//! # use suprnova::queue::Job;
//! # use suprnova::FrameworkError;
//! # use async_trait::async_trait;
//! # use std::sync::Arc;
//! # #[derive(Debug, Clone)]
//! # struct UserRegistered { user_id: i64 }
//! # impl Event for UserRegistered {
//! #     fn event_name() -> &'static str { "UserRegistered" }
//! # }
//! # #[derive(serde::Serialize, serde::Deserialize)]
//! # struct SendWelcomeEmail { user_id: i64 }
//! # #[async_trait]
//! # impl Job for SendWelcomeEmail {
//! #     fn job_name() -> &'static str { "SendWelcomeEmail" }
//! #     async fn handle(self) -> Result<(), FrameworkError> { Ok(()) }
//! # }
//! # async fn ex() {
//! // `UserRegistered` is a normal (unbounded) event; `SendWelcomeEmail` is a Job.
//! EventFacade::listen::<UserRegistered, _>(Arc::new(
//!     QueuedListener::<UserRegistered, SendWelcomeEmail>::new(
//!         |e| SendWelcomeEmail { user_id: e.user_id },
//!     ),
//! ))
//! .await;
//! # }
//! ```
//!
//! Register `QueuedListener` for a synchronous (non-`queued`) event: the
//! durability lives in the queue, so the listener only needs to enqueue —
//! which is fast — and the request that fired the event waits just for that
//! enqueue, not for the job to run.

use super::{Event as EventTrait, Listener};
use crate::FrameworkError;
use crate::queue::{Job, Queue};
use async_trait::async_trait;
use std::marker::PhantomData;
use std::sync::Arc;

/// A [`Listener`] that turns event `E` into durable job `J` and enqueues it via
/// [`Queue::push`]. See the module docs for when to use this versus an
/// in-process queued listener.
pub struct QueuedListener<E, J> {
    build: Arc<dyn Fn(&E) -> J + Send + Sync>,
    _marker: PhantomData<fn() -> (E, J)>,
}

impl<E, J> QueuedListener<E, J>
where
    E: EventTrait,
    J: Job,
{
    /// Build a listener that maps each `E` to a `J` and enqueues it.
    pub fn new(build: impl Fn(&E) -> J + Send + Sync + 'static) -> Self {
        Self {
            build: Arc::new(build),
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<E, J> Listener<E> for QueuedListener<E, J>
where
    E: EventTrait,
    J: Job,
{
    async fn handle(&self, event: &E) -> Result<(), FrameworkError> {
        let job = (self.build)(event);
        Queue::push(job).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::testing;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;

    #[derive(Debug, Clone)]
    struct UserRegistered {
        user_id: i64,
    }
    impl EventTrait for UserRegistered {
        fn event_name() -> &'static str {
            "UserRegistered"
        }
    }

    #[derive(Serialize, Deserialize)]
    struct SendWelcome {
        user_id: i64,
    }
    #[async_trait]
    impl Job for SendWelcome {
        fn job_name() -> &'static str {
            "SendWelcome"
        }
        async fn handle(self) -> Result<(), FrameworkError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn handle_builds_job_from_event_and_enqueues_it() {
        let _fake = testing::install_fake();
        let listener = QueuedListener::<UserRegistered, SendWelcome>::new(|e| SendWelcome {
            user_id: e.user_id,
        });
        listener
            .handle(&UserRegistered { user_id: 42 })
            .await
            .unwrap();
        testing::assert_pushed::<SendWelcome>(|j| j.user_id == 42);
    }

    #[tokio::test]
    async fn dispatched_event_routes_through_the_listener_to_the_queue() {
        use crate::events::EventDispatcher;
        let _fake = testing::install_fake();
        let d = EventDispatcher::new();
        d.listen::<UserRegistered, _>(Arc::new(
            QueuedListener::<UserRegistered, SendWelcome>::new(|e| SendWelcome {
                user_id: e.user_id,
            }),
        ))
        .await;
        d.dispatch(UserRegistered { user_id: 7 }).await.unwrap();
        testing::assert_pushed::<SendWelcome>(|j| j.user_id == 7);
    }
}
