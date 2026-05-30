# WebSockets

Suprnova WebSocket routes sit alongside HTTP routes in the same router. You register a path and a handler; the framework detects the `Upgrade: websocket` request at that path, runs the same middleware chain an HTTP GET to that path would run, completes the RFC 6455 handshake, and calls your handler with a typed `WsSocket` plus the original `Request`. There is no separate WebSocket server — connections are upgraded from the same hyper listener that serves your HTTP traffic. The framework also tracks every spawned handler in a per-server `JoinSet`, so a graceful shutdown drains in-flight connections before the listener exits.

## Quick start

Add an `EchoHandler` and register it in `routes!`.

`src/ws/echo.rs`:

```rust
use async_trait::async_trait;
use suprnova::{FrameworkError, http::Request, ws::{WebSocketHandler, WsSocket}};

pub struct EchoHandler;

#[async_trait]
impl WebSocketHandler for EchoHandler {
    async fn handle(&self, mut socket: WsSocket, _req: Request) -> Result<(), FrameworkError> {
        while let Some(text) = socket.recv_text().await? {
            socket.send_text(format!("echo: {text}")).await?;
        }
        Ok(())
    }
}
```

`src/routes.rs` (inside `routes! { ... }`):

```rust
ws!("/ws/echo", app_ws::echo::EchoHandler),
```

Start the app and connect with `wscat`:

```bash
cargo run --bin app
```

```text
$ wscat -c ws://localhost:3000/ws/echo
Connected (press CTRL+C to quit)
> hello
< echo: hello
> suprnova
< echo: suprnova
```

When `recv_text()` returns `Ok(None)` the peer closed the connection; the loop exits, the handler returns `Ok(())`, and the framework sends a clean Close(1000) frame.

## Lifecycle of an upgrade

A WebSocket handshake is an HTTP GET with `Upgrade: websocket`. The framework runs the full request pipeline against it before any frames flow:

