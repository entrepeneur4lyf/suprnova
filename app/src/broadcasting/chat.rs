//! Private chat channel — gates subscription on a token in the
//! subscribe data payload.
//!
//! Real authentication (session-cookie-on-WS + `Auth::user()`)
//! will land when the auth stack covers WebSocket upgrades. For now,
//! the gate accepts any token that starts with `"chat_"` as a
//! placeholder, which is sufficient to verify the authorize hook is
//! actually invoked by the handler.

use async_trait::async_trait;
use suprnova::broadcasting::{Channel, ChannelParams, PrivateChannel};
use suprnova::http::Request;
use suprnova::serde_json::Value;

/// Private channel for chat rooms. Clients must pass
/// `{"type":"subscribe","channel":"chat.lobby","data":{"token":"chat_..."}}`.
///
/// Marking a channel as `PrivateChannel` signals to tooling and
/// future middleware that this channel requires authorization;
/// the actual gate lives in the `authorize` override.
pub struct ChatChannel;

#[async_trait]
impl Channel for ChatChannel {
    fn name(&self) -> &'static str {
        "chat.lobby"
    }

    /// Accept subscribers whose `data` carries a `"token"` value
    /// starting with `"chat_"`. Replace with real session/JWT
    /// validation when the auth stack covers WebSocket upgrades.
    async fn authorize(&self, _req: &Request, _params: &ChannelParams, data: &Value) -> bool {
        data["token"]
            .as_str()
            .map(|t| t.starts_with("chat_"))
            .unwrap_or(false)
    }

    /// Allow standard chat events from authenticated subscribers.
    /// The subscribe gate (`authorize`) already validated the token,
    /// so anyone past that point is permitted to send chat events.
    async fn authorize_publish(
        &self,
        _req: &Request,
        _params: &ChannelParams,
        event: &str,
        _data: &Value,
    ) -> bool {
        matches!(event, "MessagePosted" | "Typing")
    }
}

impl PrivateChannel for ChatChannel {}
