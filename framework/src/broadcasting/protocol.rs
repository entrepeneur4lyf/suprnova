//! JSON-envelope wire protocol for the BroadcastingWsHandler.
//!
//! Two enums define the wire shape between a JS or native
//! broadcasting client and the framework's WS handler:
//!
//! - [`ClientFrame`] is what the client sends (subscribe, unsubscribe,
//!   publish).
//! - [`ServerFrame`] is what the server sends back (subscribed,
//!   unsubscribed, event push, error).
//!
//! Both are tagged with `"action"` and use snake_case discriminants so
//! the JSON matches what JS clients write idiomatically:
//!
//! ```json
//! { "action": "subscribe", "channel": "chat.42", "data": { } }
//! { "action": "event", "channel": "chat.42",
//!   "event": "MessagePosted", "data": { "text": "hi" } }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Inbound from the client.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ClientFrame {
    /// Subscribe to a channel. Optional `data` carries auth tokens
    /// or other channel-binding info; the channel's `authorize`
    /// hook sees this payload.
    Subscribe {
        /// Channel name to subscribe to.
        channel: String,
        /// Auth / binding payload forwarded to the channel's `authorize` hook.
        #[serde(default)]
        data: Value,
    },
    /// Unsubscribe from a previously subscribed channel.
    Unsubscribe {
        /// Channel name to unsubscribe from.
        channel: String,
    },
    /// Client-published event. Rare in practice — most events
    /// come from server-side dispatch via `Broadcastable`. Allowed
    /// for symmetric apps; the channel's authorize gate still
    /// applies.
    Publish {
        /// Channel to publish into.
        channel: String,
        /// Event name carried in the published frame.
        event: String,
        /// Event payload, forwarded verbatim to subscribers.
        #[serde(default)]
        data: Value,
    },
}

/// Outbound to the client.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ServerFrame {
    /// Sent once, first, when the connection opens. Carries the per-connection
    /// `socket_id` the client echoes back as the `X-Socket-ID` header on HTTP
    /// requests so server-side `broadcast_to_others` can exclude it. Mirrors
    /// Pusher's `connection_established`.
    Connected {
        /// Per-connection socket id; echoed back via `X-Socket-ID` for self-exclusion.
        socket_id: String,
    },
    /// Acknowledges a `Subscribe` request.
    Subscribed {
        /// Channel the client is now subscribed to.
        channel: String,
    },
    /// Acknowledges an `Unsubscribe` request.
    Unsubscribed {
        /// Channel the client was unsubscribed from.
        channel: String,
    },
    /// A published event being pushed to the subscriber.
    Event {
        /// Channel the event was published on.
        channel: String,
        /// Event name.
        event: String,
        /// Event payload, forwarded verbatim from the publisher.
        data: Value,
    },
    /// Subscriber lagged past the server's per-channel ring buffer and
    /// `skipped` envelopes were dropped on this connection. The client's
    /// local state on `channel` is now stale; it should refetch or
    /// resync before processing further events. Sent immediately after
    /// the lag is detected; the forwarder continues delivering subsequent
    /// frames, but the gap is not recoverable from the server side.
    Lagged {
        /// Channel whose ring buffer the subscriber fell behind on.
        channel: String,
        /// Number of envelopes dropped on this connection before the lag was reported.
        skipped: u64,
    },
    /// Error response — surfaces parse failures, auth rejections,
    /// channel-not-found, etc. `channel` is `None` for envelope-level
    /// errors that aren't tied to a specific channel.
    Error {
        /// Channel the error applies to, or `None` for envelope-level errors.
        channel: Option<String>,
        /// Human-readable error reason.
        reason: String,
    },
}
