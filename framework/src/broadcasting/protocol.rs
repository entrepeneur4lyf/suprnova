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
        channel: String,
        #[serde(default)]
        data: Value,
    },
    /// Unsubscribe from a previously subscribed channel.
    Unsubscribe { channel: String },
    /// Client-published event. Rare in practice — most events
    /// come from server-side dispatch via `Broadcastable`. Allowed
    /// for symmetric apps; the channel's authorize gate still
    /// applies.
    Publish {
        channel: String,
        event: String,
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
    Connected { socket_id: String },
    /// Acknowledges a `Subscribe` request.
    Subscribed { channel: String },
    /// Acknowledges an `Unsubscribe` request.
    Unsubscribed { channel: String },
    /// A published event being pushed to the subscriber.
    Event {
        channel: String,
        event: String,
        data: Value,
    },
    /// Error response — surfaces parse failures, auth rejections,
    /// channel-not-found, etc. `channel` is `None` for envelope-level
    /// errors that aren't tied to a specific channel.
    Error {
        channel: Option<String>,
        reason: String,
    },
}
