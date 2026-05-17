//! Notification channels.
//!
//! Concrete channel implementations: [`mail::MailChannel`] dispatches
//! through the bound mail transport; [`database::DatabaseChannel`]
//! persists each delivery as a row in the `notifications` table;
//! [`webpush::WebPushChannel`] POSTs an encrypted payload to a stored
//! browser push subscription endpoint via the vendored
//! [`crate::web_push::WebPushClient`]; [`broadcast::BroadcastChannelStub`]
//! is wired today and emits a tracing event — Phase 7B replaces it with
//! real WebSocket fan-out.

pub mod broadcast;
pub mod database;
pub mod mail;
pub mod webpush;
