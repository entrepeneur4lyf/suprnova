//! SSE dogfood controller — `/events/stream`.
//!
//! Subscribes to the in-process `UserRegistered` broadcast channel
//! and streams each dispatched event to the client as a
//! `text/event-stream` frame. Demonstrates how the framework's
//! event surface (Phase 1) composes with the streaming response
//! primitive: a controller is a stream adapter over the broadcast
//! receiver, nothing more.
//!
//! In a real app this is the shape of every "live feed" feature —
//! activity timelines, notifications, chat. The controller stays
//! tiny because the framework owns connection management, headers,
//! and framing.

use crate::bootstrap::user_registered_sender;
use crate::events::UserRegistered;
use futures::StreamExt;
use suprnova::{sse::SseEvent, HttpResponse, Request, Response};
use tokio_stream::wrappers::BroadcastStream;

/// GET `/events/stream` — opens an SSE connection that emits one
/// frame per `UserRegistered` event for as long as the client stays
/// connected.
///
/// Frame shape per the SSE spec:
/// ```text
/// event: user.registered
/// data: {"user_id":42,"email":"alice@example.com"}
///
/// ```
///
/// When the broadcast channel lags (slow consumer + bounded buffer),
/// the producing stream emits a `BroadcastStreamRecvError::Lagged(n)`
/// which we surface as a synthetic `lagged` event so the client can
/// react (e.g. trigger a full re-fetch). The connection stays open.
pub async fn stream(_req: Request) -> Response {
    let sender = user_registered_sender();
    let rx = sender.subscribe();

    // BroadcastStream<T: Clone> implements Stream<Item =
    // Result<T, BroadcastStreamRecvError>>. Map each item into an
    // SseEvent — successes serialize to JSON, lags become a typed
    // "lagged" frame so the client knows it missed events.
    let stream = BroadcastStream::new(rx).map(|result| match result {
        Ok(event) => SseEvent::json("user.registered", &SerializedUser::from(&event))
            .unwrap_or_else(|_| {
                // serde_json::to_string on a small struct of i64+String
                // shouldn't fail in practice. If it ever does, fall
                // back to a plain text frame so the connection
                // keeps moving.
                SseEvent::data(format!("user_id={}", event.user_id))
                    .with_event("user.registered")
            }),
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
            // Surface lag to the client. They can re-fetch state and
            // resume the stream from where they are now.
            SseEvent::data(n.to_string()).with_event("lagged")
        }
    });

    Ok(HttpResponse::sse(stream))
}

/// Wire representation of `UserRegistered`. Kept separate from the
/// in-process event type so the JSON shape is a deliberate part of
/// the controller's contract — not a leak of internal struct
/// layout. Today they match field-for-field; if `UserRegistered`
/// later grows fields a listener needs but a browser must not see
/// (e.g. raw IPs), they live on the event and not in this struct.
#[derive(serde::Serialize)]
struct SerializedUser {
    user_id: i64,
    email: String,
}

impl From<&UserRegistered> for SerializedUser {
    fn from(event: &UserRegistered) -> Self {
        Self {
            user_id: event.user_id,
            email: event.email.clone(),
        }
    }
}
