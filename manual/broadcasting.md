# Broadcasting

Broadcasting is the server-to-client notification layer on top of
Suprnova's [WebSocket primitive](websockets.md). You dispatch a
`Broadcastable` event through `EventFacade`; the framework fans the
event's JSON envelope out to every WebSocket subscriber on the channels
the event names. You never manage individual connections — you manage
channel subscriptions, and the hub does the rest.

The `BroadcastHub` is the bus. The default `InMemoryBroadcastHub` runs
entirely in-process — perfect for single-replica deployments and the
test suite. Behind the `broadcasting-fanout` Cargo feature,
`SeaStreamerBroadcastHub` routes the same events through a stream
broker (Redis Streams, Kafka, file, stdio) so a publish in one process
reaches subscribers in every other process.

Everything from the [WebSocket](websockets.md) chapter still applies —
heartbeat pings, `max_missed_pings`, `WsConfig`, per-route middleware,
path parameters. Broadcasting just adds a wire protocol and a channel
registry on top.

## Quick start

Four files and the browser sees an event.

`src/channels/order_updates.rs`:

```rust
use async_trait::async_trait;
use suprnova::broadcasting::Channel;

pub struct OrderUpdates;

#[async_trait]
impl Channel for OrderUpdates {
    fn name(&self) -> &'static str { "order.updates" }
}
```

`src/events/order_placed.rs`:

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

`src/bootstrap.rs`:

```rust
use std::sync::Arc;
use suprnova::broadcasting::{BroadcastHub, ChannelRegistry, InMemoryBroadcastHub};
use suprnova::container::App;
use suprnova::events::EventFacade;

pub async fn register() {
    // 1. Bind the hub behind the trait — handlers resolve it uniformly.
    let hub: Arc<dyn BroadcastHub> = Arc::new(InMemoryBroadcastHub::new());
    App::bind::<dyn BroadcastHub>(Arc::clone(&hub));

    // 2. Register every channel up front; the WS handler resolves by name.
    let mut registry = ChannelRegistry::new();
    registry.register(OrderUpdates);
    App::singleton(Arc::new(registry));

    // 3. Wire the event → hub bridge once per Broadcastable type.
    EventFacade::broadcast::<OrderPlaced>(Arc::clone(&hub)).await;
}
```

`src/routes.rs` — build a `BroadcastingWsHandler` per route by
resolving the bootstrapped hub and registry from the container:

```rust
use std::sync::Arc;
use suprnova::broadcasting::{
    BroadcastHub, BroadcastingWsHandler, ChannelRegistry, InMemoryBroadcastHub,
};
use suprnova::container::App;
use suprnova::{routes, ws, AuthMiddleware};

fn broadcasting_handler() -> BroadcastingWsHandler {
    // Container-first; fall back to a fresh in-process hub + empty registry
    // so unit tests that assemble the router without bootstrap still work.
    let hub: Arc<dyn BroadcastHub> = App::make::<dyn BroadcastHub>()
        .unwrap_or_else(|| Arc::new(InMemoryBroadcastHub::new()));
    let registry: Arc<ChannelRegistry> = App::get::<Arc<ChannelRegistry>>()
        .unwrap_or_else(|| Arc::new(ChannelRegistry::new()));
    BroadcastingWsHandler::new(hub, registry)
}

routes! {
    ws!("/ws/broadcast", broadcasting_handler())
        .middleware(AuthMiddleware::new()),
}
```

Connect and observe:

```bash
wscat -c ws://localhost:3000/ws/broadcast
> {"action":"connected","socket_id":"6f1a3c2e-…"}
> {"action":"subscribe","channel":"order.updates","data":{}}
< {"action":"subscribed","channel":"order.updates"}
```

Dispatch from any controller, worker, or scheduled task:

```rust
EventFacade::dispatch(OrderPlaced { order_id: 99, user_id: 42 }).await?;
```

```
< {"action":"event","channel":"order.updates","event":"OrderPlaced","data":{"order_id":99,"user_id":42}}
```

## Channels

