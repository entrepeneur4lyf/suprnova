//! Channel-based pub/sub for real-time apps.
//!
//! `BroadcastHub` is the framework's primitive for publishing events
//! by channel name and subscribing handlers to receive them. The
//! default `InMemoryBroadcastHub` runs entirely in-process via
//! `tokio::sync::broadcast`; the `broadcasting-fanout` feature (T11)
//! adds a sea-streamer-backed implementation for multi-process
//! fanout.
//!
//! WebSocket subscribers are served by `BroadcastingWsHandler` (T5),
//! which wires the JSON-envelope subscribe protocol against the hub.

mod broadcastable;
mod channel;
mod handler;
mod hub;
mod protocol;
pub(crate) mod request_socket;
mod testing;

#[cfg(feature = "broadcasting-fanout")]
pub mod fanout;

pub use broadcastable::{BroadcastListener, Broadcastable};
pub use channel::{
    BoxedChannel, Channel, ChannelParams, ChannelRegistry, PresenceChannel, PrivateChannel,
};
pub use handler::{BroadcastingWsHandler, DEFAULT_MAX_SUBSCRIPTIONS_PER_CONNECTION};
pub use hub::{BroadcastEnvelope, BroadcastHub, InMemoryBroadcastHub};
pub use protocol::{ClientFrame, ServerFrame};
pub use testing::RecordingBroadcastHub;
