# Broadcasting

Broadcasting is the server-to-client notification layer built on top of [the WebSocket primitive](websockets.md.md). When you dispatch a `Broadcastable` event through `EventFacade`, it does two things simultaneously: it fires all in-process event listeners as usual, AND it pushes a JSON envelope to every WebSocket client subscribed to the channels the event declares. You do not manage individual connections — you manage channel subscriptions, and the hub fans out to subscribers.

The `BroadcastHub` is the central bus. By default it is an in-process `InMemoryBroadcastHub`, which keeps everything in one process without external dependencies. A `broadcasting-fanout` Cargo feature swaps the hub for a `SeaStreamerBroadcastHub`, which routes events through a stream broker (Redis Streams) so that publishes from one process reach subscribers connected to other processes in a multi-replica deployment.

Broadcasting sits on top of Phase 7A's WebSocket infrastructure. You register a `build_broadcasting_handler()` route the same way you register any WebSocket route; the framework upgrades the HTTP connection, and from that point on the JSON envelope protocol handles channel subscriptions and event delivery. Everything in [the WebSocket docs](websockets.md.md) — heartbeat pings, `max_missed_pings`, path parameters, per-route middleware — applies to the broadcasting route as well.

## Quick Start

Fifteen lines from handler to browser event:

**`src/channels/order_updates.rs`:**

```rust
use async_trait::async_trait;
use suprnova::broadcasting::Channel;

pub struct OrderUpdates;

#[async_trait]
impl Channel for OrderUpdates {
    fn name(&self) -> &'static str { "order.updates" }
}
```

**`src/events/order_placed.rs`:**

```rust
use serde::{Deserialize, Serialize};
use suprnova::Event;
use suprnova::broadcasting::Broadcastable;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderPlaced {
    pub order_id: i64,
    pub user_id: i64,
}

impl Event for OrderPlaced {
    fn event_name() -> &'static str { "OrderPlaced" }
}

impl Broadcastable for OrderPlaced {
    fn broadcast_on(&self) -> Vec<String> {
        vec!["order.updates".into()]
    }
}
```

**`src/bootstrap.rs`:**

```rust
use std::sync::Arc;
use suprnova::broadcasting::{BroadcastHub, ChannelRegistry, InMemoryBroadcastHub};
use suprnova::container::App;
use suprnova::events::EventFacade;

pub async fn register() {
    let hub = Arc::new(InMemoryBroadcastHub::new());

    let mut registry = ChannelRegistry::new();
    registry.register(OrderUpdates);
    let registry = Arc::new(registry);

    App::bind::<dyn BroadcastHub>(hub.clone());
    App::singleton(registry.clone());

    EventFacade::broadcast::<OrderPlaced>(hub.clone()).await;
}
```

**`src/routes.rs`:**

```rust
ws!("/ws/broadcast", build_broadcasting_handler()).middleware(SessionMiddleware::new()),
```

**Connect and observe:**

```bash
cargo run --bin app
```

```bash
# In another terminal:
wscat -c ws://localhost:3000/ws/broadcast
Connected (press CTRL+C to quit)
> {"action":"subscribe","channel":"order.updates","data":{}}
< {"action":"subscribed","channel":"order.updates"}
# When an OrderPlaced is dispatched elsewhere in the app:
< {"action":"event","channel":"order.updates","event":"OrderPlaced","data":{"order_id":99,"user_id":42}}
```

Dispatch from any controller or service:

```rust
EventFacade::dispatch(OrderPlaced { order_id: 99, user_id: 42 }).await?;
```

## Channels

A channel is a named subscription point. Clients subscribe by name; the hub delivers events to all active subscribers on that channel. Three channel types are available.

### Public Channels

Public channels allow any client to subscribe — no auth check runs.

```rust
use async_trait::async_trait;
use suprnova::broadcasting::Channel;

pub struct OrderUpdates;

#[async_trait]
impl Channel for OrderUpdates {
    fn name(&self) -> &'static str { "order.updates" }
    // Default authorize() returns true — open to all.
}
```

