//! Web Push client for Suprnova.
//!
//! Ported from `web-push 0.11.0` with the upstream `isahc`/`hyper 0.14`
//! HTTP layer replaced by Suprnova's pinned `reqwest 0.13`. The crypto
//! (VAPID + ECE) is identical to upstream — only the transport changed.

pub mod error;
pub mod vapid;
pub mod ece;
pub mod payload;
pub mod client;

pub use error::WebPushError;
pub use vapid::{VapidSigner, VapidKey, VapidClaims};
pub use payload::{Payload, ContentEncoding};
pub use client::{WebPushClient, SubscriptionInfo, PushResponse};
