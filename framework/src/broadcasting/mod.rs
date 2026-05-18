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

pub use hub::{BroadcastEnvelope, BroadcastHub, InMemoryBroadcastHub};
pub use channel::{BoxedChannel, Channel, ChannelRegistry, PresenceChannel, PrivateChannel};
// Re-exports for the other submodules land in T4/T5/T7.
