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

use crate::error::FrameworkError;
use bytes::Bytes;
use std::borrow::Cow;
use std::time::Duration;

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

/// One emittable piece of an SSE stream.
///
/// Two kinds of value share this type:
///
/// 1. **Frame** — a normal event with optional `event`, `id`, `retry` and
///    a multi-line `data` payload. Constructed via [`SseEvent::data`] /
///    [`SseEvent::json`] / [`SseEvent::error`] and decorated with
///    `with_event` / `with_id` / `with_retry`.
/// 2. **Comment** — a wire-only keep-alive (`: <text>\n\n`). The browser
///    ignores comments, but the bytes traverse intermediaries which is
///    what keeps idle connections from being closed by a proxy timeout.
///    Constructed via [`SseEvent::comment`] / [`SseEvent::keep_alive`].
///
/// The kind discriminator is internal — [`Self::is_comment`] / accessor
/// methods give safe read access without locking us into the current
/// representation. Once you've built an `SseEvent`, mutators
/// (`with_event` / `with_id` / `with_retry`) only apply to the `Frame`
/// kind; on a `Comment` they are silent no-ops since "comment with an
/// event name" is not a thing the wire format expresses.
#[derive(Debug, Clone)]
pub struct SseEvent {
    pub(crate) kind: SseEventKind,
}

/// Internal kind discriminator. Kept `pub(crate)` so external code can't
/// accidentally depend on the representation — accessor methods on
/// [`SseEvent`] expose only what's safe to commit to.
#[derive(Debug, Clone)]
pub(crate) enum SseEventKind {
    Frame {
        event: Option<String>,
        id: Option<String>,
        retry: Option<Duration>,
        data: String,
    },
    Comment(String),
}

impl SseEvent {
    // ---- constructors ---------------------------------------------------

