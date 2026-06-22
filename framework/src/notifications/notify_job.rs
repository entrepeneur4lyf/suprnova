//! `SendNotificationJob` — processes `Notify::queue` dispatches via the
//! Phase 5A FROZEN envelope.
//!
//! The job carries the pre-resolved per-channel routes plus the
//! notification's `(name, payload)` pair. On `handle`, the worker
//! reconstructs the notification via the factory registry and fans it
//! out across the channels declared at queue time, using the bound
//! [`NotificationDispatcher`](crate::notifications::NotificationDispatcher). Channels declared at queue time but not
//! present on the dispatcher at execute time are logged at WARN level
//! and skipped, matching the sync dispatch path's contract.

use crate::error::FrameworkError;
use crate::notifications::{DynNotification, dispatcher_for_queue, factory_for};
use crate::queue::Job;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Queue payload produced by `Notify::queue` and consumed by the worker.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SendNotificationJob {
    /// Map of channel name to route — pre-resolved at queue time so the
    /// worker does not need to re-acquire a `Notifiable` handle.
    pub notifiable_route_per_channel: HashMap<String, String>,
    /// `Notification::notification_name()` of the queued notification (factory key).
    pub notification_name: String,
    /// Serialised notification data; rehydrated via the factory registry at execute time.
    pub notification_payload: serde_json::Value,
    /// Channels the notification declared at queue time. Filtered against
    /// the dispatcher's registered channels on dispatch.
    pub channels: Vec<String>,
}