Register the channel in bootstrap before starting the server:

```rust
registry.register(OrderUpdates);
```

### Private Channels

Private channels run an async `authorize` gate when a client subscribes. Return `false` (or `true`) to grant or deny access. A denied subscribe attempt results in an `error` frame with `reason: "unauthorized"`.

```rust
use async_trait::async_trait;
use serde_json::Value;
use suprnova::broadcasting::{Channel, ChannelParams, PrivateChannel};
use suprnova::http::Request;

pub struct PrivateChat;

#[async_trait]
impl Channel for PrivateChat {
    fn name(&self) -> &'static str { "chat.private" }

    async fn authorize(&self, _req: &Request, _params: &ChannelParams, data: &Value) -> bool {
        data["token"].as_str().map(|t| t == "valid").unwrap_or(false)
    }
}

impl PrivateChannel for PrivateChat {}
```

The `data` value is whatever the client sent in the subscribe frame's `"data"` field — a bearer token, a signed subscription ticket, or any application-defined payload. The `Request` is the original HTTP upgrade request, so you can also read headers or cookies from it. `params` carries any values captured from a parameterized channel name (see below) and is empty for fixed-name channels.

### Parameterized Channels

A channel `name()` may contain `{param}` segments. One registered channel then serves every concrete subscription that matches the pattern, and the captured values arrive in `authorize` (and the other hooks) as a `ChannelParams` map — the same model as Laravel's `Broadcast::channel('orders.{id}', …)`:

```rust
use async_trait::async_trait;
use serde_json::Value;
use suprnova::broadcasting::{Channel, ChannelParams, PrivateChannel};
use suprnova::http::Request;

pub struct OrderChannel;

#[async_trait]
impl Channel for OrderChannel {
    fn name(&self) -> &'static str { "orders.{id}" }

    async fn authorize(&self, _req: &Request, params: &ChannelParams, _data: &Value) -> bool {
        let order_id = params.get("id").unwrap_or_default();
        // Gate on the captured id — e.g. does the session user own this order?
        !order_id.is_empty()
    }
}
impl PrivateChannel for OrderChannel {}

// One registration serves orders.42, orders.99, …
registry.register(OrderChannel);
```

Each `{param}` binds exactly one dot-segment: `orders.{id}` matches `orders.42` but not `orders` or `orders.42.line`. Resolution prefers an exact fixed-name registration (`orders.featured` beats `orders.{id}` for that name), then the most specific pattern (most literal segments); register non-overlapping patterns to keep matches unambiguous.

### Presence Channels

Presence channels track membership: the hub knows which clients are subscribed and what their member information is. When a new client subscribes, the hub delivers a `presence.here` snapshot to that client; when any client joins or leaves, the hub broadcasts `presence.joined` or `presence.left` to all subscribers on the channel.

Implement both `Channel::presence_info` and `PresenceChannel` together — `presence_info` is the hook that tells the hub this channel participates in presence tracking, and `PresenceChannel::member_info` is called at subscribe time to collect the joining member's data:

```rust
use async_trait::async_trait;
use serde_json::{json, Value};
use suprnova::broadcasting::{Channel, ChannelParams, PresenceChannel};
use suprnova::http::Request;
use suprnova::FrameworkError;

pub struct PresenceLobby;

#[async_trait]
impl Channel for PresenceLobby {
    fn name(&self) -> &'static str { "presence.lobby" }

    fn presence_info<'a>(&'a self) -> Option<&'a dyn PresenceChannel> {
        Some(self)
    }
}

#[async_trait]
impl PresenceChannel for PresenceLobby {
    async fn member_info(&self, req: &Request, _params: &ChannelParams) -> Result<Value, FrameworkError> {
        // Return whatever member data your client needs — typically
        // a user ID so clients can identify who is present.
        Ok(json!({ "user_id": 42, "display_name": "Alice" }))
    }
}
```