    /// Build a `Frame` event with just a `data` payload. Equivalent to a
    /// browser `evt.data === <data>` with no event name or id.
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            kind: SseEventKind::Frame {
                event: None,
                id: None,
                retry: None,
                data: data.into(),
            },
        }
    }

    /// Build a `Frame` event whose payload is JSON-serialized `data` and
    /// whose name is `event`. The most common shape on the wire:
    /// browsers parse it with `JSON.parse(evt.data)`.
    pub fn json<T>(event: &str, data: &T) -> Result<Self, serde_json::Error>
    where
        T: serde::Serialize,
    {
        let payload = serde_json::to_string(data)?;
        Ok(Self {
            kind: SseEventKind::Frame {
                event: Some(event.to_string()),
                id: None,
                retry: None,
                data: payload,
            },
        })
    }

    /// Build a comment-only event (`: <text>\n\n`).
    ///
    /// Comments are ignored by the WHATWG `EventSource` parser, so the
    /// browser does NOT dispatch a `message` event for them. Use them as
    /// keep-alive heartbeats on idle streams — the bytes traverse proxies
    /// and load balancers that would otherwise drop a silent connection.
    ///
    /// Multi-line comment text is split on the same `\r\n` / `\r` / `\n`
    /// rules as `data`, with each line written as its own `: <line>\n`.
    /// NUL bytes in comments are not legal SSE content and would corrupt
    /// the stream, so they are stripped (mirroring the `event` / `id`
    /// contract).
    pub fn comment(text: impl Into<String>) -> Self {
        Self {
            kind: SseEventKind::Comment(text.into()),
        }
    }

    /// Shorthand for an empty keep-alive comment (`:\n\n`).
    ///
    /// Minimum-bytes form: enough to flush proxy/load-balancer write
    /// buffers without sending any payload the receiver would have to
    /// inspect. Schedule one of these every 15–30 seconds on idle streams
    /// to survive proxy idle timeouts (nginx defaults to 60s, ALBs default
    /// to 60s, Cloudflare defaults to 100s).
    pub fn keep_alive() -> Self {
        Self::comment("")
    }

    /// Build a `Frame` event with the conventional `error` name and the
    /// supplied message as `data`. Producers map application-level errors
    /// to this shape so subscribers can listen for `error` separately
    /// from the normal message stream:
    ///
    /// ```js
    /// es.addEventListener("error", (evt) => console.error(evt.data));
    /// ```
    ///
    /// Note that this is a domain-level error event — distinct from the
    /// connection-level `error` the browser fires on EventSource transport
    /// failures (those have no `data`).
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            kind: SseEventKind::Frame {
                event: Some("error".to_string()),
                id: None,
                retry: None,
                data: message.into(),
            },
        }
    }

    // ---- infallible builders (silent strip + WARN at to_wire) ----------

    /// Tag a `Frame` event with a name. Subscribers in the browser pick
    /// it up via `EventSource.addEventListener("<name>", ...)`.
    ///
    /// On a `Comment` event this is a silent no-op — comments do not
    /// carry an event name and the wire format has no way to express one.
    /// Producers that want to fail-fast on bad input (CR / LF / NUL)
    /// should use [`Self::try_with_event`] instead.
    pub fn with_event(mut self, event: impl Into<String>) -> Self {
        if let SseEventKind::Frame { event: e, .. } = &mut self.kind {
            *e = Some(event.into());
        }
        self
    }

    /// Tag a `Frame` event with a `Last-Event-ID` value. Required for
    /// resume-from-drop semantics; otherwise the client cannot tell
    /// the producer where to pick up. See [`last_event_id`] for reading
    /// the reciprocal header on the resume request.
    ///
    /// No-op on `Comment`. See [`Self::try_with_id`] for the fallible
    /// sibling.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        if let SseEventKind::Frame { id: i, .. } = &mut self.kind {
            *i = Some(id.into());
        }
        self
    }

    /// Tag a `Frame` event with a `retry:` value — the browser's
    /// reconnect delay after a transport drop. Spec-defined as
    /// non-negative integer milliseconds.
    ///
    /// `Duration::ZERO` is valid per the spec ("reconnect immediately").
    /// No sanitization required: `Duration` cannot carry control
    /// characters, the integer-ms serialization is the only thing that
    /// lands on the wire. No-op on `Comment`.
    pub fn with_retry(mut self, retry: Duration) -> Self {
        if let SseEventKind::Frame { retry: r, .. } = &mut self.kind {
            *r = Some(retry);
        }
        self
    }

    // ---- fallible siblings (fail-fast on CR/LF/NUL) --------------------

    /// Fallible variant of [`Self::with_event`]: returns
    /// `Err(FrameworkError::validation(...))` if the event name contains
    /// CR, LF, or NUL instead of stripping silently.
    ///
    /// Reach for this when the event name flows from user input and you
    /// want the producer-side bug to surface as a request-level error
    /// rather than a structured `WARN`. No-op on `Comment` (returns
    /// `Ok(self)` unchanged).
    pub fn try_with_event(mut self, event: impl Into<String>) -> Result<Self, FrameworkError> {
        let value = event.into();
        validate_no_control_chars("event", &value)?;
        if let SseEventKind::Frame { event: e, .. } = &mut self.kind {
            *e = Some(value);
        }
        Ok(self)
    }

    /// Fallible variant of [`Self::with_id`]. See [`Self::try_with_event`]
    /// for the contract.
    pub fn try_with_id(mut self, id: impl Into<String>) -> Result<Self, FrameworkError> {
        let value = id.into();
        validate_no_control_chars("id", &value)?;
        if let SseEventKind::Frame { id: i, .. } = &mut self.kind {
            *i = Some(value);
        }
        Ok(self)
    }

    // ---- accessors ------------------------------------------------------

    /// The frame's event name, if any. Returns `None` for `Comment` and
    /// for frames without `with_event(...)` set.
    pub fn event(&self) -> Option<&str> {
        match &self.kind {
            SseEventKind::Frame { event, .. } => event.as_deref(),
            SseEventKind::Comment(_) => None,
        }
    }

    /// The frame's `Last-Event-ID`, if any. Returns `None` for `Comment`
    /// and for frames without `with_id(...)` set.
    pub fn id(&self) -> Option<&str> {
        match &self.kind {
            SseEventKind::Frame { id, .. } => id.as_deref(),
            SseEventKind::Comment(_) => None,
        }
    }

    /// The frame's `retry:` value, if any. Returns `None` for `Comment`
    /// and for frames without `with_retry(...)` set.
    pub fn retry(&self) -> Option<Duration> {
        match &self.kind {
            SseEventKind::Frame { retry, .. } => *retry,
            SseEventKind::Comment(_) => None,
        }
    }

    /// The frame's payload. Returns `""` for `Comment` (comments have no
    /// `data:` field). For frames, returns the raw producer-supplied
    /// string — line-ending normalization happens at [`Self::to_wire`]
    /// time, not here.
    ///
    /// Named `payload` instead of `data` so the accessor doesn't collide
    /// with the [`Self::data`] constructor (Rust forbids identically-named
    /// items in the same impl block regardless of `self`-arity).
    pub fn payload(&self) -> &str {
        match &self.kind {
            SseEventKind::Frame { data, .. } => data,
            SseEventKind::Comment(_) => "",
        }
    }

    /// `true` iff this is a comment-only event built via
    /// [`Self::comment`] or [`Self::keep_alive`].
    pub fn is_comment(&self) -> bool {
        matches!(self.kind, SseEventKind::Comment(_))
    }

    /// The comment text, if this is a comment-only event. Returns `None`
    /// for frames.
    pub fn comment_text(&self) -> Option<&str> {
        match &self.kind {
            SseEventKind::Comment(text) => Some(text),
            SseEventKind::Frame { .. } => None,
        }
    }

    // ---- wire encoding -------------------------------------------------

    /// Serialize to the SSE wire format.
    ///
    /// For a `Frame`:
    /// ```text
    /// event: <event>\n   (only if Some)
    /// id: <id>\n         (only if Some)
    /// retry: <ms>\n      (only if Some)
    /// data: <line>\n     (one per line in self.data, after \r/\r\n
    ///                     line-ending normalization)
    /// \n                 (terminator — required by the spec)
    /// ```
    ///
    /// For a `Comment`:
    /// ```text
    /// : <line>\n         (one per line in the comment text, after
    ///                     \r/\r\n normalization; empty line if the
    ///                     comment text itself is empty)
    /// \n                 (flush boundary — without it some proxies
    ///                     buffer the comment forever)
    /// ```
    ///
    /// **Sanitization contract.** `event` and `id` are single-line fields
    /// with no escape mechanism: any embedded CR / LF / NUL is stripped at
    /// serialize time and a structured `WARN`
    /// (`target: "suprnova::sse", field = "event"|"id"`) is emitted so the
    /// producer-side bug can be tracked down. The warn never logs the
    /// stripped value — it is attacker-controlled by construction.
    /// Producers that want to fail-fast instead of silently strip should
    /// use the `try_with_*` siblings (see [`Self::try_with_event`] /
    /// [`Self::try_with_id`]).
    ///
    /// `data` and comment text are allowed to be multi-line. Embedded
    /// `\r\n` and bare `\r` are normalized to `\n` before splitting so
    /// the wire reflects exactly the lines the producer's string spelled
    /// out, regardless of which terminator the producer used — see
    /// [`normalize_data_line_endings`] for the why. NUL bytes in comment
    /// text are stripped on the same grounds as `event` / `id`.
    pub fn to_wire(&self) -> Bytes {
        match &self.kind {
            SseEventKind::Frame {
                event,
                id,
                retry,
                data,
            } => {
                let mut out = String::with_capacity(data.len() + 32);
                if let Some(name) = event {
                    write_field(&mut out, "event", name);
                }
                if let Some(id) = id {
                    write_field(&mut out, "id", id);
                }
                if let Some(retry) = retry {
                    // Saturate at u64::MAX to avoid wrapping huge Durations
                    // into nonsense small values; in practice nobody picks
                    // a retry over a few minutes, but defending against
                    // overflow is free.
                    let ms = retry.as_millis().min(u64::MAX as u128) as u64;
                    out.push_str("retry: ");
                    out.push_str(&ms.to_string());
                    out.push('\n');
                }
                let normalized = normalize_data_line_endings(data);
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
            SseEventKind::Comment(text) => {
                let mut out = String::with_capacity(text.len() + 4);
                // Comments share the line-ending and NUL contracts with
                // `event` / `id`: WHATWG's parser treats `\r`, `\n`, and
                // `\r\n` all as field terminators, so a multi-line comment
                // string must emit one `: <line>\n` per producer line, and
                // a NUL byte would corrupt the wire.
                let normalized = normalize_data_line_endings(text);
                let (sanitized, had_nul) = strip_nul(&normalized);
                if had_nul {
                    tracing::warn!(
                        target: "suprnova::sse",
                        field = "comment",
                        "stripped NUL from SSE comment text; \
                         producers MUST NOT embed NUL bytes in event streams",
                    );
                }
                for line in sanitized.split('\n') {
                    // Minimum-bytes form: `:\n` for empty line,
                    // `: <text>\n` otherwise. The leading-space convention
                    // is just readability; skipping it on empty lines is
                    // how `keep_alive()` produces the canonical `:\n\n`
                    // heartbeat shape.
                    if line.is_empty() {
                        out.push(':');
                    } else {
                        out.push_str(": ");
                        out.push_str(line);
                    }
                    out.push('\n');
                }
                // Trailing blank-line acts as a flush boundary so the
                // comment reaches the receiver immediately rather than
                // sitting in a proxy's write buffer.
                out.push('\n');
                Bytes::from(out)
            }
        }
    }
}

