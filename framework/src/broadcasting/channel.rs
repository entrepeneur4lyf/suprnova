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

/// Parameters captured from a parameterized channel name.
///
/// When a channel's [`Channel::name`] contains `{param}` segments (e.g.
/// `"orders.{id}"`), [`ChannelRegistry::resolve`] matches a concrete
/// subscription (`"orders.42"`) against the pattern and captures each
/// `{param}` → value pair here, so the channel's hooks can authorize against
/// the specific instance. A fixed-name channel resolves with an empty
/// `ChannelParams`. Mirrors the bound parameters Laravel passes to a
/// `Broadcast::channel('orders.{id}', fn ($user, $id) => …)` callback.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelParams {
    pairs: Vec<(String, String)>,
}

impl ChannelParams {
    /// The captured value for `key`, or `None` if the pattern had no
    /// matching `{key}` segment.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// `true` when no parameters were captured (a fixed-name channel).
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// Number of captured parameters.
    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    /// Iterate the captured `(name, value)` pairs in pattern order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.pairs.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// Match a concrete channel name against a (possibly parameterized) pattern,
/// returning the captured params on success. Both are split on `.`; a
/// `{name}` pattern segment binds exactly one concrete segment, every other
/// segment must match literally, and the segment counts must be equal.
fn match_channel_pattern(pattern: &str, concrete: &str) -> Option<ChannelParams> {
    let pat: Vec<&str> = pattern.split('.').collect();
    let con: Vec<&str> = concrete.split('.').collect();
    if pat.len() != con.len() {
        return None;
    }
    let mut params = ChannelParams::default();
    for (p, c) in pat.iter().zip(con.iter()) {
        match p.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            Some(name) => params.pairs.push(((*name).to_string(), (*c).to_string())),
            None if p == c => {}
            None => return None,
        }
    }
    Some(params)
}

/// Count the literal (non-`{param}`) segments of a pattern — used to rank
/// competing matches so the most specific pattern wins.
fn pattern_literal_count(pattern: &str) -> usize {
    pattern
        .split('.')
        .filter(|s| !(s.starts_with('{') && s.ends_with('}')))
        .count()
}

