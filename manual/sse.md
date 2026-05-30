# Server-Sent Events

Server-Sent Events (SSE) is the minimum one-way push channel from server to
browser: the browser opens `EventSource(url)`, the server keeps a
`text/event-stream` response open, and pushes framed events as they happen.
No WebSocket handshake, no permessage-deflate, no framing libraries — just
`data:`, `event:`, `id:`, `retry:` lines terminated by a blank line, per the
[WHATWG `EventSource`](https://html.spec.whatwg.org/multipage/server-sent-events.html)
specification.

Suprnova's SSE primitive plugs into the streaming-body path: build a
`Stream<Item = SseEvent>`, hand it to `HttpResponse::sse(...)`, and the
framework owns connection management, framing, headers, and panic
isolation. The connection stays open until the producing stream ends or
the client disconnects.

## When to reach for SSE vs WebSockets

| Property | SSE | WebSockets |
|----------|-----|------------|
| Direction | Server → browser | Bidirectional |
| Transport | Plain HTTP/1.1 or HTTP/2 | Upgrade-only |
| Reconnect | Automatic, with `retry:` and `Last-Event-ID` | Manual |
| Proxies / CDNs | Works through anything that allows long HTTP responses | Often needs explicit Upgrade support |
| Browser API | `EventSource` (built in) | `WebSocket` (built in) |
| Binary frames | Text only (UTF-8) | Text or binary |
| Per-tab connection cap | 6 (HTTP/1.1) / unlimited (HTTP/2) | Unlimited |

Reach for SSE when you only need server-to-client push (activity feeds,
notifications, log tails, AI streaming). Reach for [WebSockets](websockets.md)
when you need bidirectional traffic or binary frames.

## Quickstart

```rust
use futures::StreamExt;
use suprnova::{HttpResponse, Request, Response, sse::SseEvent};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

pub async fn stream_ticks(_req: Request) -> Response {
    let (tx, rx) = mpsc::channel::<SseEvent>(16);
    tokio::spawn(async move {
        for i in 0..10 {
            let evt = SseEvent::data(format!("tick {i}"))
                .with_event("tick")
                .with_id(i.to_string());
            if tx.send(evt).await.is_err() {
                break; // client disconnected
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
    Ok(HttpResponse::sse(ReceiverStream::new(rx)))
}
```

Wire output for one tick:

```text
event: tick
id: 0
data: tick 0

```

The browser parses this and fires a `tick` event with `evt.data === "tick 0"`
and `evt.lastEventId === "0"`.

## The `SseEvent` API

`SseEvent` is the type you push onto the stream. It has two kinds:

* **Frame** — a normal event with optional `event` / `id` / `retry` and a
  multi-line `data` payload. Built via [`SseEvent::data`](#constructors),
  `SseEvent::json`, or `SseEvent::error`.
* **Comment** — a wire-only keep-alive (`:\n\n` or `: <text>\n\n`). Built
  via `SseEvent::comment(text)` or `SseEvent::keep_alive()`. The browser
  ignores comments by spec; the bytes traversing the connection are what
  keep idle proxies and load balancers from closing it.

### Constructors

| Constructor | Produces | Use |
|-------------|----------|-----|
| `SseEvent::data(text)` | Frame with only `data:` lines | The minimal event |
| `SseEvent::json(event, &payload)` | Frame with `event:` + JSON `data:` | The 95% case — `JSON.parse(evt.data)` on the client |
| `SseEvent::error(message)` | Frame with `event: error` | Domain-level error event, distinct from the connection-level `error` the browser fires on transport failure |
| `SseEvent::comment(text)` | Comment | Keep-alive with a marker the operator can spot in logs |
| `SseEvent::keep_alive()` | Empty comment (`:\n\n`) | Canonical minimum-bytes heartbeat |

### Builders

| Builder | Effect | On `Comment` |
|---------|--------|--------------|
| `.with_event(name)` | Sets `event:` field | Silent no-op |
| `.with_id(id)` | Sets `id:` field — required for resume semantics | Silent no-op |
| `.with_retry(Duration)` | Sets `retry:` field (ms); spec says `Duration::ZERO` means "reconnect immediately" | Silent no-op |
| `.try_with_event(name)` | Fallible variant — see [Security contract](#security-contract) | `Ok(self)` unchanged |
| `.try_with_id(id)` | Fallible variant of `with_id` | `Ok(self)` unchanged |

Builders on `Comment` are no-ops on purpose — the wire format has no way
to express "comment with an event name". A misuse stays silent rather
than converting the event to a frame and surprising the producer.

### Accessors

| Method | Returns |
|--------|---------|
| `.event()` | `Option<&str>` — the event name, if set |
| `.id()` | `Option<&str>` — the last-event-id, if set |
| `.retry()` | `Option<Duration>` — the reconnect delay, if set |
| `.payload()` | `&str` — the `data:` payload (or `""` for `Comment`) |
| `.is_comment()` | `bool` |
| `.comment_text()` | `Option<&str>` — the comment text, if this is a `Comment` |

### Wire encoding

`SseEvent::to_wire()` serializes the event to `Bytes` ready for the body
stream:

**Frame:**

```text
event: <event>\n   (only if Some)
id: <id>\n         (only if Some)
retry: <ms>\n      (only if Some)
data: <line>\n     (one per line in payload, after \r/\r\n normalization)
\n                 (terminator — required by the spec)
```

**Comment:**

```text
: <line>\n         (one per line in comment text; `:\n` for empty lines)
\n                 (flush boundary)
```

## Security contract

The SSE wire format uses CR / LF / NUL as field terminators with no
escape mechanism. A producer that lets user input reach `event:` or `id:`
without sanitizing would expose a field-injection vulnerability — a
value of `"legit\ndata: injected"` would produce two `data:` fields on
the wire, and `"legit\n\nevent: spoofed"` would terminate the current
event and start a new one.

Suprnova's `to_wire()` defends in two layers:

* **`event:` and `id:` field values** — every CR / LF / NUL is stripped
  at serialize time. A structured `WARN` fires for every strip:
  `target: "suprnova::sse"`, `field = "event"|"id"`. The warn never
  logs the value — those bytes are attacker-controlled by construction.
* **`data:` and comment text** — `\r\n` and bare `\r` are normalized to
  `\n` before splitting, so a producer embedding `\r` in a payload
  cannot cause the receiver's parser to synthesize a `data:` / `event:` /
  `id:` field at parse time. NUL is stripped from comment text with a
  matching `WARN`.

If you want to **fail fast** on bad input rather than silently strip,
reach for the `try_with_*` siblings:

```rust
use suprnova::{Response, sse::SseEvent};

let evt = SseEvent::data("hello")
    .try_with_event(&user_supplied_event)?     // returns Err on CR/LF/NUL
    .try_with_id(&user_supplied_id)?;
```

The returned `FrameworkError::validation(field, ...)` names the field;
it does NOT echo the value back, so a 400 surfaced to the client is
safe to log.

## Keep-alive and proxy idle timeouts

Long-lived SSE connections are silent by default. Most production
deployments sit behind a proxy / load balancer / CDN that closes idle
connections to free resources:

* nginx default: 60 seconds
* AWS ALB default: 60 seconds
* Cloudflare default: 100 seconds

A `keep_alive()` comment every 15–30 seconds keeps the connection alive
through all of those without dispatching a `message` event to the
browser. The minimum-bytes form (`:\n\n`) is enough to flush proxy write
buffers without sending any payload.

```rust
use std::time::Duration;
use futures::StreamExt;
use suprnova::sse::SseEvent;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

let (tx, rx) = mpsc::channel::<SseEvent>(16);

// Heartbeat task — independent of the event producer.
let hb_tx = tx.clone();
tokio::spawn(async move {
    let mut ticker = tokio::time::interval(Duration::from_secs(20));
    loop {
        ticker.tick().await;
        if hb_tx.send(SseEvent::keep_alive()).await.is_err() {
            break; // client gone
        }
    }
});

// Event producer ... sends frames into `tx` as they happen.
```

## Resume after drop (`Last-Event-ID`)

When the browser's `EventSource` drops the connection, it reconnects
automatically and sends the most recent `id:` it saw as the
`Last-Event-ID` header on the new request. Tag each event with
`.with_id(...)` and read the header on the resume request:

```rust
use futures::StreamExt;
use suprnova::{HttpResponse, Request, Response, sse::{self, SseEvent}};

pub async fn stream_from_resume(req: Request) -> Response {
    let resume_from: u64 = sse::last_event_id(&req)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // Build the producer stream from `resume_from + 1` onward. The closure
    // owns its own running counter so the mutation stays inside the stream.
    let stream = futures::stream::iter(events_since(resume_from))
        .scan(resume_from + 1, |next_id, payload| {
            let id = *next_id;
            *next_id += 1;
            futures::future::ready(Some((id, payload)))
        })
        .map(|(id, payload)| {
            SseEvent::json("activity", &payload)
                .expect("payload is a Serialize value")
                .with_id(id.to_string())
        });

    Ok(HttpResponse::sse(stream))
}
```

`sse::last_event_id(&Request) -> Option<String>` returns `None` when the
header is absent OR when the value contains a NUL byte (per the WHATWG
spec, NUL invalidates a last-event-id and the browser's parser would
drop it). The returned `String` is otherwise opaque user input — parse
it as your own cursor / sequence / offset before using it.

## Domain-level errors

`SseEvent::error("...")` produces the conventional `event: error\ndata: <msg>\n\n`
shape. Subscribers can listen for it separately from the connection-level
`error` the browser fires on transport failure:

```js
const es = new EventSource("/stream");

// Connection / transport errors (no `data`).
es.onerror = (evt) => console.warn("transport error", evt);

// Domain-level errors emitted by SseEvent::error(...).
es.addEventListener("error", (evt) => console.error("server-side:", evt.data));
```

When mapping a `Stream<Item = Result<T, E>>` to `Stream<Item = SseEvent>`,
the idiomatic pattern is `map(|r| match r { Ok(x) => SseEvent::json(...), Err(e) => SseEvent::error(...) })`
— the consumer-side error mapping stays in the producer's hands and the
framework never has to invent a default shape.

## Broadcasting one stream to many subscribers

Fan-out to many SSE subscribers is already covered by the
[broadcasting subsystem](broadcasting.md): subscribe to a
`BroadcastHub` channel and adapt the `broadcast::Receiver` into the
`SseEvent` stream with `tokio_stream::wrappers::BroadcastStream` +
`.map(...)`. Each connection gets its own receiver; the hub handles
slow-consumer policy (`Lagged(n)` errors when a subscriber falls behind)
and you decide how to surface that to the client.

The working dogfood example at `app/src/controllers/sse_example.rs`
implements this in ~25 lines:

```rust
use futures::StreamExt;
use std::sync::Arc;
use suprnova::broadcasting::BroadcastHub;
use suprnova::container::App;
use suprnova::{HttpResponse, Request, Response, sse::SseEvent};
use tokio_stream::wrappers::BroadcastStream;

pub async fn stream(_req: Request) -> Response {
    let hub: Arc<dyn BroadcastHub> = App::make::<dyn BroadcastHub>()
        .expect("BroadcastHub not bootstrapped");
    let rx = hub.subscribe("user_registered");

    let stream = BroadcastStream::new(rx).map(|result| match result {
        Ok(envelope) => SseEvent::json("user.registered", &envelope.data)
            .unwrap_or_else(|_| {
                SseEvent::data(envelope.data.to_string())
                    .with_event("user.registered")
            }),
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
            SseEvent::data(n.to_string()).with_event("lagged")
        }
    });

    Ok(HttpResponse::sse(stream))
}
```

The `lagged` event lets the client trigger a full refetch and resume —
the connection stays open through the lag.

## Production setup

### Response headers

`HttpResponse::sse(...)` sets the required headers for you:

| Header | Value | Why |
|--------|-------|-----|
| `Content-Type` | `text/event-stream` | Spec-defined; the browser's `EventSource` requires it |
| `Cache-Control` | `no-cache` | Stops intermediaries from caching the stream |
| `Connection` | `keep-alive` | HTTP/1.1 long-lived response |
| `X-Accel-Buffering` | `no` | Disables nginx proxy buffering — events flush immediately. No-op on non-nginx |

### Tuning reconnect

The default browser reconnect delay is 3 seconds. Send a `retry:` field
once at the start of the stream to override it:

```rust
let preamble = SseEvent::data("ready").with_retry(Duration::from_secs(5));
```

`Duration::ZERO` is valid per the spec ("reconnect immediately") and is
emitted verbatim — no coercion. For production streams a 5–15 second
retry strikes a balance between fast recovery and not hammering the
server during a regional outage.

### Why Suprnova diverges

Laravel ships SSE as a one-off helper on `Response`: `Response::eventStream(fn () => ...)`
takes a generator-yielding closure and frames each yielded value as a
`data:` line. It does not model `event:` / `id:` / `retry:` as first-class
fields, has no built-in keep-alive primitive, and does not sanitize values
that would inject extra fields on the wire.

Suprnova treats SSE as a real subsystem rather than a one-off helper:

- `SseEvent` is a typed value with fallible (`try_with_*`) and infallible
  (`with_*`) builders, distinct `Frame` and `Comment` kinds, and a
  documented sanitization contract on every single-line field.
- `HttpResponse::sse(stream)` plugs into the same `stream_bytes` body
  pipeline used by any other long-lived response, so SSE shares one
  cancellation, headers, and panic-isolation path with the rest of the
  framework.
- Producers compose any `Stream<Item = SseEvent>` — `tokio::sync::mpsc`,
  `tokio::sync::broadcast`, `futures::stream::iter`, or the
  [BroadcastHub](broadcasting.md) fan-out adapter. None of these require
  a framework escape hatch.
- A `Last-Event-ID` reader (`sse::last_event_id`) and the WHATWG NUL-drop
  rule are in the box, so resume-after-drop is one parse call away rather
  than a custom header utility per app.

## Reference

| Symbol | Purpose |
|--------|---------|
| `suprnova::sse::SseEvent` | One emittable piece of an SSE stream. Two kinds — `Frame` (event with optional `event` / `id` / `retry` + `data`) and `Comment` (keep-alive). |
| `SseEvent::data(text)` | Build a frame with only `data:` lines. |
| `SseEvent::json(event, &payload)` | Build a frame whose payload is `serde_json`-serialized `payload`; sets `event:` to `event`. Returns `Result<Self, serde_json::Error>`. |
| `SseEvent::error(message)` | Build a frame with `event: error` and the supplied message as `data`. |
| `SseEvent::comment(text)` | Build a comment-only event (`: <text>\n\n`). Browser-invisible; keeps proxies awake. |
| `SseEvent::keep_alive()` | Shorthand for the empty comment `:\n\n`. Minimum-bytes heartbeat. |
| `.with_event(name)` / `.with_id(id)` / `.with_retry(Duration)` | Infallible builders on a `Frame`; silent no-op on a `Comment`. Strip CR / LF / NUL at `to_wire()` time with a structured WARN. |
| `.try_with_event(name)` / `.try_with_id(id)` | Fallible siblings — return `Err(FrameworkError::validation(...))` on CR / LF / NUL. Use when the value flows from user input and you want a 4xx instead of a silent strip. |
| `.event()` / `.id()` / `.retry()` / `.payload()` / `.is_comment()` / `.comment_text()` | Accessors. `payload()` is named to avoid colliding with the `data` constructor. |
| `SseEvent::to_wire()` | Serialize to `Bytes` in the SSE wire format. Public so tests and adapters can encode without crossing the response builder. |
| `suprnova::sse::last_event_id(&Request) -> Option<String>` | Read the `Last-Event-ID` header. Returns `None` when absent OR when the value contains a NUL byte (WHATWG drops invalid ids). |
| `suprnova::sse::last_event_id_from_value(Option<&str>)` | Pure helper exposing the same validation contract — unit-testable without building a `Request`. |
| `HttpResponse::sse(stream)` | Build a streaming response from any `Stream<Item = SseEvent> + Send + Sync + 'static`. Sets `Content-Type`, `Cache-Control`, `Connection`, `X-Accel-Buffering`. |

## Next

- [WebSockets](websockets.md) — the other long-lived connection, when you need bidirectional or binary frames.
- [Broadcasting](broadcasting.md) — `BroadcastHub` fan-out shared with WebSocket subscribers.
- [Notifications](notifications.md) — channel drivers for non-streaming push delivery (mail, database, broadcast).
- [Web Push](web-push.md) — server-pushed notifications that reach the client when no `EventSource` is open.
- [Responses](responses.md) — the rest of the `HttpResponse` builder surface.
