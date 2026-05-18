//! Channel trait + registry.
//!
//! A `Channel` is a named subscription target with an authorization
//! hook and optional presence semantics. Channels are registered
//! with the `ChannelRegistry` at bootstrap (typically inside
//! `bootstrap::register`); T5's BroadcastingWsHandler looks them
//! up by name when parsing client `subscribe` envelopes.

use crate::http::Request;
use crate::FrameworkError;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// A named subscription target.
///
/// Public channels accept any subscriber (default `authorize`
/// returns `true`). Private channels override `authorize` to gate by
/// session/token. Presence channels additionally publish
/// `presence.joined` / `presence.left` events — implement
/// [`PresenceChannel`] for them.
///
/// # Example
///
/// ```rust,ignore
/// use async_trait::async_trait;
/// use suprnova::broadcasting::Channel;
/// use suprnova::http::Request;
/// use serde_json::Value;
///
/// pub struct OrderUpdates;
///
/// #[async_trait]
/// impl Channel for OrderUpdates {
///     fn name(&self) -> &'static str { "order.updates" }
/// }
/// ```
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    /// Canonical channel name (e.g. `"notifications"`, `"chat.{room_id}"`).
    /// Used as the registry key; T5's WS handler matches client
    /// `subscribe` requests against this exact string.
    fn name(&self) -> &'static str;

    /// Authorize a subscribe request. Default = public (returns
    /// `true`). Override to gate by session, role, room membership,
    /// etc. The `data` argument is the optional payload the client
    /// sent alongside the subscribe envelope (typically an auth
    /// token or signed channel-bind blob).
    async fn authorize(&self, _req: &Request, _data: &Value) -> bool {
        true
    }
}

/// Marker trait for private channels. Used as a type-level signal
/// in docs and for future tooling (e.g. a clippy lint or an audit
/// pass that flags channels overriding `authorize` without
/// implementing `PrivateChannel`).
pub trait PrivateChannel: Channel {}

/// Channel with presence semantics — emits `presence.joined` /
/// `presence.left` events when subscribers attach or detach. T6
/// wires the actual emission in `BroadcastingWsHandler`.
#[async_trait]
pub trait PresenceChannel: Channel {
    /// Member info to broadcast on join/leave events. Typically
    /// includes a user id and any public profile data; should
    /// NEVER include secrets, tokens, or PII the channel
    /// subscribers shouldn't see.
    async fn member_info(&self, req: &Request) -> Result<Value, FrameworkError>;
}

/// Type-erased channel handle stored in the registry.
pub type BoxedChannel = Arc<dyn Channel>;

/// Registry of channels available for subscription. Populated at
/// bootstrap; consumed by `BroadcastingWsHandler` (T5).
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::broadcasting::{ChannelRegistry, Channel};
///
/// let mut registry = ChannelRegistry::new();
/// registry.register(OrderUpdates);
/// registry.register(ChatChannel { room_id: 42 });
///
/// let chan = registry.resolve("order.updates").expect("registered");
/// ```
pub struct ChannelRegistry {
    channels: HashMap<String, BoxedChannel>,
}

impl ChannelRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            channels: HashMap::new(),
        }
    }

    /// Register a channel. Subsequent subscribes against `channel.name()`
    /// resolve to this instance. Re-registering the same name
    /// replaces the previous entry.
    pub fn register<C: Channel + 'static>(&mut self, channel: C) {
        let name = channel.name().to_string();
        self.channels.insert(name, Arc::new(channel));
    }

    /// Look up a channel by name. Returns `None` if no channel with
    /// that name was registered.
    pub fn resolve(&self, name: &str) -> Option<BoxedChannel> {
        self.channels.get(name).cloned()
    }

    /// Number of registered channels.
    pub fn len(&self) -> usize {
        self.channels.len()
    }

    /// Returns `true` if no channels have been registered.
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self::new()
    }
}
