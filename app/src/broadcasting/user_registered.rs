//! Public broadcasting channel for the `UserRegistered` events feed.
//!
//! Any subscriber — SSE controller, WS client — that wishes to
//! receive user-registration activity connects to the
//! `"user_registered"` channel. Public channels accept every
//! subscriber (the default `authorize` returns `true`).

use async_trait::async_trait;
use suprnova::broadcasting::Channel;

/// Public channel that fans out `UserRegistered` events to all
/// interested subscribers. Registered in `bootstrap::register()` so
/// the `BroadcastingWsHandler` can look it up by name when a client
/// sends a `{"type":"subscribe","channel":"user_registered"}` frame.
pub struct UserRegisteredChannel;

#[async_trait]
impl Channel for UserRegisteredChannel {
    fn name(&self) -> &'static str {
        "user_registered"
    }
}
