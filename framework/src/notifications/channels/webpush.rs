//! Web push notification channel.
//!
//! Delivers notifications via the vendored
//! [`crate::web_push::WebPushClient`]. The route returned by
//! `Notifiable::route_for("webpush")` MUST be a JSON-encoded
//! [`SubscriptionInfo`] (`{"endpoint": "...", "keys": {"p256dh": "...",
//! "auth": "..."}}`) — this matches the shape browsers hand back from
//! `PushSubscription.toJSON()`, so callers can store the subscription
//! verbatim and return it untouched. Subscriptions that the push service
//! has invalidated (HTTP 404/410) are logged at WARN and skipped, not
//! propagated as errors — Phase 5B stops short of automatic cleanup but
//! the warn log gives operators a paper trail to act on.
//!
//! ## Why `Arc<WebPushClient>`
//!
//! `WebPushClient` wraps a `VapidSigner` which wraps a private
//! `ES256KeyPair`. None of those are `Clone` (private keys shouldn't be
//! casually duplicated), and constructing a fresh signer for every channel
//! registration would mean N independent VAPID identities for the same
//! application. Wrapping in `Arc` lets a single signed identity back every
//! registration and every concurrent delivery.

use crate::error::FrameworkError;
use crate::notifications::{Channel, DynNotification};
use crate::web_push::{ContentEncoding, SubscriptionInfo, WebPushClient, WebPushError};
use async_trait::async_trait;
use std::sync::Arc;

/// Notification channel that POSTs an encrypted payload to a stored
/// browser push subscription endpoint.
///
/// Construct with an `Arc<WebPushClient>` so a single VAPID-signing
/// client can be shared across channel registrations and concurrent
/// fan-out without re-constructing the signer (which is not `Clone`).
/// `ttl_secs` is forwarded as the `TTL` header — the push service caps
/// this and discards undelivered messages after that window.
pub struct WebPushChannel {
    client: Arc<WebPushClient>,
    ttl_secs: u32,
}

impl WebPushChannel {
    pub fn new(client: Arc<WebPushClient>, ttl_secs: u32) -> Self {
        Self { client, ttl_secs }
    }
}

#[async_trait]
impl Channel for WebPushChannel {
    fn name(&self) -> &'static str {
        "webpush"
    }

    async fn deliver(
        &self,
        route: &str,
        notification: &dyn DynNotification,
    ) -> Result<(), FrameworkError> {
        let subscription: SubscriptionInfo = serde_json::from_str(route).map_err(|e| {
            FrameworkError::internal(format!(
                "WebPushChannel: subscription JSON decode: {e}"
            ))
        })?;
        let payload = serde_json::to_vec(&notification.data())
            .map_err(|e| FrameworkError::internal(format!("WebPushChannel: payload encode: {e}")))?;

        match self
            .client
            .send(
                &subscription,
                &payload,
                ContentEncoding::Aes128Gcm,
                self.ttl_secs,
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(WebPushError::SubscriptionGone) => {
                // The push service told us this subscription is dead (404/410).
                // Surface a structured warn — callers should remove the stored
                // subscription, but we don't fail dispatch over it because the
                // notification "succeeded" in the only sense available: it
                // reached a terminal state with no recipient to retry against.
                tracing::warn!(
                    channel = "webpush",
                    endpoint = %subscription.endpoint,
                    notification = %notification.name(),
                    "webpush subscription gone (404/410); caller should remove"
                );
                Ok(())
            }
            Err(e) => Err(FrameworkError::internal(format!("WebPushChannel: {e}"))),
        }
    }
}