#[async_trait]
impl Job for SendNotificationJob {
    fn job_name() -> &'static str {
        "Suprnova::SendNotification"
    }

    async fn handle(self) -> Result<(), FrameworkError> {
        let dispatcher = dispatcher_for_queue()?;
        let factory = factory_for(&self.notification_name)?;
        // Snapshot the payload before the factory consumes it — it doubles as
        // the `data` field on the lifecycle events below.
        let payload = self.notification_payload.clone();
        let notification: Box<dyn DynNotification> = factory(self.notification_payload)?;
        for channel_name in &self.channels {
            let Some(route) = self.notifiable_route_per_channel.get(channel_name) else {
                continue;
            };
            let Some(channel) = dispatcher.channel(channel_name) else {
                tracing::warn!(
                    channel = %channel_name,
                    notification = %self.notification_name,
                    "no channel registered (queued); skipping"
                );
                continue;
            };
            // Re-check the per-channel veto on the worker: consent /
            // opt-out / quiet-hours state can change between when the job
            // was enqueued and when it runs, so the queue-time decision is
            // not authoritative. Mirrors the synchronous dispatcher's
            // `should_send` short-circuit.
            if !notification.should_send(channel_name) {
                continue;
            }

            let payload_name = self.notification_name.clone();
            let channel_str = channel_name.clone();
            let data = payload.clone();

            // Lifecycle events, parity with the synchronous dispatcher: a
            // NotificationSending listener error vetoes the channel.
            let sending = crate::notifications::events::NotificationSending {
                notification: payload_name.clone(),
                channel: channel_str.clone(),
                route: route.clone(),
                data: data.clone(),
            };
            if let Err(veto) = crate::events::EventFacade::dispatch(sending).await {
                tracing::debug!(
                    channel = %channel_str,
                    notification = %payload_name,
                    reason = %veto,
                    "NotificationSending listener veto; skipping channel (queued)"
                );
                continue;
            }

            match channel.deliver(route, notification.as_ref()).await {
                Ok(()) => {
                    let _ = crate::events::EventFacade::dispatch_best_effort(
                        crate::notifications::events::NotificationSent {
                            notification: payload_name.clone(),
                            channel: channel_str.clone(),
                            route: route.clone(),
                            data: data.clone(),
                        },
                    )
                    .await;
                    // Post-send hook. Unlike the synchronous path, an
                    // after_sending error here must NOT fail the job: the
                    // delivery already succeeded, and a job retry re-runs the
                    // whole channel list, double-sending every channel that
                    // already delivered. Log and carry on.
                    if let Err(e) = notification.after_sending(channel_name) {
                        tracing::warn!(
                            channel = %channel_str,
                            notification = %payload_name,
                            error = %e,
                            "after_sending failed on queued delivery; not retrying \
                             (delivery already succeeded)"
                        );
                    }
                }
                Err(e) => {
                    let _ = crate::events::EventFacade::dispatch_best_effort(
                        crate::notifications::events::NotificationFailed {
                            notification: payload_name.clone(),
                            channel: channel_str.clone(),
                            route: route.clone(),
                            data: data.clone(),
                            error: e.to_string(),
                        },
                    )
                    .await;
                    return Err(e);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::{
        Channel, DynNotification, Notification, NotificationDispatcher,
        register_notification_factory, set_dispatcher,
    };
    use crate::queue::Job;
    use async_trait::async_trait;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    static DELIVER_HITS: AtomicU32 = AtomicU32::new(0);
    static AFTER_HITS: AtomicU32 = AtomicU32::new(0);

    /// Vetoes the `database` channel via `should_send` while permitting any
    /// other channel through. Counts its own `after_sending` calls.
    #[derive(Serialize, Deserialize, Debug, Clone)]
    struct ConsentAware;

    impl Notification for ConsentAware {
        fn notification_name() -> &'static str {
            "ConsentAware"
        }
        fn channels(&self) -> Vec<&'static str> {
            vec!["database"]
        }
        fn data(&self) -> serde_json::Value {
            serde_json::Value::Null
        }
        fn should_send(&self, channel: &str) -> bool {
            channel != "database"
        }
        fn after_sending(&self, _channel: &str) -> Result<(), FrameworkError> {
            AFTER_HITS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// Always permits delivery; used to confirm the worker invokes
    /// `after_sending` on the success path.
    #[derive(Serialize, Deserialize, Debug, Clone)]
    struct AlwaysSend;

    impl Notification for AlwaysSend {
        fn notification_name() -> &'static str {
            "AlwaysSend"
        }
        fn channels(&self) -> Vec<&'static str> {
            vec!["database"]
        }
        fn data(&self) -> serde_json::Value {
            serde_json::Value::Null
        }
        fn after_sending(&self, _channel: &str) -> Result<(), FrameworkError> {
            AFTER_HITS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct CountingChannel;

    #[async_trait]
    impl Channel for CountingChannel {
        fn name(&self) -> &'static str {
            "database"
        }
        async fn deliver(
            &self,
            _route: &str,
            _notification: &dyn DynNotification,
        ) -> Result<(), FrameworkError> {
            DELIVER_HITS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn job_for(name: &str) -> SendNotificationJob {
        let mut routes = HashMap::new();
        routes.insert("database".to_string(), "42".to_string());
        SendNotificationJob {
            notifiable_route_per_channel: routes,
            notification_name: name.to_string(),
            notification_payload: serde_json::Value::Null,
            channels: vec!["database".to_string()],
        }
    }

    // Regression: the queued path must honour `should_send`. Before the
    // fix, `handle` called `channel.deliver` unconditionally, so a channel
    // the notification vetoes (consent / opt-out / quiet-hours) was still
    // delivered on the worker even though the synchronous dispatcher
    // suppresses it.
    #[tokio::test]
    #[serial_test::serial]
    async fn queued_path_honours_should_send_veto() {
        DELIVER_HITS.store(0, Ordering::SeqCst);
        AFTER_HITS.store(0, Ordering::SeqCst);

        let dispatcher = NotificationDispatcher::new().register_channel(Arc::new(CountingChannel));
        set_dispatcher(Arc::new(dispatcher)).unwrap();
        register_notification_factory::<ConsentAware>().unwrap();

        job_for("ConsentAware").handle().await.unwrap();

        assert_eq!(
            DELIVER_HITS.load(Ordering::SeqCst),
            0,
            "a vetoed channel must not be delivered on the queued path",
        );
        assert_eq!(
            AFTER_HITS.load(Ordering::SeqCst),
            0,
            "after_sending must not run for a channel that was never delivered",
        );
    }

    // The worker runs `after_sending` exactly once per channel that
    // delivered successfully, matching the synchronous dispatcher.
    #[tokio::test]
    #[serial_test::serial]
    async fn queued_path_runs_after_sending_on_success() {
        DELIVER_HITS.store(0, Ordering::SeqCst);
        AFTER_HITS.store(0, Ordering::SeqCst);

        let dispatcher = NotificationDispatcher::new().register_channel(Arc::new(CountingChannel));
        set_dispatcher(Arc::new(dispatcher)).unwrap();
        register_notification_factory::<AlwaysSend>().unwrap();

        job_for("AlwaysSend").handle().await.unwrap();

        assert_eq!(
            DELIVER_HITS.load(Ordering::SeqCst),
            1,
            "the permitted channel is delivered once",
        );
        assert_eq!(
            AFTER_HITS.load(Ordering::SeqCst),
            1,
            "after_sending runs once on the queued success path",
        );
    }
}
