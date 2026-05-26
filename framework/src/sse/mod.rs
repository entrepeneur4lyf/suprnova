//! Server-Sent Events delivery primitive.
//!
//! SSE is the minimal one-way push channel from server to browser:
//! the browser opens `EventSource(url)`, the server keeps a
//! `text/event-stream` response open, and pushes framed events as
//! they happen. No WebSocket handshake, no permessage-deflate, no
//! framing libs — just `data:`, `event:`, `id:` lines terminated
//! by a blank line, per the W3C [HTML Living Standard][whatwg] /
//! [WHATWG `EventSource`][es-spec].
//!
//! In Suprnova, SSE plugs into the streaming-body path added in
//! Task 16. Build an `mpsc` or `broadcast` channel of `SseEvent`s,
//! adapt it to a `Stream`, hand it to `HttpResponse::sse`, and the
//! framework boxes it into the response body. The connection stays
//! open until the producing stream ends or the client disconnects.
//!
//! # Example
//!
//! ```ignore
//! use suprnova::{sse::SseEvent, HttpResponse, Request, Response};
//! use tokio::sync::mpsc;
//! use tokio_stream::wrappers::ReceiverStream;
//! use futures::StreamExt;
//!
//! pub async fn stream_ticks(_req: Request) -> Response {
//!     let (tx, rx) = mpsc::channel::<SseEvent>(16);
//!     tokio::spawn(async move {
//!         for i in 0..10 {
//!             let evt = SseEvent::data(format!("tick {i}"))
//!                 .with_event("tick")
//!                 .with_id(i.to_string());
//!             if tx.send(evt).await.is_err() {
//!                 break; // client disconnected
//!             }
//!             tokio::time::sleep(std::time::Duration::from_secs(1)).await;
//!         }
//!     });
//!     Ok(HttpResponse::sse(ReceiverStream::new(rx)))
//! }
//! ```
//!
//! [whatwg]: https://html.spec.whatwg.org/multipage/server-sent-events.html
//! [es-spec]: https://html.spec.whatwg.org/multipage/server-sent-events.html#parsing-an-event-stream

use bytes::Bytes;

/// A single server-sent event frame.
///
/// Each field maps directly to a line in the wire encoding:
/// - `event` → `event: <name>\n` (omitted when `None`)
/// - `id`    → `id: <id>\n` (omitted when `None`)
/// - `data`  → one `data: <line>\n` per `\n`-separated line of the
///   payload, then a terminating blank line
///
/// Per the W3C spec a multi-line `data` value MUST emit one
/// `data:` field per line; consumers (browsers) re-join lines with
/// `\n` on parse. Embedding `\r` is undefined and tolerated by
/// browsers but we keep our wire output strictly LF-only.
#[derive(Debug, Clone)]
pub struct SseEvent {
    /// Optional event name; consumed by the client's
    /// `addEventListener(name, ...)`. When `None`, the browser
    /// dispatches to the default `"message"` listener.
    pub event: Option<String>,
    /// Optional last-event-id. When the client reconnects after a
    /// drop, it sends the last id back via the
    /// `Last-Event-ID` header so the producer can resume.
    pub id: Option<String>,
    /// Event payload. May contain newlines; each line is emitted
    /// as a separate `data:` field per the SSE spec.
    pub data: String,
}

impl SseEvent {
    /// Build an event with just a `data` payload. Equivalent to a
    /// browser `evt.data === <data>` with no event name or id.
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            event: None,
            id: None,
            data: data.into(),
        }
    }

    /// Tag the event with a name. Subscribers in the browser pick
    /// it up via `EventSource.addEventListener("<name>", ...)`.
    pub fn with_event(mut self, event: impl Into<String>) -> Self {
        self.event = Some(event.into());
        self
    }

    /// Tag the event with a `Last-Event-ID` value. Required for
    /// resume-from-drop semantics; otherwise the client cannot tell
    /// the producer where to pick up.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Build an event whose payload is JSON-serialized `data` and
    /// whose name is `event`. The most common shape on the wire:
    /// browsers parse it with `JSON.parse(evt.data)`.
    pub fn json<T>(event: &str, data: &T) -> Result<Self, serde_json::Error>
    where
        T: serde::Serialize,
    {
        let payload = serde_json::to_string(data)?;
        Ok(Self {
            event: Some(event.to_string()),
            id: None,
            data: payload,
        })
    }

    /// Serialize to the SSE wire format:
    ///
    /// ```text
    /// event: <event>\n   (only if Some)
    /// id: <id>\n         (only if Some)
    /// data: <line>\n     (one per line in self.data)
    /// \n                 (terminator — required by the spec)
    /// ```
    ///
    /// The trailing blank line is what tells the browser "the
    /// event is complete"; without it the browser buffers
    /// indefinitely.
    pub fn to_wire(&self) -> Bytes {
        // Pre-size: each line gets `data: ` (6 bytes) + content + `\n`.
        // Header fields and terminator add a few dozen bytes. Slight
        // over-allocation is fine; we'd rather avoid reallocations
        // for typical event sizes.
        let mut out = String::with_capacity(self.data.len() + 32);
        if let Some(name) = &self.event {
            out.push_str("event: ");
            out.push_str(name);
            out.push('\n');
        }
        if let Some(id) = &self.id {
            out.push_str("id: ");
            out.push_str(id);
            out.push('\n');
        }
        for line in self.data.split('\n') {
            out.push_str("data: ");
            out.push_str(line);
            out.push('\n');
        }
        // Required event terminator. Without this blank line the
        // browser will buffer the frame forever waiting for the
        // event to "end".
        out.push('\n');
        Bytes::from(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_only_frame_has_double_newline_terminator() {
        let evt = SseEvent::data("hello world");
        assert_eq!(&evt.to_wire()[..], b"data: hello world\n\n");
    }

    #[test]
    fn with_event_and_id_appear_before_data() {
        let evt = SseEvent::data("payload").with_event("ping").with_id("42");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "event: ping\nid: 42\ndata: payload\n\n");
    }

    #[test]
    fn multiline_data_emits_one_data_field_per_line() {
        // Per W3C: each LF in the payload is one `data:` field.
        // Browsers concat with `\n` on the receiving side, round-tripping
        // the original multi-line payload.
        let evt = SseEvent::data("line1\nline2\nline3");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "data: line1\ndata: line2\ndata: line3\n\n");
    }

    #[test]
    fn empty_data_still_emits_one_data_field_and_terminator() {
        // `"".split('\n')` yields one empty string; we still want one
        // `data:` line so the browser dispatches a `message` event with
        // empty `evt.data`.
        let evt = SseEvent::data("");
        assert_eq!(&evt.to_wire()[..], b"data: \n\n");
    }

    #[test]
    fn json_helper_serializes_payload_and_sets_event_name() {
        #[derive(serde::Serialize)]
        struct Payload<'a> {
            user_id: i64,
            email: &'a str,
        }
        let evt = SseEvent::json(
            "user.registered",
            &Payload {
                user_id: 42,
                email: "demo@suprnova.app",
            },
        )
        .unwrap();
        assert_eq!(evt.event.as_deref(), Some("user.registered"));
        // Single-line JSON serialization, so one data: line.
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert!(s.starts_with("event: user.registered\n"));
        assert!(s.contains("data: {\"user_id\":42,\"email\":\"demo@suprnova.app\"}\n"));
        assert!(s.ends_with("\n\n"));
    }
}
