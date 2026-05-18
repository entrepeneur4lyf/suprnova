//! App-specific broadcasting channels.
//!
//! Channels are registered in `bootstrap::register()` via a
//! `ChannelRegistry` bound into the App container. The framework's
//! `BroadcastingWsHandler` resolves the registry at route-build time
//! so WS clients can subscribe by name.

pub mod chat;
pub mod user_registered;

pub use chat::ChatChannel;
pub use user_registered::UserRegisteredChannel;
