//! Re-export of the vendored `suprnova-web-push` crate.
//!
//! Provides Web Push Protocol support (RFC 8030) for sending push notifications
//! to subscribed clients.

pub use suprnova_web_push::{
    ContentEncoding, PushResponse, SubscriptionInfo, VapidClaims, VapidKey, VapidSigner,
    WebPushClient, WebPushError,
};
