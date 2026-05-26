//! SSE dogfood controller — `/events/stream`.
//!
//! Subscribes to the framework's `BroadcastHub` on the
//! `"user_registered"` channel and streams each published envelope
//! as a `text/event-stream` frame. The hub primitive replaced the
//! bespoke `tokio::sync::broadcast` sender that lived in
//! `bootstrap.rs`; both SSE and WS subscribers now share the same
//! channel through the hub.
//!
//! In a real app this is the shape of every "live feed" feature —
//! activity timelines, notifications, chat. The controller stays
//! tiny because the framework owns connection management, headers,
//! and framing.

use futures::StreamExt;
use std::sync::Arc;
use suprnova::broadcasting::BroadcastHub;
use suprnova::container::App;
use suprnova::{HttpResponse, Request, Response, sse::SseEvent};
use tokio_stream::wrappers::BroadcastStream;

/// GET `/events/stream` — opens an SSE connection that emits one
/// frame per `UserRegistered` event for as long as the client stays
/// connected.
///
/// Frame shape per the SSE spec:
/// ```text
/// event: user.registered
/// data: {"channel":"user_registered","event":"UserRegistered","data":{"user_id":42,"email":"alice@example.com"}}
///
/// ```
///
/// When the broadcast channel lags (slow consumer + bounded buffer),
/// the producing stream emits a `BroadcastStreamRecvError::Lagged(n)`
/// which we surface as a synthetic `lagged` event so the client can
/// react (e.g. trigger a full re-fetch). The connection stays open.
pub async fn stream(_req: Request) -> Response {
    let hub: Arc<dyn BroadcastHub> = App::make::<dyn BroadcastHub>()
        .expect("BroadcastHub not bootstrapped — call bootstrap::register() first");
    let rx = hub.subscribe("user_registered");

    // BroadcastStream<BroadcastEnvelope> implements
    // Stream<Item = Result<BroadcastEnvelope, BroadcastStreamRecvError>>.
    // Map each item into an SseEvent — successes serialize the envelope
    // data as JSON, lags become a typed "lagged" frame.
    let stream = BroadcastStream::new(rx).map(|result| match result {
        Ok(envelope) => SseEvent::json("user.registered", &envelope.data).unwrap_or_else(|_| {
            // serde_json::to_string on a Value shouldn't fail in practice.
            // Fall back to a plain text frame so the connection keeps moving.
            SseEvent::data(envelope.data.to_string()).with_event("user.registered")
        }),
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
            // Surface lag to the client. They can re-fetch state and
            // resume the stream from where they are now.
            SseEvent::data(n.to_string()).with_event("lagged")
        }
    });

    Ok(HttpResponse::sse(stream))
}
