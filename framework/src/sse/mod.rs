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
use std::borrow::Cow;

/// Strip CR / LF / NUL from a single-line SSE field value (`event:`, `id:`).
///
/// The SSE wire grammar uses CR/LF as field terminators with no escape
/// mechanism. A producer that lets `event` or `id` carry a CR or LF would
/// let a hostile input inject additional SSE fields — or a whole new event —
/// after a legitimate one. NUL is rejected for the same reason the WHATWG
/// parser drops it from `id` ("Last-Event-ID containing NULL is ignored"):
/// pre-stripping keeps producer and consumer in agreement.
///
/// Returns `(sanitized, had_bad_chars)` so the caller can emit a structured
/// `WARN` with the field NAME (the value is attacker-controlled and must
/// never appear in logs).
fn sanitize_field(value: &str) -> (Cow<'_, str>, bool) {
    if value.bytes().any(|b| matches!(b, b'\n' | b'\r' | b'\0')) {
        let cleaned: String = value
            .chars()
            .filter(|c| !matches!(*c, '\n' | '\r' | '\0'))
            .collect();
        (Cow::Owned(cleaned), true)
    } else {
        (Cow::Borrowed(value), false)
    }
}

/// Push one SSE field (`<name>: <value>\n`) into the output buffer,
/// stripping any CR/LF/NUL from the value and emitting a structured `WARN`
/// (field name only, never the bytes) when a strip actually fires.
fn write_field(out: &mut String, name: &str, value: &str) {
    let (sanitized, had_bad) = sanitize_field(value);
    if had_bad {
        tracing::warn!(
            target: "suprnova::sse",
            field = name,
            "stripped CR/LF/NUL from SSE field value; \
             producers MUST NOT embed line terminators in event names or ids",
        );
    }
    out.push_str(name);
    out.push_str(": ");
    out.push_str(&sanitized);
    out.push('\n');
}

