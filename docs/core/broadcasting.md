---
title: "Broadcasting"
description: "Publish events to WebSocket subscribers through named channels with public, private, and presence semantics, multi-process fanout, and a JSON envelope wire protocol"
icon: "tower-broadcast"
---

# Broadcasting

Broadcasting is the server-to-client notification layer built on top of [the WebSocket primitive](./websockets.md). When you dispatch a `Broadcastable` event through `EventFacade`, it does two things simultaneously: it fires all in-process event listeners as usual, AND it pushes a JSON envelope to every WebSocket client subscribed to the channels the event declares. You do not manage individual connections — you manage channel subscriptions, and the hub fans out to subscribers.

The `BroadcastHub` is the central bus. By default it is an in-process `InMemoryBroadcastHub`, which keeps everything in one process without external dependencies. A `broadcasting-fanout` Cargo feature swaps the hub for a `SeaStreamerBroadcastHub`, which routes events through a stream broker (Redis Streams) so that publishes from one process reach subscribers connected to other processes in a multi-replica deployment.

Broadcasting sits on top of Phase 7A's WebSocket infrastructure. You register a `build_broadcasting_handler()` route the same way you register any WebSocket route; the framework upgrades the HTTP connection, and from that point on the JSON envelope protocol handles channel subscriptions and event delivery. Everything in [the WebSocket docs](./websockets.md) — heartbeat pings, `max_missed_pings`, path parameters, per-route middleware — applies to the broadcasting route as well.

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
use suprnova::broadcasting::{Channel, PrivateChannel};
use suprnova::http::Request;

pub struct PrivateChat;

#[async_trait]
impl Channel for PrivateChat {
    fn name(&self) -> &'static str { "chat.private" }

    async fn authorize(&self, _req: &Request, data: &Value) -> bool {
        data["token"].as_str().map(|t| t == "valid").unwrap_or(false)
    }
}

impl PrivateChannel for PrivateChat {}
```

The `data` value is whatever the client sent in the subscribe frame's `"data"` field — a bearer token, a signed subscription ticket, or any application-defined payload. The `Request` is the original HTTP upgrade request, so you can also read headers or cookies from it.

### Presence Channels

Presence channels track membership: the hub knows which clients are subscribed and what their member information is. When a new client subscribes, the hub delivers a `presence.here` snapshot to that client; when any client joins or leaves, the hub broadcasts `presence.joined` or `presence.left` to all subscribers on the channel.

Implement both `Channel::presence_info` and `PresenceChannel` together — `presence_info` is the hook that tells the hub this channel participates in presence tracking, and `PresenceChannel::member_info` is called at subscribe time to collect the joining member's data:

```rust
use async_trait::async_trait;
use serde_json::{json, Value};
use suprnova::broadcasting::{Channel, PresenceChannel};
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
    async fn member_info(&self, req: &Request) -> Result<Value, FrameworkError> {
        // Return whatever member data your client needs — typically
        // a user ID so clients can identify who is present.
        Ok(json!({ "user_id": 42, "display_name": "Alice" }))
    }
}
```

See [Presence channels](#presence-channels) for the full event semantics.

## The Broadcastable Trait

`Broadcastable` extends [the event system](./events.md). Any event that implements `Broadcastable` is automatically pushed to the hub when dispatched — you call `EventFacade::dispatch` once, and both in-process listeners and WebSocket subscribers receive the event.

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

The event is serialized to JSON using `serde_json` and delivered to every subscriber on every channel returned by `broadcast_on`. Channels that no client has subscribed to are silently skipped.

## The Wire Protocol

All messages over the broadcasting WebSocket route are UTF-8 JSON frames. Two frame shapes are defined: `ClientFrame` (client to server) and `ServerFrame` (server to client).

### Client Frames

| `action` | Required fields | Optional fields | Meaning |
|----------|----------------|-----------------|---------|
| `subscribe` | `channel` | `data` | Subscribe to the named channel. `data` is forwarded to `authorize`. |
| `unsubscribe` | `channel` | | Unsubscribe from the named channel. |
| `publish` | `channel`, `event`, `data` | | Publish an event to all subscribers on the channel. |

```json
{"action":"subscribe","channel":"chat.42","data":{"token":"abc"}}
{"action":"unsubscribe","channel":"chat.42"}
{"action":"publish","channel":"chat.42","event":"MessagePosted","data":{"text":"hi"}}
```

### Server Frames

| `action` | Fields | Meaning |
|----------|--------|---------|
| `subscribed` | `channel` | Subscription accepted. |
| `unsubscribed` | `channel` | Unsubscription confirmed. |
| `event` | `channel`, `event`, `data` | An event was broadcast to this channel. |
| `error` | `channel`, `reason` | The last action on this channel failed. |

```json
{"action":"subscribed","channel":"chat.42"}
{"action":"unsubscribed","channel":"chat.42"}
{"action":"event","channel":"chat.42","event":"MessagePosted","data":{"text":"hi"}}
{"action":"error","channel":"chat.42","reason":"unauthorized"}
```

### Example Session Log

```
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

