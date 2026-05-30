//! Channel trait + ChannelRegistry — registration + lookup + the
//! authorize / member_info hooks.
//!
//! Note: `hyper::body::Incoming` cannot be constructed outside hyper
//! internals, so tests that would require a real `suprnova::Request`
//! are replaced with static-property tests.

use async_trait::async_trait;
use serde_json::{Value, json};
use suprnova::FrameworkError;
use suprnova::broadcasting::{
    Channel, ChannelParams, ChannelRegistry, PresenceChannel, PrivateChannel,
};

// ---------------------------------------------------------------------------
// Concrete channel impls used across tests
// ---------------------------------------------------------------------------

struct PublicNotifications;

#[async_trait]
impl Channel for PublicNotifications {
    fn name(&self) -> &'static str {
        "notifications"
    }
}

struct PrivateChat {
    #[allow(dead_code)]
    room_id: i64,
}

#[async_trait]
impl Channel for PrivateChat {
    fn name(&self) -> &'static str {
        "chat.{room_id}"
    }
    async fn authorize(
        &self,
        _req: &suprnova::http::Request,
        _params: &ChannelParams,
        _data: &Value,
    ) -> bool {
        true
    }
}
impl PrivateChannel for PrivateChat {}

struct PresenceLobby;

#[async_trait]
impl Channel for PresenceLobby {
    fn name(&self) -> &'static str {
        "presence.lobby"
    }
    fn presence_info(&self) -> Option<&dyn PresenceChannel> {
        Some(self)
    }
}

#[async_trait]
impl PresenceChannel for PresenceLobby {
    async fn member_info(
        &self,
        _req: &suprnova::http::Request,
        _params: &ChannelParams,
    ) -> Result<Value, FrameworkError> {
        Ok(json!({ "user_id": 42 }))
    }
}

// ---------------------------------------------------------------------------
// Registry tests
// ---------------------------------------------------------------------------

#[test]
fn registry_resolves_registered_channels() {
    let mut registry = ChannelRegistry::new();
    registry.register(PublicNotifications);
    registry.register(PrivateChat { room_id: 1 });
    registry.register(PresenceLobby);

    assert_eq!(registry.len(), 3);
    assert!(registry.resolve("notifications").is_some());
    assert!(registry.resolve("presence.lobby").is_some());
    assert!(registry.resolve("nonexistent").is_none());

    // A concrete subscription resolves against the `chat.{room_id}` pattern
    // and binds the captured segment.
    let (chan, params) = registry.resolve("chat.7").expect("pattern resolves");
    assert_eq!(chan.name(), "chat.{room_id}");
    assert_eq!(params.get("room_id"), Some("7"));
}

#[test]
fn empty_registry_returns_none() {
    let registry = ChannelRegistry::new();
    assert!(registry.is_empty());
    assert!(registry.resolve("anything").is_none());
}

#[test]
fn re_register_replaces_channel() {
    let mut registry = ChannelRegistry::new();
    registry.register(PublicNotifications);
    registry.register(PublicNotifications);
    assert_eq!(registry.len(), 1, "same name → single entry");
}

#[test]
fn registry_default_is_empty() {
    let registry = ChannelRegistry::default();
    assert!(registry.is_empty());
}

// ---------------------------------------------------------------------------
// Trait surface tests (no Request construction needed)
// ---------------------------------------------------------------------------

#[test]
fn public_channel_name_returns_static_str() {
    let chan = PublicNotifications;
    assert_eq!(chan.name(), "notifications");
}

#[test]
fn private_channel_name_returns_static_str() {
    let chan = PrivateChat { room_id: 99 };
    assert_eq!(chan.name(), "chat.{room_id}");
}

#[test]
fn presence_channel_name_returns_static_str() {
    let chan = PresenceLobby;
    assert_eq!(chan.name(), "presence.lobby");
}