/// Normalize `\r\n` and bare `\r` line endings in a multi-line `data`
/// payload to `\n` so that splitting on `\n` produces exactly the lines
/// the producer's string spelled out.
///
/// Per the [WHATWG parsing algorithm][whatwg-parse] an SSE *consumer*
/// treats `\r\n`, `\r`, and `\n` all as field terminators. If the producer
/// embeds a bare `\r` in `data` and we split only on `\n`, the receiver's
/// parser will still treat the `\r` as a terminator and synthesise a new
/// `data:` field at parse time — the same injection shape as `\n` in
/// `event`, just one layer down. Normalizing here keeps producer intent
/// and receiver behaviour aligned regardless of which terminator was used.
///
/// [whatwg-parse]: https://html.spec.whatwg.org/multipage/server-sent-events.html#parsing-an-event-stream
fn normalize_data_line_endings(data: &str) -> Cow<'_, str> {
    if data.bytes().any(|b| b == b'\r') {
        Cow::Owned(data.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(data)
    }
}

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
    /// data: <line>\n     (one per line in self.data, after \r/\r\n
    ///                     line-ending normalization)
    /// \n                 (terminator — required by the spec)
    /// ```
    ///
    /// **Sanitization contract.** `event` and `id` are single-line fields
    /// with no escape mechanism: any embedded CR / LF / NUL is stripped at
    /// serialize time and a structured `WARN`
    /// (`target: "suprnova::sse", field = "event"|"id"`) is emitted so the
    /// producer-side bug can be tracked down. The warn never logs the
    /// stripped value — it is attacker-controlled by construction.
    /// Producers that want to fail-fast instead of silently strip should
    /// use the `try_with_*` siblings (see [`Self::with_event`] /
    /// [`Self::with_id`]).
    ///
    /// `data` is allowed to be multi-line. Embedded `\r\n` and bare `\r`
    /// are normalized to `\n` before splitting so the wire reflects exactly
    /// the lines the producer's `data` string spelled out, regardless of
    /// which terminator the producer used — see
    /// [`normalize_data_line_endings`] for the why.
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
            write_field(&mut out, "event", name);
        }
        if let Some(id) = &self.id {
            write_field(&mut out, "id", id);
        }
        let normalized = normalize_data_line_endings(&self.data);
        for line in normalized.split('\n') {
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

    // ---- sanitization regression tests --------------------------------

    /// LF in `event:` is the canonical injection vector — without
    /// sanitization the receiver would parse the `data:` field after the
    /// LF as part of THIS event's frame and the rest as a follow-up.
    #[test]
    fn event_with_lf_is_stripped_in_wire_output() {
        let evt = SseEvent::data("payload").with_event("legit\ndata: injected");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "event: legitdata: injected\ndata: payload\n\n");
        assert!(
            !s.starts_with("event: legit\n"),
            "an LF in event MUST NOT terminate the field early — got: {s:?}",
        );
    }

    /// CR alone (no LF) is also a field terminator in the WHATWG parser,
    /// so it is just as dangerous as LF.
    #[test]
    fn event_with_cr_is_stripped_in_wire_output() {
        let evt = SseEvent::data("payload").with_event("legit\rdata: injected");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "event: legitdata: injected\ndata: payload\n\n");
    }

    /// CRLF (the most common "newline" sequence on the wire) collapses to
    /// nothing when both bytes are in the strip set.
    #[test]
    fn event_with_crlf_is_stripped_in_wire_output() {
        let evt = SseEvent::data("payload").with_event("legit\r\nspoofed");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "event: legitspoofed\ndata: payload\n\n");
    }

    /// `id:` is sanitized on the same contract — a CR/LF there would let
    /// a producer claim two different `Last-Event-ID` values for a single
    /// frame.
    #[test]
    fn id_with_lf_is_stripped_in_wire_output() {
        let evt = SseEvent::data("payload").with_id("42\nid: 999");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "id: 42id: 999\ndata: payload\n\n");
    }

    /// NUL is rejected by the WHATWG parser anyway (it drops the
    /// last-event-id update when the value contains U+0000); stripping at
    /// emit-time keeps the producer's view and the receiver's view in sync
    /// for the rest of the field too.
    #[test]
    fn event_with_nul_is_stripped_in_wire_output() {
        let evt = SseEvent::data("payload").with_event("legit\0poison");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "event: legitpoison\ndata: payload\n\n");
    }

    /// Legitimate values pass through unchanged — sanity check that the
    /// strip path is gated on the actual presence of bad bytes.
    #[test]
    fn event_and_id_without_terminators_pass_through() {
        let evt = SseEvent::data("payload")
            .with_event("user.registered")
            .with_id("user-42");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "event: user.registered\nid: user-42\ndata: payload\n\n",);
    }

    /// `\r\n` in data must collapse to one line — without normalization a
    /// receiver would parse the CR as a terminator AND the LF as a
    /// terminator, producing a synthetic empty data line.
    #[test]
    fn data_with_crlf_is_normalized_to_single_line_split() {
        let evt = SseEvent::data("line1\r\nline2");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "data: line1\ndata: line2\n\n");
    }

    /// Bare CR in data must split as if it were `\n` — otherwise a producer
    /// embedding `\r` in `data` could inject `data:`/`event:`/`id:` fields
    /// at the WHATWG parser layer.
    #[test]
    fn data_with_bare_cr_splits_like_lf() {
        let evt = SseEvent::data("line1\rline2\rline3");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "data: line1\ndata: line2\ndata: line3\n\n");
    }

    /// Mixed `\r\n`, `\r`, and `\n` collapse to the same shape — exercises
    /// the full WHATWG line-terminator normalization contract.
    #[test]
    fn data_with_mixed_line_endings_collapses_uniformly() {
        let evt = SseEvent::data("a\r\nb\rc\nd");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "data: a\ndata: b\ndata: c\ndata: d\n\n");
    }

    /// `sanitize_field` returns Borrowed for the common-case clean input —
    /// pins the no-allocation fast path so future refactors can't quietly
    /// regress it.
    #[test]
    fn sanitize_field_avoids_allocation_when_input_is_clean() {
        let (out, had_bad) = sanitize_field("user.registered");
        assert!(matches!(out, Cow::Borrowed(_)));
        assert!(!had_bad);

        let (out, had_bad) = sanitize_field("legit\ninjected");
        assert!(matches!(out, Cow::Owned(_)));
        assert!(had_bad);
        assert_eq!(out.as_ref(), "legitinjected");
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