A channel is a named subscription target. Clients subscribe by name; the
hub delivers events to every active subscriber on that name. The `Channel`
trait has asymmetric defaults that fail closed on writes and open on
reads — see [Why Suprnova diverges](#why-suprnova-diverges) below.

### Public channels

The default. Any client may subscribe.

```rust
use async_trait::async_trait;
use suprnova::broadcasting::Channel;

pub struct OrderUpdates;

#[async_trait]
impl Channel for OrderUpdates {
    fn name(&self) -> &'static str { "order.updates" }
    // authorize() defaults to true — open to all subscribers.
}
```

### Private channels

Override `authorize` to gate subscriptions. A rejected subscribe
produces an `error` frame with `reason: "unauthorized"`; no
`subscribed` frame is sent.

```rust
use async_trait::async_trait;
use serde_json::Value;
use suprnova::broadcasting::{Channel, ChannelParams, PrivateChannel};
use suprnova::http::Request;

pub struct PrivateChat;

#[async_trait]
impl Channel for PrivateChat {
    fn name(&self) -> &'static str { "chat.private" }

    async fn authorize(
        &self,
        _req: &Request,
        _params: &ChannelParams,
        data: &Value,
    ) -> bool {
        data["token"].as_str().map(|t| t == "valid").unwrap_or(false)
    }
}

impl PrivateChannel for PrivateChat {}
```

`data` is whatever the client sent in the subscribe frame's `data`
field — a bearer token, a signed channel-bind, anything
application-defined. `Request` is the original HTTP upgrade request
(headers and cookies are readable directly). `params` carries the
captured values from a parameterized name and is empty for fixed
names.

`PrivateChannel` is a marker trait. The framework does not check for
it at runtime — it is a type-level signal that the channel overrides
`authorize` and is intended for future tooling (a clippy lint, an
audit pass).

### Parameterized channels

Embed `{param}` segments in `name()` and one registration serves every
concrete subscription that matches the pattern — the same model as
Laravel's `Broadcast::channel('orders.{id}', …)`. Captured values reach
every hook as a `ChannelParams` map.

```rust
use async_trait::async_trait;
use serde_json::Value;
use suprnova::broadcasting::{Channel, ChannelParams, PrivateChannel};
use suprnova::http::Request;

pub struct OrderChannel;

#[async_trait]
impl Channel for OrderChannel {
    fn name(&self) -> &'static str { "orders.{id}" }

    async fn authorize(
        &self,
        _req: &Request,
        params: &ChannelParams,
        _data: &Value,
    ) -> bool {
        let order_id = params.get("id").unwrap_or_default();
        // Gate on the captured id — does the session user own this order?
        !order_id.is_empty()
    }
}

impl PrivateChannel for OrderChannel {}

// One registration serves orders.42, orders.99, orders.featured, …
registry.register(OrderChannel);
```

Each `{param}` binds exactly one dot-segment: `orders.{id}` matches
`orders.42` but not `orders` or `orders.42.line`. Resolution prefers an
exact fixed-name registration over any pattern (`orders.featured`
beats `orders.{id}` for that one name), then the most specific
pattern (most literal segments), with the lexicographically smallest
pattern as a deterministic tie-break.

### Presence channels

Presence channels track membership. When a client subscribes, the hub
delivers a `presence.here` snapshot to that client and broadcasts
`presence.joined` to every other subscriber. When a client leaves,
the hub broadcasts `presence.left`.

The two-part contract is easy to half-implement: you must both
override `Channel::presence_info` to return `Some(self)` AND
implement `PresenceChannel::member_info`. Forgetting `presence_info`
wires the channel as non-presence — subscribes work, but
`presence.joined` / `presence.here` / `presence.left` never fire.

```rust
use async_trait::async_trait;
use serde_json::{json, Value};
use suprnova::FrameworkError;
use suprnova::broadcasting::{Channel, ChannelParams, PresenceChannel};
use suprnova::http::Request;

pub struct PresenceLobby;

#[async_trait]
impl Channel for PresenceLobby {
    fn name(&self) -> &'static str { "presence.lobby" }

    // Required — without this override, PresenceChannel is wired but inert.
    fn presence_info(&self) -> Option<&dyn PresenceChannel> {
        Some(self)
    }
}

#[async_trait]
impl PresenceChannel for PresenceLobby {
    async fn member_info(
        &self,
        _req: &Request,
        _params: &ChannelParams,
    ) -> Result<Value, FrameworkError> {
        // Return what other subscribers need to identify this member —
        // typically a user id. Never include secrets or private PII.
        Ok(json!({ "user_id": 42, "display_name": "Alice" }))
    }
}
```

See [Presence](#presence) for the full event flow and the self-join
echo.

### Reserved names

Names starting with `__` are reserved for framework meta-channels
(`__presence__` carries cross-process presence replication). Calling
`registry.register(channel)` on a `__`-prefixed name panics at
registration so the mistake is caught at boot, not at runtime.

### Why Suprnova diverges

Laravel binds channel authorization to a `$user` callback parameter
because PHP injects the current authenticated user implicitly.
Suprnova's `authorize` instead takes the raw `Request`, the captured
`ChannelParams`, and an arbitrary `data: Value` — three orthogonal
inputs, all available, with no implicit context. You read the session
cookie or bearer token from `Request` and the routing-style params
from `ChannelParams`; the `data` payload is a free slot for tokens
the client provides at subscribe time.

The `Channel` trait's defaults are **asymmetric on purpose**:
`authorize` defaults to `true` (subscribe is public by default),
`authorize_publish` defaults to `false` (client-initiated publish is
denied by default). The dangerous action fails closed; the safe one
fails open. When in doubt, leave both alone.

## The Broadcastable trait

`Broadcastable: Event + Serialize` — every `Broadcastable` is also an
`Event`. Dispatch via `EventFacade::dispatch(event)` runs every
in-process listener AND pushes the JSON-serialized payload to every
WebSocket subscriber on the channels the event names.

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
        // One event, multiple channels. Each subscriber on each channel
        // receives the same envelope.
        vec![
            format!("user.{}.orders", self.user_id),
            "orders.global".into(),
        ]
    }
}
```

Wire the bridge once per Broadcastable type at boot:

```rust
EventFacade::broadcast::<OrderPlaced>(Arc::clone(&hub)).await;
```

After that, `EventFacade::dispatch(event).await?` is the entire send
side — no separate `publish` call.

By default the event is serialized via `serde_json::to_value(&event)`
and pushed to every subscriber. Channels with zero subscribers are
silently skipped on the in-process hub; the cross-process hub still
publishes them so other processes get a chance to deliver.

Four optional methods refine the default:

**`broadcast_event_name(&self) -> &'static str`** — override the wire
event name. Defaults to `Self::event_name()`. Use to decouple the
in-process event identity from the over-the-wire name.