/// Validate that `value` carries no SSE-illegal control character. Returns
/// `Err(FrameworkError::validation(...))` describing which field rejected
/// the input. The error MESSAGE includes the field name; it does NOT
/// include the value bytes, which are attacker-controlled.
fn validate_no_control_chars(field: &str, value: &str) -> Result<(), FrameworkError> {
    if value.bytes().any(|b| matches!(b, b'\n' | b'\r' | b'\0')) {
        Err(FrameworkError::validation(
            field.to_string(),
            format!(
                "SSE `{field}` MUST NOT contain CR / LF / NUL — \
                 line terminators have no escape in the wire format"
            ),
        ))
    } else {
        Ok(())
    }
}

/// Strip NUL bytes from a string. Used for comment text (which may
/// legitimately span multiple lines so we DON'T strip `\r`/`\n`, but NUL
/// is still illegal). Returns `(sanitized, had_nul)` so callers can emit
/// a structured `WARN` on the strip.
fn strip_nul(value: &str) -> (Cow<'_, str>, bool) {
    if value.bytes().any(|b| b == b'\0') {
        let cleaned: String = value.chars().filter(|c| *c != '\0').collect();
        (Cow::Owned(cleaned), true)
    } else {
        (Cow::Borrowed(value), false)
    }
}