**v1 scope note.** `list_members` on the in-process hub returns only the members connected to this process. In a multi-replica deployment using the `broadcasting-fanout` feature, members connected to other processes are not visible. Cross-process presence tracking is out of v1 scope; see [Out of v1 scope](#out-of-v1-scope).

## Multi-Process Fanout

By default, the `InMemoryBroadcastHub` fans out only to WebSocket subscribers connected to the current process. For multi-replica deployments, enable the `broadcasting-fanout` Cargo feature and swap the hub for `SeaStreamerBroadcastHub`:

**`Cargo.toml`:**

```toml
suprnova = { version = "...", features = ["broadcasting-fanout"] }
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

`SeaStreamerBroadcastHub` accepts any broker URI supported by sea-streamer. Common forms:

| Backend | URI shape |
|---------|-----------|
| Redis Streams | `redis://host:6379/stream-name` |
| Kafka | `kafka://host:9092/topic-name` |

Published events flow through the broker to every process in the cluster. Each process's local in-memory layer delivers the event to its own WebSocket subscribers. The application code that dispatches events does not change — only the bootstrap wiring changes.

**Presence and multi-process.** In v1, `list_members` returns only the members connected to the local process. Members connected to other replicas are not included. Cross-process presence is explicitly out of v1 scope.

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

The broadcasting WebSocket route is an upgraded HTTP connection on the same hyper listener as your HTTP routes. TLS termination happens upstream, exactly as described in [the WebSocket docs](./websockets.md#production-deployment). The nginx and Caddy configurations from that doc apply unchanged — extend them to cover the `/ws/broadcast` path.

Active WebSocket handler tasks (including broadcasting connections) are tracked in `WS_TASKS` and are drained on graceful shutdown, so in-flight event deliveries complete before the process exits.

## Out of v1 Scope

The following items are intentionally deferred. Each note describes the path forward when it is taken up.

- **Per-channel rate limiting / publish authorization.** Today, applications gate publish actions inside the channel implementation (inspect `data`, return early, etc.). A first-class `authorize_publish` hook on the `Channel` trait is a natural v2 addition.

- **Cross-process presence.** `list_members` is local-only in v1. Cross-process membership requires either gossip between replicas or a shared presence key in the broker; both approaches are feasible with the `broadcasting-fanout` stack and are deferred to a follow-on phase.

- **Cross-region replication.** `SeaStreamerBroadcastHub` is single-cluster. Multi-region fan-out would require a higher-level broker topology or a replication layer above sea-streamer.

- **End-to-end encryption per channel.** Channels today are plaintext from the server's perspective. E2EE (where the server cannot read message content) requires key-exchange support in the wire protocol.

- **Per-route `WsConfig` override.** `max_missed_pings` and `ping_interval` are currently global. Attaching a per-route `WsConfig` via `.config(WsConfig { ... })` on the route entry is scoped to a future phase.

## Reference

| Symbol | Purpose |
|--------|---------|
| `suprnova::broadcasting::Channel` | Trait for named channels. Implement `name()` (required). Override `authorize(&self, req, data) -> bool` for private semantics, or `presence_info()` for presence semantics. |
| `suprnova::broadcasting::PrivateChannel` | Marker trait. Implementing it alongside `Channel` (with a custom `authorize`) makes the channel private. No additional methods. |
| `suprnova::broadcasting::PresenceChannel` | Trait with `async fn member_info(&self, req: &Request) -> Result<Value, FrameworkError>`. Returns the joining member's data used in `presence.joined` / `presence.here` frames. |
| `suprnova::broadcasting::Broadcastable` | Trait with `fn broadcast_on(&self) -> Vec<String>`. Implement on any `Event` to have it pushed to hub subscribers when dispatched via `EventFacade`. |
| `suprnova::broadcasting::BroadcastHub` | Trait implemented by `InMemoryBroadcastHub` and `SeaStreamerBroadcastHub`. The DI container holds the active hub. |
| `suprnova::broadcasting::InMemoryBroadcastHub` | Default hub. In-process fanout only. No external dependencies. |
| `suprnova::broadcasting::fanout::SeaStreamerBroadcastHub` | Multi-process hub behind the `broadcasting-fanout` feature. Accepts a broker URI. |
| `suprnova::broadcasting::ChannelRegistry` | Registry of all known channels. Populated at bootstrap. Resolved from the container by `build_broadcasting_handler()`. |
| `build_broadcasting_handler()` | Returns a `WebSocketHandler` that implements the full JSON envelope protocol. Resolves the hub and registry from the container. Pass directly to `ws!(...)`. |
| `suprnova::ws::WsConfig::max_missed_pings` | `u32` (default 2). Number of consecutive missed Pongs before the heartbeat closes the connection with code 1011. |
| `ws!(path, build_broadcasting_handler()).middleware(M)` | Route macro syntax. Chains middleware onto the broadcasting route. A non-2xx middleware response short-circuits the upgrade. |
