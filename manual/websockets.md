# WebSockets

Suprnova WebSocket routes sit alongside HTTP routes in the same router. You register a path and a handler; the framework detects the `Upgrade: websocket` request at that path, completes the RFC 6455 handshake, and calls your handler with a typed `WsSocket` and the original `Request`. There is no separate WebSocket server to run — WebSocket connections are upgraded from the same hyper listener that serves your HTTP traffic.

The handler receives the `Request` that triggered the upgrade. **Headers, raw cookies, captured path parameters, and the query string are all available** — anything that can be read directly off the upgrade request. **Middleware does not run on WebSocket upgrade requests in v1**, so anything that depends on middleware (the thread-local `session()` accessor, `RequestIdMiddleware`-attached request IDs, `AuthMiddleware`-derived identity) is *not* populated. Auth checks happen inline inside the handler — typically via a bearer token or a raw session cookie validated against your session store — and the handler closes the socket explicitly if the caller is unauthorized. Per-route WebSocket middleware lands in Phase 7B.

When the handler returns `Ok(())`, the framework sends a clean close frame (code 1000) and tears down the connection. When it returns `Err(_)`, the error is logged and the connection closes with code 1011 (internal error). The framework also runs a heartbeat task for each connection — it sends a Ping every 30 seconds by default — so idle connections stay alive through NAT gateways and load balancers without any work on your part.

## Quick Start

Add an `EchoHandler` to your app and register it in the route list.

**`src/ws/echo.rs`:**

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

**`src/routes.rs`** (inside `routes! { ... }`):

```rust
ws!("/ws/echo", app_ws::echo::EchoHandler),
```

Start the app and connect with `wscat`:

```bash
cargo run --bin app
```

```bash
# In another terminal:
wscat -c ws://localhost:3000/ws/echo
Connected (press CTRL+C to quit)
> hello
< echo: hello
> suprnova
< echo: suprnova
```

Type any line and the server echoes it back with the `echo: ` prefix. Press `Ctrl+C` to close — `wscat` sends a close frame; the handler's `recv_text()` returns `Ok(None)` and the loop exits cleanly.

## The `WsSocket` API

`WsSocket` is the bidirectional handle the framework passes to your handler. It wraps the underlying tungstenite stream with typed send/recv methods and hides the split-sink/stream complexity.

### `send_text`

```rust
socket.send_text("hello").await?;
socket.send_text(format!("user {id} joined")).await?;
```

Enqueues a UTF-8 text frame. Returns `Err` only if the connection is already closed (the remote peer disconnected or you called `close` earlier). The send path is non-blocking from the handler's perspective — frames are forwarded to the sink by an internal task.

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
// Ok(None) means the peer closed the connection
```

Receives the next text frame. Binary frames, Ping, and Pong frames are skipped automatically — the heartbeat pings the framework sends are handled transparently and never surface here. Returns `Ok(None)` when the peer sends a close frame or the connection drops. Returns `Err` on a protocol-level error.

This is the method most handlers should use. If your handler only exchanges text messages, this loop pattern is the entire receive side.

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

Receives the next message of any type, including Binary, Ping, Pong, and Close frames. Use this when your protocol mixes text and binary frames, or when you need to inspect close codes. `Ok(None)` means the underlying stream ended.

### `close`

```rust
socket.close(1008, "policy violation").await?;
return Ok(());
```

Sends a close frame with the given code and reason string and returns. Subsequent sends on the same socket will return `Err` because the forwarder task has terminated. Always return `Ok(())` immediately after calling `close` — there is nothing useful to do with the socket after a close frame has been sent.

Common close codes: `1000` (normal), `1008` (policy violation), `1011` (internal server error). The full list is in [RFC 6455 §7.4.1](https://www.rfc-editor.org/rfc/rfc6455#section-7.4.1).

## `WsConfig`

`WsConfig` controls per-connection behavior. Defaults aim at public, browser-facing endpoints — every active connection reserves a tungstenite buffer sized to `max_message_size`, so the framework defaults small and lets routes that need more raise the limits explicitly.

| Field               | Default  | Type       | Effect |
|---------------------|----------|------------|--------|
| `ping_interval`     | 30s      | `Duration` | How often the framework sends a Ping frame to keep the connection alive. |
| `max_message_size`  | 1 MiB    | `usize`    | Maximum reassembled message size in bytes. Messages larger than this are rejected. |
| `max_frame_size`    | 64 KiB   | `usize`    | Maximum single WebSocket frame size in bytes. |
| `max_missed_pings`  | 2        | `usize`    | Consecutive missed Pongs before the framework closes the connection with code 1011. |
| `origin_policy`     | `SameOrigin` | `OriginPolicy` | Origin-header policy enforced at upgrade time. |

**Recommended overrides by use case:**

- **Chat / notifications / cursor positions** — the defaults are fine. You may lower `ping_interval` to 15s if your load balancer has an aggressive idle-connection timeout.
- **Trusted internal feeds** (server-to-server fan-out, bulk export, large binary transfers) — start from [`WsConfig::generous()`], which raises `max_message_size` to 64 MiB and `max_frame_size` to 16 MiB while keeping other defaults. Routes published to the public internet should not use `generous()` without an explicit decision: per-connection buffers multiply across concurrent sockets.
- **Specific oversize payload** (e.g. one route that uploads 256 MiB audio files) — set the fields directly: `max_message_size: 256 * 1024 * 1024`. Don't apply the larger limit to routes that don't need it.

The config struct is `Default`-constructible and all fields are public:

```rust
use std::time::Duration;
use suprnova::ws::WsConfig;

