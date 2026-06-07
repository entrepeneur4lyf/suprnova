//! Re-export of the vendored `suprnova-web-push` crate.
//!
//! Provides Web Push Protocol support (RFC 8030) for sending push notifications
//! to subscribed clients. The items here are the low-level protocol surface —
//! VAPID signing, AES128GCM payload encryption, and a transport client.
//!
//! Most applications use web push through the notifications subsystem instead
//! of calling [`WebPushClient`] directly:
//!
//! - [`crate::WebPushChannel`] adapts a [`WebPushClient`] to the
//!   [`crate::notifications::Channel`] trait so a `Notification` declaring
//!   `webpush` in its `channels()` fans out automatically when dispatched
//!   via `Notify::send` or `Notify::queue`.
//! - [`crate::notifications::SendNotificationJob`] wraps queued dispatch,
//!   so background workers replay the same channel fan-out (web push
//!   included) off the queue backend.
//! - A `Notifiable` recipient supplies its push endpoint by returning the
//!   JSON-serialized [`SubscriptionInfo`] from `route_for("webpush")`.
//!
//! See `manual/notifications.md` for end-to-end wiring including
//! channel registration and queued delivery.

pub use suprnova_web_push::{
    ContentEncoding, EndpointPolicy, PushResponse, SubscriptionInfo, VapidClaims, VapidKey,
    VapidSigner, WebPushClient, WebPushError,
};
