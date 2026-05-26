//! Web Push client for Suprnova.
//!
//! Ported from `web-push 0.11.0` with the upstream `isahc`/`hyper 0.14`
//! HTTP layer replaced by Suprnova's pinned `reqwest 0.13`. The crypto
//! (VAPID + ECE) is identical to upstream — only the transport changed.

pub mod client;
pub mod ece;
pub mod error;
pub mod payload;
pub mod vapid;

pub use client::{PushResponse, SubscriptionInfo, WebPushClient};
pub use error::WebPushError;
pub use payload::{ContentEncoding, Payload};
pub use vapid::{VapidClaims, VapidKey, VapidSigner};