/// Pure helper: validate a raw `Last-Event-ID` header value per the
/// WHATWG `EventSource` contract. Pulled out so the validation logic is
/// unit-testable without building a full `Request` (which requires a
/// live `hyper::body::Incoming` body in the current API).
///
/// Returns `None` when the value is absent OR contains a NUL byte —
/// per the spec a NUL invalidates the id, and pre-filtering keeps
/// producer code from having to defend against it on every read.
pub fn last_event_id_from_value(value: Option<&str>) -> Option<String> {
    value.filter(|v| !v.contains('\0')).map(String::from)
}

/// Read the `Last-Event-ID` header from a request, if present and valid
/// per the WHATWG `EventSource` contract.
///
/// The browser sends this header when reconnecting after an
/// `EventSource` drop — its value is the most recent `id:` field the
/// browser saw on the previous connection. Producers map it back to
/// stream state (cursor / sequence number / offset) so the receiver
/// resumes from where it dropped instead of re-receiving the entire
/// history.
///
/// Returns `None` when the header is absent OR contains a NUL byte —
/// per the spec a NUL invalidates the id, and pre-filtering keeps
/// producer code from having to defend against it on every read.
///
/// The returned `String` is otherwise opaque user input: do not trust it
/// for SQL fragments, file paths, or anything else. Validate the shape
/// (e.g. parse as a `u64` cursor) at the point of use.
///
/// Internally delegates to [`last_event_id_from_value`] — that function
/// is the one targeted by unit tests since constructing a `Request` in
/// isolation requires a live `hyper::body::Incoming` body.
pub fn last_event_id(req: &crate::Request) -> Option<String> {
    last_event_id_from_value(req.header("last-event-id"))
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
        assert_eq!(evt.event(), Some("user.registered"));
        // Single-line JSON serialization, so one data: line.
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert!(s.starts_with("event: user.registered\n"));
        assert!(s.contains("data: {\"user_id\":42,\"email\":\"demo@suprnova.app\"}\n"));
        assert!(s.ends_with("\n\n"));
    }

    // ---- retry / comment / error / try_with_* / last_event_id ---------

    /// `with_retry` emits a `retry:` line with the duration in
    /// integer milliseconds. Spec contract: non-negative integer ms
    /// only; no fractional component, no units suffix.
    #[test]
    fn with_retry_emits_integer_ms_field() {
        let evt = SseEvent::data("payload").with_retry(Duration::from_millis(2500));
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "retry: 2500\ndata: payload\n\n");
        assert_eq!(evt.retry(), Some(Duration::from_millis(2500)));
    }

    /// Zero retry is valid per the spec — "reconnect immediately" —
    /// and must NOT be coerced to anything else.
    #[test]
    fn with_retry_zero_is_valid_and_emitted_verbatim() {
        let evt = SseEvent::data("payload").with_retry(Duration::ZERO);
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "retry: 0\ndata: payload\n\n");
    }

    /// `retry` comes after `event`/`id` and before `data` — pinned because
    /// the WHATWG parsing algorithm reads fields in order and producers
    /// often eyeball the wire for debugging.
    #[test]
    fn retry_field_appears_between_id_and_data() {
        let evt = SseEvent::data("payload")
            .with_event("tick")
            .with_id("42")
            .with_retry(Duration::from_secs(5));
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "event: tick\nid: 42\nretry: 5000\ndata: payload\n\n");
    }

    /// `keep_alive()` produces a minimal comment frame — `:\n\n` —
    /// without emitting any `data:` line. Empty `data:` would dispatch a
    /// spurious empty `message` event to the client every heartbeat, so
    /// the comment kind must NOT share the empty-data fallthrough that
    /// `data("")` uses.
    #[test]
    fn keep_alive_emits_comment_only_no_data_field() {
        let evt = SseEvent::keep_alive();
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, ":\n\n");
        assert!(evt.is_comment());
        assert_eq!(evt.payload(), "");
        assert!(
            !s.contains("data:"),
            "a keep-alive MUST NOT emit a data: line — it would dispatch \
             an empty message event to every subscriber",
        );
    }

    /// `comment("ping")` emits `: ping\n\n`. The body is informational
    /// only; the wire-level keep-alive purpose is satisfied by the
    /// flush-boundary `\n\n`.
    #[test]
    fn comment_with_text_emits_prefixed_comment_line() {
        let evt = SseEvent::comment("ping");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, ": ping\n\n");
        assert_eq!(evt.comment_text(), Some("ping"));
    }

    /// Multi-line comments split on the same `\r\n`/`\r`/`\n` rules as
    /// `data` so the wire stays well-formed even when producers feed
    /// platform-mixed line endings.
    #[test]
    fn multiline_comment_splits_lines_with_colon_prefix() {
        let evt = SseEvent::comment("line1\r\nline2\rline3");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, ": line1\n: line2\n: line3\n\n");
    }

    /// NUL in comment text is illegal SSE content. Strip + WARN, same
    /// contract as `event` / `id`.
    #[test]
    fn comment_with_nul_is_stripped() {
        let evt = SseEvent::comment("ping\0poison");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, ": pingpoison\n\n");
    }

    /// `error("msg")` is the conventional `event: error` + payload shape
    /// — subscribers can `addEventListener("error", ...)` without colliding
    /// with the connection-level `error` the browser fires on transport
    /// failure (those carry no `data`).
    #[test]
    fn error_helper_emits_event_error_with_message_payload() {
        let evt = SseEvent::error("ratelimit exceeded");
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, "event: error\ndata: ratelimit exceeded\n\n");
        assert_eq!(evt.event(), Some("error"));
    }

    /// `try_with_event` rejects CR/LF/NUL with a validation error instead
    /// of silently stripping. Returns the FIELD name in the error so
    /// producers can surface a 400 without leaking the attacker bytes.
    #[test]
    fn try_with_event_rejects_lf_with_validation_error() {
        let result = SseEvent::data("payload").try_with_event("legit\ninjected");
        let err = result.expect_err("an LF in event must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("event"),
            "error message must name the failing field: {msg:?}",
        );
        assert!(
            !msg.contains("legit"),
            "error MUST NOT echo back the attacker-controlled value: {msg:?}",
        );
    }

    /// `try_with_event` accepts clean input and returns `Ok(Self)`.
    #[test]
    fn try_with_event_passes_clean_input_through() {
        let evt = SseEvent::data("payload")
            .try_with_event("user.registered")
            .expect("clean input must succeed");
        assert_eq!(evt.event(), Some("user.registered"));
    }

    /// `try_with_id` follows the same contract as `try_with_event`.
    #[test]
    fn try_with_id_rejects_cr_with_validation_error() {
        let result = SseEvent::data("payload").try_with_id("42\rspoof");
        let err = result.expect_err("a CR in id must be rejected");
        assert!(format!("{err}").contains("id"));
    }

    /// Builders on a `Comment` event are silent no-ops — `with_event` on
    /// a comment does NOT convert it to a frame, and the wire stays
    /// comment-shaped.
    #[test]
    fn with_event_on_comment_is_silent_noop() {
        let evt = SseEvent::keep_alive().with_event("would-be-event");
        assert!(evt.is_comment());
        assert_eq!(evt.event(), None);
        let s = std::str::from_utf8(&evt.to_wire()).unwrap().to_string();
        assert_eq!(s, ":\n\n");
    }

    /// `try_with_event` on a `Comment` returns `Ok(self)` unchanged for
    /// the same reason — it's a no-op, so by definition it cannot fail.
    #[test]
    fn try_with_event_on_comment_returns_ok_unchanged() {
        let evt = SseEvent::keep_alive()
            .try_with_event("anything")
            .expect("no-op on comment is infallible");
        assert!(evt.is_comment());
        assert_eq!(evt.event(), None);
    }

    /// `with_retry` on a `Comment` is also a no-op — `retry:` is only
    /// meaningful on a `Frame`.
    #[test]
    fn with_retry_on_comment_is_silent_noop() {
        let evt = SseEvent::keep_alive().with_retry(Duration::from_secs(5));
        assert!(evt.is_comment());
        assert_eq!(evt.retry(), None);
    }

    /// Clean header value passes through to `Some(<owned String>)`. We
    /// target the pure `last_event_id_from_value` helper since
    /// constructing a `Request` in isolation requires a live
    /// `hyper::body::Incoming` — the Request-bound `last_event_id` is a
    /// one-line wrapper over this helper, so this exercises the entire
    /// validation contract.
    #[test]
    fn last_event_id_from_value_passes_clean_input_through() {
        assert_eq!(
            last_event_id_from_value(Some("user-42")).as_deref(),
            Some("user-42"),
        );
    }

    /// Absent header → `None`.
    #[test]
    fn last_event_id_from_value_returns_none_on_absent_header() {
        assert!(last_event_id_from_value(None).is_none());
    }

    /// Per WHATWG, a `Last-Event-ID` value containing NUL is invalid and
    /// the parser drops the update. Our reader returns `None` rather than
    /// passing the corrupted bytes to producer code.
    #[test]
    fn last_event_id_from_value_returns_none_on_nul_byte() {
        assert!(last_event_id_from_value(Some("42\0poison")).is_none());
    }

    /// Empty header value is permitted by RFC 9110 but undefined as a
    /// last-event-id. We pass it through verbatim — producer code is
    /// expected to validate the shape (e.g. parse as cursor). The
    /// last-event-id contract is "what was the last id you saw"; an
    /// empty value means "I saw no id yet", which is semantically
    /// indistinguishable from absent. Producers usually treat both
    /// as "start from the beginning" so we don't special-case.
    #[test]
    fn last_event_id_from_value_passes_empty_string_through() {
        assert_eq!(last_event_id_from_value(Some("")).as_deref(), Some(""));
    }
}