/// Whether a channel name contains any `{param}` segment.
fn is_pattern(name: &str) -> bool {
    name.split('.')
        .any(|s| s.starts_with('{') && s.ends_with('}'))
}

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
    /// Canonical channel name. Used as the registry key; the WS handler
    /// matches client `subscribe` requests against it.
    ///
    /// May be a **fixed** name (`"notifications"`, `"chat.lobby"`) or a
    /// **pattern** with `{param}` segments (`"orders.{id}"`,
    /// `"chat.{room}.{topic}"`). Each `{param}` binds exactly one concrete
    /// dot-segment; the captured values reach [`authorize`](Self::authorize)
    /// (and the other hooks) as [`ChannelParams`]. Fixed names win over a
    /// competing pattern, and among patterns the most literal one wins.
    fn name(&self) -> &'static str;

    /// Authorize a subscribe request. Default = public (returns `true`).
    /// Override to gate by session, role, room membership, etc.
    ///
    /// `params` carries the values captured from a parameterized
    /// [`name`](Self::name) — e.g. `params.get("id")` for `"orders.{id}"` —
    /// and is empty for fixed-name channels. `data` is the optional payload
    /// the client sent alongside the subscribe envelope (typically an auth
    /// token or signed channel-bind blob).
    async fn authorize(&self, _req: &Request, _params: &ChannelParams, _data: &Value) -> bool {
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
    ///
    /// `params` carries the captured values from a parameterized
    /// [`name`](Self::name), as in [`authorize`](Self::authorize).
    async fn authorize_publish(
        &self,
        _req: &Request,
        _params: &ChannelParams,
        _event: &str,
        _data: &Value,
    ) -> bool {
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
///
/// # Two-part contract — easy to half-implement
///
/// Implementing `PresenceChannel` is **not enough** on its own. The
/// `BroadcastingWsHandler` detects presence by calling
/// [`Channel::presence_info`] — whose default returns `None`. A channel
/// that implements `PresenceChannel` but forgets to override
/// `presence_info` is wired as a non-presence channel: subscribes work,
/// but `presence.joined` / `presence.left` / `presence.here` never fire
/// and `member_info` is never called. The compiler cannot catch this on
/// stable Rust, so both halves must be supplied by hand:
///
/// ```rust,ignore
/// use async_trait::async_trait;
/// use suprnova::broadcasting::{Channel, ChannelParams, PresenceChannel};
/// use suprnova::http::Request;
/// use suprnova::FrameworkError;
/// use serde_json::{Value, json};
///
/// pub struct Lobby;
///
/// #[async_trait]
/// impl Channel for Lobby {
///     fn name(&self) -> &'static str { "presence.lobby" }
///
///     // Required for presence semantics to fire — without this override
///     // `PresenceChannel` is wired but inert.
///     fn presence_info(&self) -> Option<&dyn PresenceChannel> {
///         Some(self)
///     }
/// }
///
/// #[async_trait]
/// impl PresenceChannel for Lobby {
///     async fn member_info(
///         &self,
///         _req: &Request,
///         _params: &ChannelParams,
///     ) -> Result<Value, FrameworkError> {
///         Ok(json!({ "user_id": 42 }))
///     }
/// }
/// ```
#[async_trait]
pub trait PresenceChannel: Channel {
    /// Member info to broadcast on join/leave events. Typically
    /// includes a user id and any public profile data; should
    /// NEVER include secrets, tokens, or PII the channel
    /// subscribers shouldn't see. `params` carries the captured values
    /// from a parameterized [`name`](Channel::name) (e.g. the room id for
    /// `"presence.{room}"`).
    async fn member_info(
        &self,
        req: &Request,
        params: &ChannelParams,
    ) -> Result<Value, FrameworkError>;
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
/// let (chan, _params) = registry.resolve("order.updates").expect("registered");
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

    /// Look up a channel by the concrete subscription name, returning the
    /// channel plus any [`ChannelParams`] captured from a parameterized name.
    ///
    /// Resolution order: an **exact** (fixed-name) registration wins outright;
    /// otherwise the concrete name is matched against every registered
    /// `{param}` pattern and the **most specific** match wins — most literal
    /// segments first, then the lexicographically smallest pattern as a
    /// deterministic tie-break. Returns `None` when nothing matches.
    pub fn resolve(&self, name: &str) -> Option<(BoxedChannel, ChannelParams)> {
        // Exact match wins — covers every fixed-name channel, and a literal
        // registration that competes with a pattern (`chat.lobby` beats
        // `chat.{room}`).
        if let Some(ch) = self.channels.get(name) {
            return Some((Arc::clone(ch), ChannelParams::default()));
        }
        // Otherwise rank the matching patterns: most literal segments wins,
        // smaller pattern string breaks ties so resolution is deterministic.
        self.channels
            .iter()
            .filter_map(|(pat, ch)| {
                if !is_pattern(pat) {
                    return None;
                }
                match_channel_pattern(pat, name).map(|params| (pat.clone(), Arc::clone(ch), params))
            })
            .max_by(|a, b| {
                pattern_literal_count(&a.0)
                    .cmp(&pattern_literal_count(&b.0))
                    .then_with(|| b.0.cmp(&a.0))
            })
            .map(|(_, ch, params)| (ch, params))
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

    #[test]
    fn resolve_exact_returns_empty_params() {
        let mut r = ChannelRegistry::new();
        r.register(DummyChannel { name: "chat.lobby" });
        let (ch, params) = r.resolve("chat.lobby").expect("exact match");
        assert_eq!(ch.name(), "chat.lobby");
        assert!(params.is_empty());
    }

    #[test]
    fn resolve_pattern_captures_params() {
        let mut r = ChannelRegistry::new();
        r.register(DummyChannel {
            name: "orders.{id}",
        });
        let (ch, params) = r.resolve("orders.42").expect("pattern match");
        assert_eq!(ch.name(), "orders.{id}");
        assert_eq!(params.get("id"), Some("42"));
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn resolve_multi_param_pattern_captures_each_segment() {
        let mut r = ChannelRegistry::new();
        r.register(DummyChannel {
            name: "chat.{room}.{topic}",
        });
        let (_, params) = r.resolve("chat.7.general").expect("match");
        assert_eq!(params.get("room"), Some("7"));
        assert_eq!(params.get("topic"), Some("general"));
    }

    #[test]
    fn resolve_exact_beats_pattern_but_other_values_fall_through() {
        let mut r = ChannelRegistry::new();
        r.register(DummyChannel {
            name: "orders.{id}",
        });
        r.register(DummyChannel {
            name: "orders.featured",
        });
        // The literal registration wins for its own exact name.
        let (ch, params) = r.resolve("orders.featured").expect("exact");
        assert_eq!(ch.name(), "orders.featured");
        assert!(params.is_empty());
        // Other ids still fall through to the pattern.
        let (ch2, params2) = r.resolve("orders.99").expect("pattern");
        assert_eq!(ch2.name(), "orders.{id}");
        assert_eq!(params2.get("id"), Some("99"));
    }

    #[test]
    fn resolve_most_specific_pattern_wins() {
        let mut r = ChannelRegistry::new();
        r.register(DummyChannel { name: "{a}.{b}" }); // 0 literal segments
        r.register(DummyChannel {
            name: "orders.{id}",
        }); // 1 literal — more specific
        let (ch, params) = r.resolve("orders.5").expect("match");
        assert_eq!(ch.name(), "orders.{id}");
        assert_eq!(params.get("id"), Some("5"));
    }

    #[test]
    fn resolve_segment_count_mismatch_is_no_match() {
        let mut r = ChannelRegistry::new();
        r.register(DummyChannel {
            name: "orders.{id}",
        });
        assert!(r.resolve("orders").is_none()); // too few segments
        assert!(r.resolve("orders.1.extra").is_none()); // too many segments
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let r = ChannelRegistry::new();
        assert!(r.resolve("nope").is_none());
    }
}