**`broadcast_with(&self) -> Option<Value>`** — return `Some(value)` to
push a curated payload instead of the full event serialization
(Laravel's `broadcastWith()`). Omit secrets or reshape for the client
without changing the event type:

```rust
impl Broadcastable for AccountFunded {
    fn broadcast_on(&self) -> Vec<String> {
        vec![format!("account.{}", self.account_id)]
    }
    fn broadcast_with(&self) -> Option<serde_json::Value> {
        // Never put the balance on the wire — only the public id.
        Some(serde_json::json!({ "account_id": self.account_id }))
    }
}
```

**`broadcast_when(&self) -> bool`** — return `false` to dispatch the
event to in-process listeners but skip the WebSocket push (Laravel's
`broadcastWhen()`). Only the broadcast is gated; the rest of the
event pipeline runs unchanged:

```rust
impl Broadcastable for DraftSaved {
    fn broadcast_on(&self) -> Vec<String> { vec![format!("doc.{}", self.doc_id)] }
    fn broadcast_when(&self) -> bool { self.publish } // only broadcast on publish
}
```

**`broadcast_to_others(&self) -> bool`** — return `true` to exclude
the connection that triggered the broadcast (Laravel's `toOthers()`).
The framework assigns each broadcasting connection a `socket_id` on
connect (sent in the `connected` frame); the browser echoes it back
as the `X-Socket-ID` header on HTTP requests; a `broadcast_to_others`
event dispatched while handling that request skips the originating
connection. Off-request (a worker or job) or when no `X-Socket-ID`
is present, it degrades to broadcasting to everyone:

```rust
impl Broadcastable for MessagePosted {
    fn broadcast_on(&self) -> Vec<String> { vec![format!("chat.{}", self.room)] }
    fn broadcast_to_others(&self) -> bool { true } // the sender already has it
}
```

This is a per-event-type choice. For per-dispatch exclusion, publish
directly:

```rust
use suprnova::broadcasting::BroadcastEnvelope;

hub.publish(
    BroadcastEnvelope::new(channel, event, data).with_except(socket_id),
).await?;
```

### Dispatch ordering with sibling listeners

`EventFacade::dispatch` is **fail-fast**: if a hub publish returns
`Err` (e.g. a broker disconnect on a cross-process hub), the
`BroadcastListener` returns `Err` and any sibling listeners registered
**after** it do not run. Two ways to handle this:

- Register the broadcast bridge AFTER in-process listeners whose side
  effects (DB writes, log emission) must run regardless of broadcast
  outcome.
- Switch to `EventFacade::dispatch_best_effort(event)` when every
  listener must run regardless of one returning `Err`.

In-memory hubs never return `Err` — only the cross-process variant
surfaces broker failures.

## The wire protocol

Every message over the broadcasting route is a UTF-8 JSON frame. Two
shapes: `ClientFrame` (client → server) and `ServerFrame` (server →
client).

### Client frames

| `action` | Required fields | Optional fields | Meaning |
|----------|-----------------|-----------------|---------|
| `subscribe` | `channel` | `data` | Subscribe to `channel`. `data` is forwarded to `Channel::authorize`. |
| `unsubscribe` | `channel` | | Detach from `channel`. |
| `publish` | `channel`, `event`, `data` | | Push an event to every subscriber on `channel`. Gated by `Channel::authorize_publish` AND requires a live subscription. |

Client-initiated `publish` is gated by **two** checks: the connection
MUST hold an authorized subscription to the target channel, AND
`Channel::authorize_publish` must return `true` (it defaults to
`false`). This mirrors the Pusher client-event contract — channels
that want client publishes opt in explicitly by overriding the hook.
Most server-side broadcasting channels never want client-initiated
events, and the default-deny shape matches that intent.

```json
{"action":"subscribe","channel":"chat.42","data":{"token":"abc"}}
{"action":"unsubscribe","channel":"chat.42"}
{"action":"publish","channel":"chat.42","event":"MessagePosted","data":{"text":"hi"}}
```

### Server frames

| `action` | Fields | Meaning |
|----------|--------|---------|
| `connected` | `socket_id` | Sent once, first. Echo `socket_id` as the `X-Socket-ID` HTTP header so server-side `broadcast_to_others` can exclude this connection. |
| `subscribed` | `channel` | Subscription accepted. |
| `unsubscribed` | `channel` | Unsubscription confirmed. |
| `event` | `channel`, `event`, `data` | An event was broadcast on `channel`. |
| `lagged` | `channel`, `skipped` | The subscriber fell behind the server's per-channel ring buffer and `skipped` envelopes were dropped on this connection. Client local state on `channel` is stale; refetch before processing further events. |
| `error` | `channel` (nullable), `reason` | The last action failed. `channel` is `null` for envelope-level errors not tied to a channel. |

```json
{"action":"connected","socket_id":"6f1a3c2e-…"}
{"action":"subscribed","channel":"chat.42"}
{"action":"unsubscribed","channel":"chat.42"}
{"action":"event","channel":"chat.42","event":"MessagePosted","data":{"text":"hi"}}
{"action":"lagged","channel":"chat.42","skipped":42}
{"action":"error","channel":"chat.42","reason":"unauthorized"}
{"action":"error","channel":null,"reason":"malformed envelope: …"}
```

#### About `lagged`

Every channel has a per-process ring buffer (256 envelopes). A
subscriber that doesn't drain fast enough — a slow client, a stuck
forwarder — falls behind, and the buffer overwrites the oldest
events. When that happens, the server sends one `lagged` frame
naming the channel and the count of dropped events, then continues
delivering subsequent frames normally. The gap is **not** recoverable
from the server side; the client must refetch or resync before
processing further events on that channel. Silently dropping events
would let bugs hide as "we lost a tick" rather than "the client's
state diverged from the server's".

#### Publish failures

When a client-initiated `publish` is accepted by `authorize_publish`
but the hub publish itself fails (broker disconnect on the
cross-process hub), the originating client receives an `error` frame
with `reason: "publish failed: …"` so it knows the event didn't
reach other processes. Other subscribers are not notified.

### Example session

```
S → C  {"action":"connected","socket_id":"6f1a3c2e-…"}
C → S  {"action":"subscribe","channel":"order.updates","data":{}}
S → C  {"action":"subscribed","channel":"order.updates"}

# Server dispatches OrderPlaced:
S → C  {"action":"event","channel":"order.updates","event":"OrderPlaced","data":{"order_id":99,"user_id":42}}

C → S  {"action":"subscribe","channel":"chat.private","data":{"token":"bad"}}
S → C  {"action":"error","channel":"chat.private","reason":"unauthorized"}

C → S  {"action":"unsubscribe","channel":"order.updates"}
S → C  {"action":"unsubscribed","channel":"order.updates"}
```

## Per-route middleware

Broadcasting routes support the same `.middleware(M)` chaining as plain
WebSocket routes:

```rust
ws!("/ws/broadcast", broadcasting_handler())
    .middleware(AuthMiddleware::new()),
```

A non-2xx response from any middleware short-circuits the upgrade —
the client receives the HTTP error response and no WebSocket
handshake happens. This is the right place to enforce transport-level
auth (session validity, origin checks, rate limits at connection
time) without duplicating the check inside every channel's
`authorize`.

Multiple middleware compose left-to-right:

```rust
ws!("/ws/broadcast", broadcasting_handler())
    .middleware(AuthMiddleware::new())
    .middleware(RateLimitMiddleware::connections_per_ip(100)),
```

The split is intentional: **transport-level** (who may open the
connection at all) lives in middleware; **channel-level** (who may
subscribe to which channel) lives in `Channel::authorize`.

### Per-route `WsConfig`

Override the process-wide WebSocket defaults per route. Chain
`.config(WsConfig { ... })` after the handler — before or after
`.middleware(M)` (order doesn't matter):

```rust
use std::time::Duration;
use suprnova::ws::WsConfig;

ws!("/ws/chat", broadcasting_handler())
    .config(WsConfig {
        ping_interval: Duration::from_secs(5),
        max_missed_pings: 1,
        ..Default::default()
    })
    .middleware(AuthMiddleware::new())
```

The five configurable fields and where each one matters:

| Field | Default | Use case |
|-------|---------|----------|
| `ping_interval` | 30s | Chat / presence: shorten to 5–10s to detect dead mobile connections quickly. Bulk-data streaming: lengthen to reduce overhead. |
| `max_missed_pings` | 2 | Set to `1` for chat where one missed Pong should close immediately. Set to `3+` for flaky mobile networks. Set to `usize::MAX` to disable close-on-no-pong. |
| `max_message_size` | 1 MiB | Public-endpoint-safe default. Start from `WsConfig::generous()` (64 MiB) for trusted internal feeds. |
| `max_frame_size` | 64 KiB | Sized for chat / notification frames with headroom. Start from `WsConfig::generous()` (16 MiB) for large unfragmented frames. |
| `origin_policy` | `SameOrigin` | Defaults reject cross-origin upgrades — the only CSRF protection a browser WS handshake has. Use `AllowList(vec![...])` for explicit cross-origin frontends, or `AllowAny` only for non-browser endpoints. |

When no `.config(...)` is provided, the route inherits
`WsConfig::default()`. Explicit per-route config always wins over the
default.

For routes serving trusted internal feeds (server-to-server fanout,
large binary transfers), start from the trusted-feed factory and
adjust as needed:

```rust
use suprnova::ws::WsConfig;
use std::time::Duration;

ws!("/ws/internal/firehose", FirehoseHandler::new())
    .config(WsConfig {
        ping_interval: Duration::from_secs(10),
        ..WsConfig::generous() // 64 MiB message / 16 MiB frame
    })
```

## Presence

When a client successfully subscribes to a presence channel the hub:

1. Calls `PresenceChannel::member_info` with the upgrade `Request` and
   the captured `ChannelParams` to collect the joining member's data.
2. Sends a `presence.here` event frame to the new subscriber with
   `data: { "members": [...] }` — a snapshot of all currently tracked
   members (excluding the newly joining one).
3. Publishes a `presence.joined` event with `data: <member_info>` to
   the channel. Every subscriber — including the new one via its own
   forwarder — receives it; clients filter the self-join by comparing
   the joining member's identity to their own.

When a subscriber disconnects or sends an unsubscribe frame:

4. The hub publishes a `presence.left` event with the departing
   member's data. Every remaining subscriber receives it.

All three frames arrive as `event` action frames with reserved
`event` names:

```json
{"action":"event","channel":"presence.lobby","event":"presence.here","data":{"members":[{"user_id":1},{"user_id":2}]}}
{"action":"event","channel":"presence.lobby","event":"presence.joined","data":{"user_id":3}}
{"action":"event","channel":"presence.lobby","event":"presence.left","data":{"user_id":3}}
```

Across processes, presence state is replicated via the reserved
`__presence__` meta-channel (see [Cross-process
fanout](#cross-process-fanout)). Track and untrack operations on any
process propagate to all subscribers; `list_members` returns the
merged view (local + remote). Dead processes whose `untrack_member`
never fired have their members pruned via TTL — default 60 s.

## Cross-process fanout

The default `InMemoryBroadcastHub` fans out only to subscribers on the
current process. For multi-replica deployments, enable the
`broadcasting-fanout` Cargo feature and swap in
`SeaStreamerBroadcastHub`:

`Cargo.toml`:

```toml
suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git", features = ["broadcasting-fanout"] }
```

`src/bootstrap.rs`:

```rust
use std::sync::Arc;
use suprnova::broadcasting::{BroadcastHub, ChannelRegistry};
use suprnova::broadcasting::fanout::SeaStreamerBroadcastHub;
use suprnova::container::App;

pub async fn register() {
    let hub: Arc<dyn BroadcastHub> = Arc::new(
        SeaStreamerBroadcastHub::new(
            "redis://broker:6379",   // streamer URI (backend chosen from scheme)
            "suprnova-broadcast",    // stream key (shared by every process in the cluster)
        )
        .await
        .expect("connect"),
    );
    App::bind::<dyn BroadcastHub>(Arc::clone(&hub));
    // ... rest of bootstrap unchanged
}
```

The constructor takes two arguments: the streamer URI (selects the
backend at runtime by scheme) and the stream key (the topic name
shared by every process in the cluster). Use the same stream key on
every replica or they won't see each other's events.

`new_with_presence_ttl(uri, key, ttl)` overrides the default 60 s
presence TTL — useful for tests that need to exercise the
crash-recovery path quickly. `new_loopback(uri, key)` enables stdio
loopback for single-process integration tests; the duplicate guard
ensures each app event still delivers exactly once locally.

### Backends

The backend is selected at runtime from the URI scheme:

| URI scheme | Backend | Production-ready | Notes |
|------------|---------|------------------|-------|
| `redis://`, `rediss://` | Redis Streams | **Yes** | Default recommendation. `rediss://` uses TLS. Enabled in the default build. |
| `kafka://`, `kafka+ssl://` | Kafka | **Yes** | Requires `kafka` in the `sea-streamer` feature set (`framework/Cargo.toml`). |
| `stdio://` | stdin/stdout pipes | No — tests only | Single-process loopback. |
| `file://` | Local file | No — single-host | Requires `file` in the `sea-streamer` feature set. |

The default Suprnova build enables `stdio` + `redis` + `socket`. To
enable Kafka or file, edit `framework/Cargo.toml` and add the
relevant `sea-streamer` feature.

### Architecture

Each `publish(envelope)` does two things in parallel:

1. **Local fanout** — the inner `InMemoryBroadcastHub` delivers to
   subscribers on this process immediately. Local subscribers never
   wait on the network.
2. **Stream write** — the same envelope is serialized and pushed to
   the sea-streamer stream so every other process's consumer pump
   picks it up and delivers it locally.

A duplicate-delivery guard prevents seeing each app-data event twice:
the hub instance has a random UUID, every envelope it produces carries
that UUID, and the consumer pump skips inbound envelopes whose
instance id matches the local hub's own. Presence meta-channel
messages are an exception — each hub needs its own events in the
cross-process view so the read path is unified.

Backend dispatch is enum-based, not trait-object: the hub stores a
concrete `SeaProducer` / `SeaConsumer` from sea-streamer's socket
adapter, which is an enum over every compiled backend. No `dyn`
overhead at the publish call site.

### Cross-process presence

`SeaStreamerBroadcastHub` replicates presence state across processes
automatically. Each instance has a UUID `instance_id` at
construction; `track_member` / `untrack_member` publish
`PresenceEvent`s to the reserved `__presence__` meta-channel. Every
process maintains a `cross_process_view` updated by its consumer
task; `list_members` returns the merged view (local and remote
uniformly).

Liveness: each process re-publishes its members every `ttl / 6` (10 s
at the default 60 s TTL) as a heartbeat. Stale entries — members
whose `last_seen` exceeds the TTL — get pruned every `ttl / 2`. This
handles process crashes that didn't get to publish
`MemberRemoved`.

## Close-on-no-pong

Broadcasting routes participate in the same WebSocket heartbeat as
plain `ws!` routes. The framework sends a Ping every
`WsConfig::ping_interval` (default 30 s). If a connection fails to
respond with a Pong within `max_missed_pings` consecutive intervals
(default 2), the framework closes with code 1011.

```rust
use std::time::Duration;
use suprnova::ws::WsConfig;

let config = WsConfig {
    ping_interval: Duration::from_secs(15),
    max_missed_pings: 3,
    ..WsConfig::default()
};
```

Lowering `ping_interval` detects dead connections faster at the cost
of higher baseline traffic. `max_missed_pings: 1` closes after the
very first missed Pong — use this only when network glitches are
rare and you want the fastest possible dead-connection cleanup.
`max_missed_pings: usize::MAX` disables close-on-no-pong entirely.

## Production deployment

Broadcasting routes are upgraded HTTP connections on the same hyper
listener as your HTTP routes. TLS termination happens upstream,
exactly as described in [the WebSocket
chapter](websockets.md#production-deployment). The nginx and Caddy
configurations from that chapter apply unchanged — extend them to
cover the `/ws/broadcast` path.

Active WebSocket handler tasks (including broadcasting connections)
are tracked in the framework's `WS_TASKS` set and drained on
graceful shutdown, so in-flight event deliveries complete before the
process exits.

## Testing broadcasts

`RecordingBroadcastHub` is the Suprnova analogue of Laravel's
`Broadcast::fake()` — a `BroadcastHub` that records every published
envelope while still delivering to live subscribers. Bind it in
place of `InMemoryBroadcastHub` in a test and assert what was
broadcast without subscribing first:

```rust
use std::sync::Arc;
use suprnova::broadcasting::{BroadcastHub, RecordingBroadcastHub};
use suprnova::container::App;

#[tokio::test]
async fn shipping_an_order_broadcasts_to_the_user_channel() {
    let hub = Arc::new(RecordingBroadcastHub::new());
    App::bind::<dyn BroadcastHub>(Arc::clone(&hub) as Arc<dyn BroadcastHub>);

    // ... run code that publishes (directly, or via a dispatched Broadcastable) ...

    hub.assert_broadcast("orders.42", "OrderShipped");
    assert_eq!(hub.count(), 1);
}
```

| Helper                         | Asserts                                                  |
|--------------------------------|----------------------------------------------------------|
| `assert_broadcast(ch, ev)`     | at least one envelope on `ch` with event name `ev`       |
| `assert_nothing_broadcast()`   | nothing was published                                    |
| `broadcasts()`                 | `Vec<BroadcastEnvelope>` — every recorded envelope       |
| `count()`                      | total envelopes recorded                                 |

To assert that a `Broadcastable` *event* was dispatched at all
(rather than what reached the wire), `EventFacade::fake()` records
the event itself — see [Events](events.md#testing--eventfacadefake).

## Laravel parity reference

| Laravel | Suprnova |
|---------|----------|
| `Broadcast::channel('name', fn(...))` | `Channel` trait impl + `registry.register(...)` |
| `Broadcast::channel('orders.{id}', ...)` | `fn name() -> "orders.{id}"`, params in `ChannelParams` |
| `PrivateChannel` (interface) | `PrivateChannel` marker trait + override `authorize` |
| `PresenceChannel` (interface) | `PresenceChannel` + override `Channel::presence_info` |
| `ShouldBroadcast` (interface) | `Broadcastable` trait |
| `broadcastOn()` | `broadcast_on(&self) -> Vec<String>` |
| `broadcastAs()` | `broadcast_event_name(&self) -> &'static str` |
| `broadcastWith()` | `broadcast_with(&self) -> Option<Value>` |
| `broadcastWhen()` | `broadcast_when(&self) -> bool` |
| `toOthers()` | `broadcast_to_others(&self) -> bool` |
| `Broadcast::fake()` | `RecordingBroadcastHub` bound as `dyn BroadcastHub` |
| `assertBroadcasted` | `RecordingBroadcastHub::assert_broadcast(channel, event)` |
| Pusher / Reverb / Ably driver | `InMemoryBroadcastHub` (single-process) or `SeaStreamerBroadcastHub` (cross-process: Redis / Kafka / file / stdio) |
| Echo client library | not shipped — wire the JSON envelope protocol from the browser by hand for now |

## Reference

| Symbol | Purpose |
|--------|---------|
| `suprnova::broadcasting::Channel` | Channel trait. Override `name()` (required), `authorize`, `authorize_publish`, `presence_info`. |
| `suprnova::broadcasting::ChannelParams` | Captured values from a parameterized `name()`. `get(key) -> Option<&str>`. Empty for fixed names. |
| `suprnova::broadcasting::PrivateChannel` | Marker trait on a `Channel` that overrides `authorize`. No required methods. |
| `suprnova::broadcasting::PresenceChannel` | `async fn member_info(req, params) -> Result<Value, FrameworkError>`. Requires `Channel::presence_info` override. |
| `suprnova::broadcasting::ChannelRegistry` | Holds every registered channel. Bound as `Arc<ChannelRegistry>` in the container; resolved by `BroadcastingWsHandler`. |
| `suprnova::broadcasting::Broadcastable` | Trait on `Event + Serialize`. Required: `broadcast_on()`. Optional: `broadcast_event_name`, `broadcast_with`, `broadcast_when`, `broadcast_to_others`. |
| `suprnova::broadcasting::BroadcastHub` | Hub trait. `subscribe`, `publish`, `subscriber_count`, presence track/untrack/list. |
| `suprnova::broadcasting::InMemoryBroadcastHub` | Default in-process hub. No external dependencies. Publish returns `Ok` unconditionally. |
| `suprnova::broadcasting::RecordingBroadcastHub` | Test double. Records every publish; still delivers to live subscribers. |
| `suprnova::broadcasting::BroadcastEnvelope` | One published event: `channel`, `event`, `data`, `except`. `new(ch, ev, data)` builder; `.with_except(socket_id)` for per-dispatch exclusion. |
| `suprnova::broadcasting::ClientFrame` / `ServerFrame` | The JSON-envelope wire types. `ServerFrame::Lagged { channel, skipped }` surfaces per-channel ring-buffer overflows. |
| `suprnova::broadcasting::BroadcastingWsHandler` | The framework's reusable `WebSocketHandler`. Constructor: `BroadcastingWsHandler::new(hub, registry)`. Pass to `ws!()`. |
| `suprnova::broadcasting::fanout::SeaStreamerBroadcastHub` | Cross-process hub behind `broadcasting-fanout`. `new(uri, stream_key)`, `new_with_presence_ttl(uri, key, ttl)`, `new_loopback(uri, key)`. |
| `EventFacade::broadcast::<E>(hub)` | Register the event → hub bridge for `E`. Call once per `Broadcastable` at boot. |
| `EventFacade::dispatch(event)` | Fires in-process listeners AND publishes to the hub on every channel `E::broadcast_on()` returns. |
| `WsRouteDef::config(WsConfig)` | Per-route WS config override. Composes with `.middleware(M)` in either order. |
| `WsRouteDef::middleware(M)` | Per-route middleware chain. A non-2xx response short-circuits the upgrade. |
| `WsConfig::generous()` | Trusted-feed factory: 64 MiB message / 16 MiB frame, other fields unchanged. Do NOT use on public routes. |

## Next

- [WebSockets](websockets.md) — the underlying primitive, `WsSocket`, `OriginPolicy`
- [Events](events.md) — `EventFacade`, fail-fast vs best-effort dispatch
- [Server-Sent Events](sse.md) — one-way push without an Upgrade handshake
- [Notifications](notifications.md) — the `BroadcastChannel` notification driver
- [Web Push](web-push.md) — server-pushed notifications to offline users
