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
            channel.deliver(route, notification.as_ref()).await?;
        }
        Ok(())
    }
}