// Public default — safe for browser-facing endpoints.
let config = WsConfig {
    ping_interval: Duration::from_secs(15),
    ..Default::default()
};

// Trusted-feed factory — raises message + frame caps.
let trusted = WsConfig::generous();
assert_eq!(trusted.max_message_size, 64 * 1024 * 1024);
assert_eq!(trusted.max_frame_size, 16 * 1024 * 1024);
```

A per-route override is applied via `.config(WsConfig { ... })` on the WS route entry (see [`Router::ws_with_config`] and the [Per-Route WsConfig section in broadcasting docs]). Routes without an override inherit `WsConfig::default()`.

[`WsConfig::generous()`]: https://docs.rs/suprnova/latest/suprnova/ws/struct.WsConfig.html#method.generous
[`Router::ws_with_config`]: https://docs.rs/suprnova/latest/suprnova/routing/struct.Router.html#method.ws_with_config
[Per-Route WsConfig section in broadcasting docs]: ./broadcasting.md#per-route-wsconfig

## Path Parameters

WebSocket routes support the same `{param}` capture syntax as HTTP routes. The captured values are available on the `Request` the handler receives.

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

`req.param("id")` returns `Result<&str, ParamError>`. The `?` operator propagates a `FrameworkError::ParamError` if the segment is missing, which closes the connection with code 1011. In practice, if the route matched, the capture is always present — the error path is a safety net against typos in the param name.

For the full `Request` API — headers, cookies, session, query string — see [the request docs](requests.md.md).

## Auth at Connect

The handler receives the full `Request` from the HTTP upgrade. You can read headers and cookies exactly as you would in an HTTP controller, then close the socket if the caller is not authorized.

> **v1 caveat: middleware does not run on WebSocket upgrade requests.** Phase 7A's upgrade branch sits *above* the HTTP middleware chain — the upgrade is detected, handed off to the handler, and the middleware chain is bypassed entirely. That means `SessionMiddleware` does not populate the thread-local `session()` accessor for WS connections, and any other middleware you registered does not run either. Per-route WebSocket middleware (including a Phase 7B–native session attach) lands in Phase 7B. Until then, handlers do their auth check inline by reading the raw request directly.

The pattern for v1 is **bearer-token-in-header** (the most portable choice — `wscat`, browser clients, and load balancers all pass headers cleanly):

```rust
use async_trait::async_trait;
use suprnova::{FrameworkError, http::Request, ws::{WebSocketHandler, WsSocket}};

pub struct PrivateChatHandler;

#[async_trait]
impl WebSocketHandler for PrivateChatHandler {
    async fn handle(&self, mut socket: WsSocket, req: Request) -> Result<(), FrameworkError> {
        // Read the Authorization header from the upgrade request and
        // validate it against your token store. Replace `verify_token`
        // with your own validation; the framework doesn't run any
        // middleware here, so this is the only auth gate.
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

        // Handler proceeds for authenticated connections.
        while let Some(text) = socket.recv_text().await? {
            socket.send_text(format!("[user {user_id}] {text}")).await?;
        }
        Ok(())
    }
}

