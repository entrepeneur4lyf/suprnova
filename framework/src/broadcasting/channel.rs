//! Channel trait + registry.
//!
//! A `Channel` is a named subscription target with an authorization
//! hook and optional presence semantics. Channels are registered
//! with the `ChannelRegistry` at bootstrap (typically inside
//! `bootstrap::register`); T5's BroadcastingWsHandler looks them
//! up by name when parsing client `subscribe` envelopes.

use crate::FrameworkError;
use crate::http::Request;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// A named subscription target with auth + presence semantics.
///
/// Channels are registered with [`ChannelRegistry`] at bootstrap;
/// [`BroadcastingWsHandler`](crate::broadcasting::BroadcastingWsHandler)
/// resolves them by name when parsing client envelopes.
///
/// # Default behavior — asymmetric by design
///
/// - [`authorize`](Self::authorize) defaults to `true` (subscribe is
///   **public by default**). Most channels accept any subscriber; private
///   channels override this to gate by session, role, etc.
/// - [`authorize_publish`](Self::authorize_publish) defaults to `false`
///   (client-side publish is **denied by default**). Most broadcasting
///   channels only accept server-side events (via `Broadcastable` +
///   `EventFacade::dispatch`); channels that want client-initiated publishes
///   (chat rooms, presence pings) explicitly opt in by overriding the hook.
///
/// The asymmetry is intentional: it fails closed on the dangerous action
/// (writing data) and open on the safe one (reading public data). When in
/// doubt, leave both defaults alone.
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
    /// Canonical channel name (e.g. `"notifications"`, `"chat.lobby"`).
    /// Used as the registry key; the WS handler matches client `subscribe`
    /// requests against this **exact** string — there is no `{param}`
    /// templating, so a channel name is a fixed `&'static str`.
    fn name(&self) -> &'static str;

    /// Authorize a subscribe request. Default = public (returns
    /// `true`). Override to gate by session, role, room membership,
    /// etc. The `data` argument is the optional payload the client
    /// sent alongside the subscribe envelope (typically an auth
    /// token or signed channel-bind blob).
    async fn authorize(&self, _req: &Request, _data: &Value) -> bool {
        true
    }

    /// Authorize a client-initiated publish. Default: `false` (deny).
    ///
    /// Override to allow client-side publishes on channels that support
    /// them (chat rooms, presence updates, etc.). The channel impl
    /// typically inspects the event name, data shape, and the
    /// subscriber's identity (via the `Request`) before returning `true`.
    ///
    /// The default is `false` (fail closed): a channel that doesn't
    /// explicitly opt in to client publishes rejects them. Most
    /// server-side broadcasting channels never want client-initiated
    /// events; this default matches that expectation. Note that this
    /// hook only governs `ClientFrame::Publish` received over the
    /// WebSocket connection — server-side `hub.publish()` calls are
    /// unaffected and bypass this gate entirely.
    async fn authorize_publish(&self, _req: &Request, _event: &str, _data: &Value) -> bool {
        false
    }

    /// If this channel carries presence semantics, return `Some(self)`
    /// cast as `&dyn PresenceChannel`. Default: `None` (non-presence).
    ///
    /// Implementers of [`PresenceChannel`] must override this to return
    /// `Some(self)` — this is the detection hook used by
    /// `BroadcastingWsHandler` at subscribe time (T6).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn presence_info<'a>(&'a self) -> Option<&'a dyn PresenceChannel> {
    ///     Some(self)
    /// }
    /// ```
    fn presence_info(&self) -> Option<&dyn PresenceChannel> {
        None
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
/// registry.register(LobbyChat);
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
    ///
    /// # Panics
    ///
    /// Panics if the channel's `name()` starts with `"__"`. Names beginning
    /// with `__` are reserved for framework meta-channels (e.g.
    /// `__presence__` for cross-process presence replication). Attempting to
    /// register a user channel with such a name is a programming error that is
    /// caught at registration time rather than at runtime.
    pub fn register<C: Channel + 'static>(&mut self, channel: C) {
        let name = channel.name();
        assert!(
            !name.starts_with("__"),
            "Channel name '{}' starts with '__' which is reserved for framework \
             meta-channels (e.g. __presence__ for cross-process presence \
             replication). Pick a different name.",
            name
        );
        self.channels.insert(name.to_string(), Arc::new(channel));
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct DummyChannel {
        name: &'static str,
    }

    #[async_trait]
    impl Channel for DummyChannel {
        fn name(&self) -> &'static str {
            self.name
        }
    }

    #[test]
    fn register_normal_channel_succeeds() {
        let mut registry = ChannelRegistry::new();
        registry.register(DummyChannel { name: "chat.lobby" });
        assert!(registry.resolve("chat.lobby").is_some());
    }

    #[test]
    #[should_panic(expected = "starts with '__'")]
    fn register_reserved_name_panics() {
        let mut registry = ChannelRegistry::new();
        // `__presence__` is a framework-reserved name; registration must panic.
        registry.register(DummyChannel {
            name: "__presence__",
        });
    }

    #[test]
    #[should_panic(expected = "starts with '__'")]
    fn register_any_double_underscore_prefix_panics() {
        let mut registry = ChannelRegistry::new();
        // Any `__foo__` prefix is reserved, not just `__presence__`.
        registry.register(DummyChannel { name: "__rpc__" });
    }
}