See [Presence channels](#presence-channels) for the full event semantics.

## The Broadcastable Trait

`Broadcastable` extends [the event system](events.md.md). Any event that implements `Broadcastable` is automatically pushed to the hub when dispatched — you call `EventFacade::dispatch` once, and both in-process listeners and WebSocket subscribers receive the event.

```rust
use serde::{Deserialize, Serialize};
use suprnova::Event;
use suprnova::broadcasting::Broadcastable;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderPlaced {
    pub order_id: i64,
    pub user_id: i64,
}

impl Event for OrderPlaced {
    fn event_name() -> &'static str { "OrderPlaced" }
}

impl Broadcastable for OrderPlaced {
    fn broadcast_on(&self) -> Vec<String> {
        // One event, multiple channels. Both channels receive the same envelope.
        vec![
            format!("user.{}.orders", self.user_id),
            "orders.global".into(),
        ]
    }
}
```

Wire up the dispatcher integration once at bootstrap with `EventFacade::broadcast::<E>(hub)`. After that, calling `EventFacade::dispatch(OrderPlaced { ... }).await?` is all that is needed — no separate publish call.

By default the event is serialized to JSON using `serde_json` and delivered to every subscriber on every channel returned by `broadcast_on`. Channels that no client has subscribed to are silently skipped.

Two optional methods refine that default:

- **`broadcast_with(&self) -> Option<Value>`** — return `Some(value)` to push a curated payload instead of the full event serialization (Laravel's `broadcastWith()`). Use it to omit secrets or reshape for the client without changing the event type:

  ```rust
  impl Broadcastable for AccountFunded {
      fn broadcast_on(&self) -> Vec<String> { vec![format!("account.{}", self.account_id)] }
      fn broadcast_with(&self) -> Option<serde_json::Value> {
          // Never put the balance on the wire — only the public id.
          Some(serde_json::json!({ "account_id": self.account_id }))
      }
  }
  ```

- **`broadcast_when(&self) -> bool`** — return `false` to dispatch the event to in-process listeners but skip the WebSocket push (Laravel's `broadcastWhen()`). Only the broadcast is gated; the rest of the event pipeline runs unchanged:

  ```rust
  impl Broadcastable for DraftSaved {
      fn broadcast_on(&self) -> Vec<String> { vec![format!("doc.{}", self.doc_id)] }
      fn broadcast_when(&self) -> bool { self.publish } // only broadcast on publish
  }
  ```

- **`broadcast_to_others(&self) -> bool`** — return `true` to exclude the connection that triggered the broadcast (Laravel's `toOthers()`). Each broadcasting client is assigned a `socket_id` on connect (the `connected` frame) and echoes it as the `X-Socket-ID` header on HTTP requests; a `broadcast_to_others` event dispatched while handling such a request skips that connection. Off-request (a worker or job), or when no `X-Socket-ID` is present, it broadcasts to everyone:

  ```rust
  impl Broadcastable for MessagePosted {
      fn broadcast_on(&self) -> Vec<String> { vec![format!("chat.{}", self.room)] }
      fn broadcast_to_others(&self) -> bool { true } // the sender already has the message
  }
  ```

  This is a per-event-type choice. For per-dispatch exclusion, publish directly: `hub.publish(BroadcastEnvelope::new(channel, event, data).with_except(socket_id))`.

  > **Server-side plumbing.** Suprnova assigns and surfaces the `socket_id` and excludes it on the broadcast path. Echoing `X-Socket-ID` from the browser is the client's job; the bundled typed broadcast client is not yet shipped, so for now you wire the header yourself — read the `connected` frame's `socket_id` and send it back as `X-Socket-ID`.

## The Wire Protocol

All messages over the broadcasting WebSocket route are UTF-8 JSON frames. Two frame shapes are defined: `ClientFrame` (client to server) and `ServerFrame` (server to client).

### Client Frames

| `action` | Required fields | Optional fields | Meaning |
|----------|----------------|-----------------|---------|
| `subscribe` | `channel` | `data` | Subscribe to the named channel. `data` is forwarded to `authorize`. |
| `unsubscribe` | `channel` | | Unsubscribe from the named channel. |
| `publish` | `channel`, `event`, `data` | | Publish an event to all subscribers on the channel. Gated by `Channel::authorize_publish` — see note below. |

`Publish` envelopes are gated by `Channel::authorize_publish`. The default is to reject — channels must explicitly opt in to client-side publishes by overriding the hook. Most server-side broadcasting channels never want client-initiated events; the default-deny shape matches that intent. An unauthorized publish results in a `ServerFrame::Error { reason: "publish unauthorized" }` response to the sender; other subscribers are not notified.

```json
{"action":"subscribe","channel":"chat.42","data":{"token":"abc"}}
{"action":"unsubscribe","channel":"chat.42"}
{"action":"publish","channel":"chat.42","event":"MessagePosted","data":{"text":"hi"}}
```

### Server Frames

| `action` | Fields | Meaning |
|----------|--------|---------|
| `connected` | `socket_id` | Sent once, first. Echo `socket_id` as the `X-Socket-ID` header on HTTP requests so `broadcast_to_others` can exclude this connection. |
| `subscribed` | `channel` | Subscription accepted. |
| `unsubscribed` | `channel` | Unsubscription confirmed. |
| `event` | `channel`, `event`, `data` | An event was broadcast to this channel. |
| `error` | `channel`, `reason` | The last action on this channel failed. |

```json
{"action":"connected","socket_id":"6f1a3c2e-…"}
{"action":"subscribed","channel":"chat.42"}
{"action":"unsubscribed","channel":"chat.42"}
{"action":"event","channel":"chat.42","event":"MessagePosted","data":{"text":"hi"}}
{"action":"error","channel":"chat.42","reason":"unauthorized"}
```

### Example Session Log

```
S → C  {"action":"connected","socket_id":"6f1a3c2e-…"}
C → S  {"action":"subscribe","channel":"order.updates","data":{}}
S → C  {"action":"subscribed","channel":"order.updates"}

# Server dispatches OrderPlaced event:
S → C  {"action":"event","channel":"order.updates","event":"OrderPlaced","data":{"order_id":99,"user_id":42}}

C → S  {"action":"subscribe","channel":"chat.private","data":{"token":"bad"}}
S → C  {"action":"error","channel":"chat.private","reason":"unauthorized"}

C → S  {"action":"unsubscribe","channel":"order.updates"}
S → C  {"action":"unsubscribed","channel":"order.updates"}
```

## Per-Route Middleware

Broadcasting routes support the same `.middleware(M)` chaining as plain WebSocket routes:

```rust
ws!("/ws/broadcast", build_broadcasting_handler()).middleware(SessionMiddleware::new()),
```

A non-2xx response from any middleware in the chain short-circuits the WebSocket upgrade — the client receives the HTTP error response and no upgrade happens. This is the right place to enforce transport-level auth (session validity, origin checks, rate limits at connection time) without duplicating the check inside every channel's `authorize`.

```rust
// Multiple middleware compose left-to-right:
ws!("/ws/broadcast", build_broadcasting_handler())
    .middleware(SessionMiddleware::new())
    .middleware(RateLimitMiddleware::connections_per_ip(100)),
```

Channel-level authorization (who may subscribe to which channel) happens inside `Channel::authorize`. Transport-level middleware (who may open a connection at all) happens here.

### Per-Route WsConfig

Each broadcasting (or plain WebSocket) route can carry its own `WsConfig` that overrides the process-wide defaults. Chain `.config(WsConfig { ... })` after the handler — before or after `.middleware(M)`, the order does not matter:

```rust
ws!("/ws/chat", BroadcastingWsHandler::new(hub, registry))
    .config(WsConfig {
        ping_interval: Duration::from_secs(5),
        max_missed_pings: 1,
        ..Default::default()
    })
    .middleware(SessionMiddleware::new())
```

The four configurable fields and their typical use cases:

| Field | Default | Use case |
|-------|---------|----------|
| `ping_interval` | 30s | Chat routes: shorten to 5–10s to detect dead mobile connections quickly. Bulk-data streaming: lengthen to reduce overhead. |
| `max_missed_pings` | 2 | Set to `1` for chat where a single missed Pong should close immediately. Set to `3+` for flaky mobile networks where brief gaps are expected. |
| `max_message_size` | 1 MiB | Public-endpoint-safe default. Increase for file-transfer or rich-media channels; for trusted internal feeds use `WsConfig::generous()` (64 MiB cap). |
| `max_frame_size` | 64 KiB | Sized for chat / notification frames with headroom. Raise when the client sends unfragmented large frames; or start from `WsConfig::generous()` (16 MiB cap). |

When no `.config(...)` is provided, the route inherits `WsConfig::default()`. Explicit per-route config always wins over the process-wide default.

For routes serving trusted internal feeds (server-to-server fanout, large binary transfers) start from the trusted-feed factory and adjust:

```rust
use suprnova::ws::WsConfig;
use std::time::Duration;

ws!("/ws/internal/firehose", FirehoseHandler::new())
    .config(WsConfig {
        ping_interval: Duration::from_secs(10),
        ..WsConfig::generous() // 64 MiB message / 16 MiB frame
    })
```

## Presence Channels

Presence channels surface membership information — who is currently subscribed — to every subscriber on the channel.

When a client successfully subscribes to a presence channel, the hub:

1. Calls `PresenceChannel::member_info` with the upgrade request to collect the new member's data.
2. Sends a `presence.here` frame to the new subscriber with a snapshot of all current members.
3. Broadcasts a `presence.joined` frame to all other subscribers (including the new one) with the new member's data.

When a subscriber disconnects or sends an unsubscribe frame:

4. The hub broadcasts a `presence.left` frame to all remaining subscribers with the departing member's data.

All three frames arrive as `event` action frames with reserved `event` names:

```json
{"action":"event","channel":"presence.lobby","event":"presence.here","data":{"members":[{"user_id":1},{"user_id":2}]}}
{"action":"event","channel":"presence.lobby","event":"presence.joined","data":{"user_id":3}}
{"action":"event","channel":"presence.lobby","event":"presence.left","data":{"user_id":3}}
```

**Detecting your own join echo.** When you subscribe, you receive both `presence.here` (snapshot) and a `presence.joined` (your own join). Clients that want to suppress the self-join echo should compare the joining member's identity to their own known identity and skip the notification if it matches.

Across processes, presence state is replicated via the `__presence__` meta-channel when using `SeaStreamerBroadcastHub`. Track/untrack operations on any process propagate to all subscribers; `list_members` returns the merged view (local and remote). Dead processes whose `untrack_member` never fired have their members pruned after 60 seconds via TTL. See [Cross-process presence](#cross-process-presence) for details.

## Multi-Process Fanout

By default, the `InMemoryBroadcastHub` fans out only to WebSocket subscribers connected to the current process. For multi-replica deployments, enable the `broadcasting-fanout` Cargo feature and swap the hub for `SeaStreamerBroadcastHub`:

**`Cargo.toml`:**

```toml
suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git", features = ["broadcasting-fanout"] }
```

**`src/bootstrap.rs`:**

```rust
use std::sync::Arc;
use suprnova::broadcasting::{BroadcastHub, ChannelRegistry};
use suprnova::broadcasting::fanout::SeaStreamerBroadcastHub;
use suprnova::container::App;

pub async fn register() {
    let hub = SeaStreamerBroadcastHub::new("redis://broker:6379/my-stream").await?;

    App::bind::<dyn BroadcastHub>(Arc::new(hub));
    // ... rest of bootstrap unchanged
}
```

`SeaStreamerBroadcastHub` accepts any broker URI supported by sea-streamer. The backend is selected at runtime from the URI scheme:

| URI scheme | Backend | Production-ready | Notes |
|------------|---------|------------------|-------|
| `redis://` `rediss://` | Redis Streams | **Yes** | Default recommendation. `rediss://` uses TLS. |
| `kafka://` | Kafka | **Yes** | Requires adding `kafka` to the `sea-streamer` features in `framework/Cargo.toml`. |
| `stdio://` | stdin/stdout pipes | No — tests only | Single-process loopback. Used by the framework's own integration tests. |
| `file://` | Local file | No — single-host | Requires adding `file` to the `sea-streamer` features. |

The stream key (the part after the URI) is the topic/stream name shared by every process in the cluster: `SeaStreamerBroadcastHub::new("redis://host:6379", "suprnova-broadcast")`.

Published events flow through the broker to every process in the cluster. Each process's local in-memory layer delivers the event to its own WebSocket subscribers. The application code that dispatches events does not change — only the bootstrap wiring changes.

**Backend dispatch is enum-based, not trait-object.** The hub stores a concrete `SeaProducer` / `SeaConsumer` from `sea-streamer`'s socket adapter, which is an enum over every backend compiled into the dependency. There is no `dyn Producer` overhead at the call site.

### Cross-process presence

`SeaStreamerBroadcastHub` replicates presence state across processes automatically. Each instance gets a UUID `instance_id` at construction; `track_member` / `untrack_member` publish `PresenceEvent`s to a reserved `__presence__` meta-channel. Every process maintains a `cross_process_view` updated by the consumer task; `list_members` returns this merged view (local and remote uniformly).

**Liveness.** Each process re-publishes its members every 10 seconds (heartbeat). Stale entries — members whose `last_seen` is older than 60 seconds — get pruned every 30 seconds. This handles process crashes that did not get to publish `MemberRemoved`.

## Close-on-No-Pong

The broadcasting WebSocket route participates in the same heartbeat infrastructure as plain WebSocket routes. The framework sends a Ping frame every `WsConfig::ping_interval` seconds (default 30s). If a connection fails to respond with a Pong within `max_missed_pings` consecutive intervals (default 2), the framework closes the connection with code 1011.

```rust
use suprnova::ws::WsConfig;
use std::time::Duration;

let config = WsConfig {
    ping_interval: Duration::from_secs(15),
    max_missed_pings: 3,
    ..WsConfig::default()
};
```

Lowering `ping_interval` helps detect dead connections faster at the cost of higher baseline traffic. `max_missed_pings: 1` closes after the very first missed Pong — use this only in environments where network glitches are rare and you want the fastest possible dead-connection cleanup.

## Production Deployment

The broadcasting WebSocket route is an upgraded HTTP connection on the same hyper listener as your HTTP routes. TLS termination happens upstream, exactly as described in [the WebSocket docs](websockets.md.md). The nginx and Caddy configurations from that doc apply unchanged — extend them to cover the `/ws/broadcast` path.

Active WebSocket handler tasks (including broadcasting connections) are tracked in `WS_TASKS` and are drained on graceful shutdown, so in-flight event deliveries complete before the process exits.

## Out of v1 Scope

The following items are intentionally deferred. Each note describes the path forward when it is taken up.

- **Cross-region replication.** `SeaStreamerBroadcastHub` is single-cluster. Multi-region fan-out would require a higher-level broker topology or a replication layer above sea-streamer.

- **End-to-end encryption per channel.** Channels today are plaintext from the server's perspective. E2EE (where the server cannot read message content) requires key-exchange support in the wire protocol.

- **Per-instance presence visibility.** The framework merges all connected members into one unified view; there is no admin-facing surface for "which members are connected to which specific process." This is a monitoring/ops concern rather than a product concern and is deferred.

## Testing broadcasts

`RecordingBroadcastHub` is a `BroadcastHub` that records every published envelope while still delivering to live subscribers — the Suprnova analogue of Laravel's `Broadcast::fake()`. Bind it in place of `InMemoryBroadcastHub` in a test and assert what was broadcast without subscribing first:

```rust
use suprnova::broadcasting::RecordingBroadcastHub;

let hub = RecordingBroadcastHub::new();
// ... run code that publishes (directly, or via a dispatched Broadcastable) ...
hub.assert_broadcast("orders.42", "OrderShipped");
assert_eq!(hub.count(), 1);
```

`broadcasts()` returns every recorded envelope for finer-grained assertions, and `assert_nothing_broadcast()` asserts none were sent. To assert that a `Broadcastable` *event* was dispatched at all (rather than what reached the wire), `Event::fake()` records the event itself — see [events](events.md.md).

## Reference

| Symbol | Purpose |
|--------|---------|
| `suprnova::broadcasting::Channel` | Trait for named channels. Implement `name()` (required; may be a `{param}` pattern). Override `authorize(&self, req, params, data) -> bool` for private semantics, or `presence_info()` for presence semantics. |
| `suprnova::broadcasting::ChannelParams` | Values captured from a parameterized channel `name()` (e.g. `{id}` for `orders.{id}`). Passed to the channel hooks; `get(key) -> Option<&str>`. Empty for fixed-name channels. |
| `suprnova::broadcasting::PrivateChannel` | Marker trait. Implementing it alongside `Channel` (with a custom `authorize`) makes the channel private. No additional methods. |
| `suprnova::broadcasting::PresenceChannel` | Trait with `async fn member_info(&self, req: &Request, params: &ChannelParams) -> Result<Value, FrameworkError>`. Returns the joining member's data used in `presence.joined` / `presence.here` frames. |
| `suprnova::broadcasting::Broadcastable` | Trait on an `Event`: `broadcast_on() -> Vec<String>` (channels), `broadcast_event_name()` (wire name), `broadcast_with() -> Option<Value>` (curated payload), `broadcast_when() -> bool` (conditional push), `broadcast_to_others() -> bool` (exclude the originating socket via `X-Socket-ID`). Pushed to hub subscribers when dispatched via `EventFacade`. |
| `suprnova::broadcasting::BroadcastHub` | Trait implemented by `InMemoryBroadcastHub` and `SeaStreamerBroadcastHub`. The DI container holds the active hub. |
| `suprnova::broadcasting::InMemoryBroadcastHub` | Default hub. In-process fanout only. No external dependencies. |
| `suprnova::broadcasting::RecordingBroadcastHub` | Test double (`Broadcast::fake()` analogue). Records published envelopes for assertions (`assert_broadcast`, `broadcasts`, `assert_nothing_broadcast`) while still delivering to subscribers. |
| `suprnova::broadcasting::fanout::SeaStreamerBroadcastHub` | Multi-process hub behind the `broadcasting-fanout` feature. Accepts a broker URI. |
| `suprnova::broadcasting::ChannelRegistry` | Registry of all known channels. Populated at bootstrap. Resolved from the container by `build_broadcasting_handler()`. |
| `build_broadcasting_handler()` | Returns a `WebSocketHandler` that implements the full JSON envelope protocol. Resolves the hub and registry from the container. Pass directly to `ws!(...)`. |
| `suprnova::ws::WsConfig::max_missed_pings` | `u32` (default 2). Number of consecutive missed Pongs before the heartbeat closes the connection with code 1011. |
| `ws!(path, build_broadcasting_handler()).middleware(M)` | Route macro syntax. Chains middleware onto the broadcasting route. A non-2xx middleware response short-circuits the upgrade. |
| `Channel::authorize_publish(&self, req: &Request, event: &str, data: &Value) -> bool` | Per-channel publish authorization hook. Default returns `false` (fail-closed). Override to allow specific client-initiated event names. |
| `WsRouteDef::config(WsConfig)` | Attaches a per-route `WsConfig` that overrides process-wide defaults for this route. Composes with `.middleware(M)` in either order. |
| `Router::ws_with_config(path, handler, config)` | Lower-level variant of `ws!` that accepts an explicit `WsConfig`. |
| `Router::ws_with_middleware_and_config(path, handler, middleware, config)` | Lower-level variant that accepts both middleware and an explicit `WsConfig`. |
| `WsMatch::config() -> Option<&WsConfig>` | Returns the per-route `WsConfig` if one was set, or `None` (causing the framework to fall back to `WsConfig::default()`). |