1. **Route match.** The router looks up the path in the WS route table; on miss the request falls through to the HTTP fallback.
2. **Origin policy.** The configured [`OriginPolicy`](#origin-policy) is enforced. A violation returns HTTP 403 with no upgrade.
3. **Subprotocol negotiation.** If the route has `accepted_protocols`, the first client-offered token that overlaps is echoed on the 101 response.
4. **Middleware chain.** `RequestIdMiddleware` runs outermost, followed by every globally-registered middleware, followed by the route's per-route middleware. A non-2xx response from any middleware short-circuits the upgrade — the peer receives the HTTP error, and the WebSocket future drops cleanly.
5. **Handshake.** `hyper_tungstenite::upgrade` produces the future that resolves into a `WebSocketStream`.
6. **Handler dispatch.** The (possibly middleware-rewritten) `Request` and a freshly-built `WsSocket` are handed to `WebSocketHandler::handle`.
7. **Heartbeat + handler.** The framework spawns a per-connection heartbeat task and awaits the handler future under a `ws.connection` tracing span carrying the request id.
8. **Close handshake.** On `Ok(())` the framework sends Close(1000); on `Err(_)` it sends Close(1011 "internal error"). The forwarder is awaited so the close frame is flushed to the wire before the connection's tracked task is reported done.

Return value semantics are inverted from HTTP: there is no body. `Ok(())` means clean disconnect; `Err(_)` is logged and the peer sees Close(1011). Either way the connection tears down.

## The `WsSocket` API

`WsSocket` is the bidirectional handle the framework passes to your handler. Internally the underlying tungstenite stream is split into Sink + Stream halves: a forwarder task owns the sink and drains an mpsc; the handler-facing send methods enqueue onto the mpsc. The handler reads directly from the stream half. This split means the framework can also push frames (heartbeat pings, broadcaster fanout) without contending with the handler's send path.

### `send_text`

```rust
socket.send_text("hello").await?;
socket.send_text(format!("user {id} joined")).await?;
```

Enqueues a UTF-8 text frame. Returns `Err` only when the connection is already closed.

### `send_binary`

```rust
socket.send_binary(bytes).await?;
```

Enqueues a binary frame. Accepts anything `Into<Vec<u8>>`. Same error semantics as `send_text`.

### `recv_text`

```rust
while let Some(text) = socket.recv_text().await? {
    // text: String
}
// Ok(None) means the peer closed.
```

Returns the next text message, silently discarding frame kinds a text-only handler isn't expected to care about:

- `Message::Binary` — peer binary payload
- `Message::Ping` — peer-initiated ping (tungstenite handles the pong automatically)
- `Message::Pong` — peer pong reply to a framework heartbeat (the missed-ping counter is reset to zero as a side effect)
- `Message::Frame` — raw frame variants from server-side contexts; never expected at this layer

A swallowed frame is gone; there is no retroactive way to see it. If the handler needs to observe binary frames or close codes, use [`recv`](#recv) from the very first read.

### `recv`

```rust
use tokio_tungstenite::tungstenite::Message;

while let Some(msg) = socket.recv().await? {
    match msg {
        Message::Text(t)   => { /* ... */ }
        Message::Binary(b) => { /* ... */ }
        Message::Close(_)  => break,
        _                  => {}
    }
}
```

Returns the next message of any kind, including Binary, Ping, Pong, and Close. `Pong` still resets the missed-ping counter as a side effect before it's returned. `Ok(None)` means the underlying stream ended.

### `close`

```rust
socket.close(1008, "policy violation").await?;
return Ok(());
```

Enqueues a close frame and returns. The forwarder writes the frame to the sink, calls `close()` on the sink, and terminates. Subsequent sends on the same socket return `Err` because the forwarder is gone. Always return `Ok(())` immediately after calling `close`.

`close` validates its arguments up front against RFC 6455 §7.4 + §5.5.1:

- `code` must satisfy `CloseCode::is_allowed()`. Reserved or invalid codes (1004, 1005, 1006, 1015, anything below 1000, anything above 4999) are rejected with `Err` and **no frame is sent** — the connection stays open and the caller can retry with a valid code. Use 1000 for normal closure, 1001-1013 for the defined reasons, 3000-3999 for IANA-registered codes, or 4000-4999 for application-private codes.
- `reason` is capped at 123 bytes (the 125-byte control-frame limit minus the two-byte code). Longer reasons are rejected without enqueuing anything.

### Why Suprnova diverges

PHP frameworks bolt WebSocket support on as a separate process (ratchet, soketi, pusher). Suprnova's WebSocket route lives in the same `routes! { ... }` as your HTTP routes, served by the same hyper listener, drained by the same graceful-shutdown path. There is one binary, one config, one deploy. Long-lived connections are first-class because Tokio makes them cheap; the framework doesn't have to apologize for them.

## Path parameters

WebSocket routes support the same `{param}` capture syntax as HTTP routes. Captured values are available on the `Request` passed to the handler.

```rust
// In routes!:
ws!("/ws/rooms/{id}", RoomHandler),
```

```rust
use async_trait::async_trait;
use suprnova::{FrameworkError, http::Request, ws::{WebSocketHandler, WsSocket}};

pub struct RoomHandler;

#[async_trait]
impl WebSocketHandler for RoomHandler {
    async fn handle(&self, mut socket: WsSocket, req: Request) -> Result<(), FrameworkError> {
        let room_id = req.param("id")?;
        socket.send_text(format!("joined room {room_id}")).await?;
        while let Some(text) = socket.recv_text().await? {
            socket.send_text(format!("[{room_id}] {text}")).await?;
        }
        Ok(())
    }
}
```

`req.param("id")` returns `Result<&str, ParamError>`; the `?` propagates a `FrameworkError::ParamError` if the segment is missing, which causes the handler to return `Err` and the framework to send Close(1011). In practice the capture is always present when the route matched — the error path is a safety net against param-name typos.

Express-style `:id` segments are also accepted (`ws!("/ws/rooms/:id", h)`) and convert to matchit-form internally.

For the full `Request` API — headers, cookies, query string, peer address — see [the request docs](requests.md).

## Per-route middleware

Chain `.middleware(M)` on the `ws!` entry. Multiple middleware compose left to right and run in the same fixed order an HTTP request to the same path would run: `RequestIdMiddleware` outermost, then every globally registered middleware, then the per-route chain, then the handler.

```rust
ws!("/ws/private", PrivateHandler)
    .middleware(AuthMiddleware::new())
    .middleware(RateLimitMiddleware::connections_per_ip(100)),
```

A non-2xx response from any middleware short-circuits the upgrade. The peer receives the rejection (e.g. 401, 403) with `X-Request-Id` set, the unwoken WebSocket future drops cleanly, and the handler is never called. This is the right layer for transport-level checks: who may open the connection at all, where the connection is coming from, how many concurrent connections per identity.

Middleware can substitute a modified `Request` by calling `next(modified_req)`. The terminator captures whatever the chain ultimately passes through, and that is what the handler sees as its `Request` argument. Middleware that resolves identity (a session lookup, a token check) can attach the result via `Request` extensions; the handler reads it back the same way HTTP controllers do.

Direct-on-`Router` variants (`Router::ws`, `Router::ws_with_middleware`, `Router::ws_with_config`, `Router::ws_with_middleware_and_config`) cover the same surface for code that builds a `Router` outside the macro. Each has a fallible `try_*` sibling that returns `Err(FrameworkError)` on duplicate or malformed patterns instead of panicking.

### Why Suprnova diverges

Most ecosystems either skip middleware on WebSocket upgrades (the Node convention) or force a separate registration ceremony for "WebSocket middleware" (the .NET / Spring convention). Suprnova treats the upgrade as the HTTP GET it actually is: the same chain runs, in the same order, with the same short-circuit semantics. There is no second concept to learn — `AuthMiddleware`, `RateLimitMiddleware`, `RequestIdMiddleware`, `CorsMiddleware` work on WS routes because they work on any route. Origin enforcement is the only extra wrinkle, and it's a property of `WsConfig`, not a separate middleware.

## Auth at connect

The handler receives the middleware-rewritten `Request`. Three patterns work well, in increasing order of integration with the rest of the framework:

**Pattern 1 — inline bearer token in the handler.** Simplest. Works without any auth middleware. `wscat`, browser clients, and load balancers all pass headers cleanly.

```rust
use async_trait::async_trait;
use suprnova::{FrameworkError, http::Request, ws::{WebSocketHandler, WsSocket}};

pub struct PrivateChatHandler;

#[async_trait]
impl WebSocketHandler for PrivateChatHandler {
    async fn handle(&self, mut socket: WsSocket, req: Request) -> Result<(), FrameworkError> {
        let Some(token) = req.header("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
        else {
            socket.close(1008, "missing bearer token").await?;
            return Ok(());
        };
        let Some(user_id) = verify_token(token).await else {
            socket.close(1008, "invalid bearer token").await?;
            return Ok(());
        };
        while let Some(text) = socket.recv_text().await? {
            socket.send_text(format!("[user {user_id}] {text}")).await?;
        }
        Ok(())
    }
}

async fn verify_token(_token: &str) -> Option<i64> { Some(42) }
```

**Pattern 2 — gate the upgrade with a route middleware.** Reject unauthorized opens before any frames flow. Cleaner separation of concerns; the handler only sees authenticated connections.

```rust
ws!("/ws/private", PrivateChatHandler)
    .middleware(AuthMiddleware::new()),
```

`AuthMiddleware` returns 401 on unauthenticated requests; the upgrade is aborted with the rejection response and the handler is never called.

**Pattern 3 — middleware gate plus handler re-read.** Middleware short-circuits unauthorized opens; the handler then re-reads the same credential (token, cookie, etc.) it knows is now present to identify which user just connected:

```rust
async fn handle(&self, mut socket: WsSocket, req: Request) -> Result<(), FrameworkError> {
    // Middleware already vetted the bearer; we only get here if it was valid.
    let token = req.bearer_token().expect("auth middleware vetted bearer presence");
    let user_id = lookup_user_by_token(&token).await?;
    // ...
}
```

The thread-local accessors that work in HTTP controllers — `session()`, `Auth::user()`, the per-request `Context` bag — are **not** populated inside a WebSocket handler. The middleware chain's task-local scopes unwind when the chain returns; the handler runs in a freshly spawned task that only inherits the request id. Read everything the handler needs directly off the `Request` (headers, cookies via `req.cookie("...")`, captured params, the bearer token via `req.bearer_token()`) — those survive into the handler task.

## `WsConfig`

`WsConfig` controls per-connection behavior. Defaults aim at public, browser-facing endpoints — each active connection reserves a tungstenite buffer sized to `max_message_size`, so the framework defaults small and lets routes that need more raise the limits explicitly.

| Field                 | Default        | Type            | Effect |
|-----------------------|----------------|-----------------|--------|
| `ping_interval`       | 30s            | `Duration`      | How often the framework sends a Ping frame to keep the connection alive. |
| `max_message_size`    | 1 MiB          | `usize`         | Maximum reassembled message size in bytes. Larger messages are rejected by tungstenite. |
| `max_frame_size`      | 64 KiB         | `usize`         | Maximum single WebSocket frame size in bytes. |
| `max_missed_pings`    | 2              | `usize`         | Consecutive missed Pongs before the heartbeat closes the connection with code 1011. `usize::MAX` disables enforcement. |
| `origin_policy`       | `SameOrigin`   | `OriginPolicy`  | Origin-header check enforced at upgrade time. See [Origin policy](#origin-policy). |
| `accepted_protocols`  | `vec![]`       | `Vec<String>`   | Server's accepted `Sec-WebSocket-Protocol` tokens. Empty means no negotiation. See [Subprotocols](#subprotocols). |

Recommended overrides by use case:

- **Chat / notifications / cursor positions** — defaults are fine. Drop `ping_interval` to 5–10s if your LB has an aggressive idle timeout.
- **Trusted internal feeds** (server-to-server fan-out, bulk export, large binary transfers) — start from `WsConfig::generous()`, which raises `max_message_size` to 64 MiB and `max_frame_size` to 16 MiB while keeping other defaults.
- **Specific oversize payload** (one route that uploads 256 MiB audio files) — set the fields directly; don't apply the larger limit to routes that don't need it.

The config struct is `Default`-constructible and every field is public:

```rust
use std::time::Duration;
use suprnova::ws::WsConfig;

let chat = WsConfig {
    ping_interval: Duration::from_secs(5),
    max_missed_pings: 1,
    ..Default::default()
};

let trusted = WsConfig::generous();
assert_eq!(trusted.max_message_size, 64 * 1024 * 1024);
assert_eq!(trusted.max_frame_size, 16 * 1024 * 1024);
```

Apply the override per route either on the `ws!` entry or on `Router::ws_with_config`:

```rust
ws!("/ws/chat", ChatHandler).config(chat),
```

`WsConfig` is validated at route registration. A zero `ping_interval` or a zero `max_missed_pings` would corrupt the heartbeat task; both are rejected at boot rather than panicking at first connection.

### Heartbeat and close-on-no-pong

For each upgraded connection the framework spawns a heartbeat task that sends `Ping(b"")` every `ping_interval`. On each tick the missed-ping counter increments; on each peer Pong it resets to zero. If the counter reaches `max_missed_pings`, the heartbeat sends Close(1011 "no pong response") and the connection tears down. Set `max_missed_pings` to `usize::MAX` to disable enforcement (pings still flow, but the connection is never closed for missing pongs).

The first tick is consumed at task start so the peer gets at least one full interval of grace before the first ping.

## Origin policy

Browsers always send an `Origin` header on WebSocket handshakes. Unlike `fetch()` / `XMLHttpRequest`, WebSocket upgrades aren't protected by CSRF token middleware (the handshake carries no token), so a same-origin `Origin` check is the only thing standing between a malicious page and a privileged WS endpoint on a logged-in user's session. The framework enforces the configured policy before `hyper_tungstenite::upgrade` is called; a violation returns HTTP 403 with no upgrade.

```rust
use suprnova::ws::{OriginPolicy, WsConfig};

let cfg = WsConfig {
    origin_policy: OriginPolicy::AllowList(vec![
        "https://app.example.com".into(),
        "https://admin.example.com".into(),
    ]),
    ..Default::default()
};
```

| Variant      | Behavior |
|--------------|----------|
| `SameOrigin` (default) | Allow only when `Origin`'s host (and port if present) matches the request's `Host` header. Missing `Origin` is rejected. Scheme is not compared (TLS terminates upstream, so the server can't reliably tell whether the public scheme was https or http). |
| `AllowAny`   | Skip the check. Use only for non-browser endpoints (server-to-server, native apps, test mocks). |
| `AllowList(Vec<String>)` | Allow only when `Origin` exactly matches (case-insensitive) one of the supplied origins. Each entry is the full `scheme://host[:port]` form a browser would send. |

Non-browser clients (CLI tools, servers, native apps) typically don't send an `Origin` header. Routes that serve such clients exclusively should use `AllowAny`; routes serving both should use `AllowList` enumerating every production frontend origin.

## Subprotocols

A WebSocket subprotocol is an application-level token (e.g. `graphql-transport-ws`, `jsonrpc-2.0`) the client and server agree on during the handshake. Populate `accepted_protocols` to participate:

```rust
use suprnova::ws::WsConfig;

let cfg = WsConfig {
    accepted_protocols: vec![
        "graphql-transport-ws".into(),
        "graphql-ws".into(),
    ],
    ..Default::default()
};
```

When the client offers `Sec-WebSocket-Protocol`, the framework picks the first client-offered token (in client preference order per RFC 6455 §4.2.2) that overlaps with `accepted_protocols`, matched case-insensitively, and echoes it on the 101 response. If the client offered protocols but none matched, the upgrade still succeeds with no `Sec-WebSocket-Protocol` header — RFC 6455 then requires the browser to fail the connection client-side, which is the right behavior (a server that proceeded would silently be speaking the wrong protocol).

When `accepted_protocols` is empty, negotiation is skipped entirely — the upgrade response omits `Sec-WebSocket-Protocol` and the client falls back to default protocol handling.

## Production deployment

The framework handles the handshake and frame I/O. You do not need any extra configuration on the framework side for production.

**TLS termination happens upstream.** Clients connect to `wss://` on nginx, Caddy, or the cloud load balancer; the proxy strips TLS and forwards plain `ws://` to the framework. The framework does not need a `rustls` feature or a TLS certificate.

### nginx

```nginx
location /ws/ {
    proxy_pass http://127.0.0.1:3000;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "Upgrade";
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_read_timeout 3600s;
    proxy_send_timeout 3600s;
}
```

`proxy_read_timeout` and `proxy_send_timeout` must be long enough to cover idle gaps between heartbeats. With the default 30s `ping_interval`, 3600s is a comfortable ceiling.

### Caddy

```caddy
reverse_proxy /ws/* localhost:3000 {
    header_up Upgrade {http.request.header.Upgrade}
    header_up Connection "Upgrade"
}
```

Caddy handles `Upgrade` / `Connection` automatically when proxying; the explicit `header_up` directives above are for clarity.

### Cloud load balancers (AWS ALB, GCP GLB)

Enable WebSocket support on the listener rule (AWS ALB does this automatically when the target group's protocol is HTTP/1.1 with sticky sessions off). Ensure the load balancer's idle timeout is at least as long as `ping_interval`; the framework's heartbeat keeps the wire active, but the LB drops connections that look idle from its perspective.

## Graceful shutdown

Every spawned WebSocket handler is tracked in the server's `WS_TASKS` `JoinSet`. On `Ctrl-C` or an external shutdown signal, the listener stops accepting new connections and `Server::run` drains the set before the process exits. The handler future doesn't resolve until the close handshake has been flushed: after the user's `handle` returns, the framework awaits the forwarder so the final Close(1000) or Close(1011) frame is written to the wire before the connection's task is reported done. In a clean shutdown peers see a normal close, not a TCP reset.

Completed handles are reaped opportunistically during the lifetime of the server, so the `JoinSet` doesn't grow unbounded under long-running operation.

## Reference

| Symbol | Purpose |
|---|---|
| `suprnova::ws::WebSocketHandler` | Trait: `async fn handle(&self, socket: WsSocket, request: Request) -> Result<(), FrameworkError>`. `Send + Sync + 'static`. |
| `suprnova::ws::WsSocket` | Bidirectional handle. Methods: `send_text`, `send_binary`, `recv_text`, `recv`, `close`. `close` validates code + reason length up front. |
| `suprnova::ws::WsConfig` | Per-connection config. Fields: `ping_interval`, `max_message_size`, `max_frame_size`, `max_missed_pings`, `origin_policy`, `accepted_protocols`. `Default` + `generous()` constructors. Validated at registration. |
| `suprnova::ws::OriginPolicy` | `SameOrigin` (default), `AllowAny`, `AllowList(Vec<String>)`. Enforced at upgrade time. |
| `ws!(path, Handler)` | Macro form for `routes! { ... }`. Returns a `WsRouteDef` supporting `.config(WsConfig)` and `.middleware(M)` in either order. |
| `Router::ws(path, handler)` | Direct registration. Returns `Router`. |
| `Router::ws_with_config(path, handler, cfg)` | Per-route `WsConfig` override. |
| `Router::ws_with_middleware(path, handler, mws)` | Per-route middleware list. |
| `Router::ws_with_middleware_and_config(...)` | Both. |
| `Router::try_ws*` family | Fallible siblings — return `Err(FrameworkError)` on duplicate or malformed patterns instead of panicking. |

## Next

- [Broadcasting](broadcasting.md) — channels, presence, the wire protocol on top of `ws!`
- [Server-Sent Events](sse.md) — one-way push for browsers behind strict proxies
- [Routing](routing.md) — what `routes!` and `ws!` actually expand into
- [Middleware](middleware.md) — writing middleware that gates HTTP and WS uniformly
- [Requests](requests.md) — headers, cookies, query, extensions on the `Request` your handler receives