async fn verify_token(_token: &str) -> Option<i64> {
    // Your token-store lookup goes here.
    Some(42)
}
```

Cookie-based auth works the same way — `req.cookie("session_id")` returns `Option<String>` from the raw cookie header (no middleware required). You then look the session up in your own session store. If your store uses the framework's `SessionStore` directly, you can resolve it from the container the same way controllers do, just without the thread-local convenience accessor.

Always return `Ok(())` after calling `close`. Returning `Err` after a close would log a spurious error; the socket is already closing cleanly.

Per-route auth middleware — where the framework rejects the upgrade request *before* your handler code runs, and where `SessionMiddleware` attaches the session before `handle()` is invoked — lands in Phase 7B.

## Production Deployment

The framework handles the WebSocket handshake and all frame I/O. You do not need any extra configuration on the framework side for production.

**TLS termination happens upstream.** Clients connect to `wss://` on your nginx, Caddy, or load balancer; the proxy strips TLS and forwards plain `ws://` to the framework. The framework does not need a `rustls` feature or a TLS certificate. Per-connection TLS directly to the framework is out of scope for v1 (see below).

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

The `proxy_read_timeout` and `proxy_send_timeout` values must be long enough to cover idle connections between heartbeat pings. With the default 30s ping interval, 3600s (one hour) is a comfortable ceiling; lower it if your connections are short-lived.

### Caddy

```caddy
reverse_proxy /ws/* localhost:3000 {
    header_up Upgrade {http.request.header.Upgrade}
    header_up Connection "Upgrade"
}
```

Caddy handles the `Upgrade` and `Connection` headers automatically when proxying; the explicit `header_up` directives above are shown for clarity but are not required in all Caddy configurations.

### Load balancers (AWS ALB, GCP GLB, etc.)

Enable WebSocket support on the listener rule (AWS ALB does this automatically when the target group's protocol is HTTP/1.1 with sticky sessions off). Ensure the idle timeout on the load balancer is at least as long as your `ping_interval`; the framework's heartbeat keeps connections alive, but the load balancer will drop connections that appear idle from its perspective.

## Out of Scope for v1

The following items are intentionally deferred:

- **Subprotocol negotiation** (`Sec-WebSocket-Protocol` echo) — the framework does not inspect or echo subprotocol headers. Negotiation and per-subprotocol dispatch land in Phase 7B alongside broadcasting.

- **`permessage-deflate` compression** — tungstenite has a `deflate` feature behind a Cargo flag. Enabling it as a configurable toggle is deferred to a future phase.

- **Per-connection TLS (`wss://` directly to the framework)** — TLS termination upstream is the supported deployment model. A future `rustls` feature could expose direct `wss://` without a proxy; it is not in scope for v1.

- **Per-route auth middleware** — today, auth checks happen inside the handler by reading session/cookie state from the `Request` passed in. Per-WS-route `.middleware()` chaining lands in Phase 7B.

- **Close-on-no-pong enforcement** (`max_missed_pings`) — the heartbeat sends Pings but does not yet count missed Pongs or drop the connection after N consecutive misses. Enforcement lands in Phase 7B.

## Reference

| Symbol | Purpose |
|--------|---------|
| `suprnova::ws::WebSocketHandler` | Trait with `async fn handle(&self, socket: WsSocket, request: Request) -> Result<(), FrameworkError>`. Implement this on your handler struct. `Send + Sync + 'static` bounds required. |
| `suprnova::ws::WsSocket` | Bidirectional handle passed to the handler. Methods: `send_text`, `send_binary`, `recv_text`, `recv`, `close`. |
| `suprnova::ws::WsConfig` | Connection configuration: `ping_interval`, `max_message_size`, `max_frame_size`. `Default` impl applies the v1 global defaults. |
| `Router::ws(path, handler)` | Direct registration on a `Router` value: `Router::new().ws("/ws/echo", EchoHandler)`. Accepts any `WebSocketHandler`. |
| `ws!(path, Handler)` | Macro form for use inside `routes! { ... }`. Produces a WebSocket route entry. Does not support `.name()` or `.middleware()` in v1. |
