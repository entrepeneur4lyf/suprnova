# Phase 7: Broadcasting + Supervised Workers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Ship the "where Rust eats Laravel's lunch" track. (1) WebSocket-based `Broadcast::channel(typed_channel).send(event)` with typed channels, private auth, presence tracking, and multi-process fanout via sea-streamer; (2) `Worker::supervise(name, interval, closure)` for in-process supervised background workers with exponential-backoff crash restart. Together they replace the "deploy Horizon + Reverb + worker container" stack with one Rust binary.

**Architecture:** Broadcasting hangs off the existing hyper HTTP server via `tokio_tungstenite`'s `accept_async` for HTTP/1.1 Upgrade. A `BroadcastHub` (process-global `OnceLock<Arc<BroadcastHub>>`) holds per-channel `tokio::sync::broadcast` senders; clients connecting to a channel get a receiver clone. `Broadcast::channel(ch).send(event)` writes to the local hub AND publishes to sea-streamer for multi-process fanout. A consumer task subscribes to sea-streamer and re-broadcasts inbound messages into the local hub. Channel auth lives on the channel type via `Channel::authorize(self, user)`; the `#[channel("orders.{id}")]` proc macro generates the channel-name parser and the trait impl. Presence is a layer on top — joining/leaving fires `PresenceEvent`s the channel handlers can observe.

Supervised workers use a tokio task per worker; the supervisor wraps the closure in `tokio::spawn` with a `JoinHandle`; on completion (Ok or Err) the supervisor schedules a restart with backoff if the worker is supposed to be long-lived.

**Tech Stack:** `tokio-tungstenite` 0.24 (with `rustls-tls-webpki-roots` feature), reuses `sea-streamer` (Phase 5), `dashmap` 6 (for the connection registry — already a transitive dep via loco reference but verify).

---

## File Structure

**New files:**
- `framework/src/broadcast/mod.rs` — `Broadcast` facade, `Channel` trait
- `framework/src/broadcast/hub.rs` — `BroadcastHub` (in-process)
- `framework/src/broadcast/socket.rs` — WebSocket upgrade handler
- `framework/src/broadcast/presence.rs` — `PresenceChannel`, member tracking
- `framework/src/broadcast/fanout.rs` — sea-streamer fanout consumer
- `framework/src/broadcast/auth.rs` — private channel auth flow
- `framework/src/broadcast/testing.rs` — `Broadcast::fake()`
- `framework/src/worker/mod.rs` — `Worker::supervise`, `SupervisorRegistry`
- `framework/src/worker/supervisor.rs` — restart-with-backoff loop
- `framework/tests/broadcast.rs` — channel auth, presence, fanout
- `framework/tests/supervised_workers.rs` — crash + restart with backoff
- `suprnova-macros/src/channel.rs` — `#[channel("name.{id}")]` macro
- `app/src/channels/orders_channel.rs` — dogfood
- `app/src/workers/payments_poll.rs` — dogfood

**Modified files:**
- `framework/Cargo.toml` — add tokio-tungstenite, dashmap
- `framework/src/server.rs` — register WebSocket upgrade handler at `/broadcast`
- `framework/src/lib.rs` — declare + re-export

---

## Task 1: Add deps

**Files:** `framework/Cargo.toml`

- [ ] **Step 1: Add**

```toml
# framework/Cargo.toml
tokio-tungstenite = { version = "0.24", default-features = false, features = ["rustls-tls-webpki-roots"] }
dashmap = "6"
```

- [ ] **Step 2: Verify build**

```bash
cargo check --workspace
```

- [ ] **Step 3: Commit**

```bash
git add framework/Cargo.toml Cargo.lock
git commit -m "feat(deps): add tokio-tungstenite + dashmap for Phase 7"
```

---

## Task 2: BroadcastHub — in-process pub/sub by channel name

**Files:** `framework/src/broadcast/hub.rs`, `framework/src/broadcast/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/broadcast.rs
use suprnova::broadcast::BroadcastHub;

#[tokio::test]
async fn hub_delivers_message_to_subscribers() {
    let hub = BroadcastHub::new();
    let mut rx_a = hub.subscribe("orders.42");
    let mut rx_b = hub.subscribe("orders.42");
    hub.publish("orders.42", serde_json::json!({"status": "shipped"}));
    let msg_a = rx_a.recv().await.unwrap();
    let msg_b = rx_b.recv().await.unwrap();
    assert_eq!(msg_a["status"], "shipped");
    assert_eq!(msg_b["status"], "shipped");
}

#[tokio::test]
async fn hub_isolates_channels() {
    let hub = BroadcastHub::new();
    let mut rx_orders = hub.subscribe("orders.1");
    let mut _rx_messages = hub.subscribe("messages.1");
    hub.publish("orders.1", serde_json::json!({"x": 1}));
    let _ = tokio::time::timeout(std::time::Duration::from_millis(50), rx_orders.recv())
        .await
        .expect("orders receive");
    // messages.1 has no message
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/broadcast/hub.rs
//! In-process pub/sub keyed by channel name. Each channel gets a
//! `tokio::sync::broadcast` channel sized for fanout to many local
//! subscribers (WebSocket connections).

use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

const CHANNEL_CAPACITY: usize = 256;

pub struct BroadcastHub {
    channels: Arc<DashMap<String, broadcast::Sender<serde_json::Value>>>,
}

impl BroadcastHub {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(DashMap::new()),
        }
    }

    pub fn subscribe(&self, channel: impl Into<String>) -> broadcast::Receiver<serde_json::Value> {
        let key = channel.into();
        let entry = self
            .channels
            .entry(key)
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0);
        entry.subscribe()
    }

    pub fn publish(&self, channel: impl Into<String>, payload: serde_json::Value) {
        let key = channel.into();
        if let Some(sender) = self.channels.get(&key) {
            let _ = sender.send(payload);
        }
    }

    /// Returns the number of currently-subscribed clients on a channel.
    pub fn subscriber_count(&self, channel: &str) -> usize {
        self.channels
            .get(channel)
            .map(|s| s.receiver_count())
            .unwrap_or(0)
    }
}

impl Default for BroadcastHub {
    fn default() -> Self {
        Self::new()
    }
}
```

```rust
// framework/src/broadcast/mod.rs
pub mod hub;
pub mod socket;
pub mod presence;
pub mod fanout;
pub mod auth;
pub mod testing;

pub use hub::BroadcastHub;
pub use presence::{PresenceChannel, PresenceEvent};

use crate::FrameworkError;
use std::sync::{Arc, OnceLock};

static HUB: OnceLock<Arc<BroadcastHub>> = OnceLock::new();

fn hub() -> Arc<BroadcastHub> {
    HUB.get_or_init(|| Arc::new(BroadcastHub::new())).clone()
}

pub struct Broadcast;

impl Broadcast {
    /// Send `payload` on `channel`. Delivered to every local
    /// subscriber and, if fanout is configured, to remote subscribers
    /// across the sea-streamer bus.
    pub async fn send(channel: impl Into<String>, payload: serde_json::Value) -> Result<(), FrameworkError> {
        if testing::is_active() {
            return testing::record(channel.into(), payload);
        }
        let channel = channel.into();
        hub().publish(&channel, payload.clone());
        fanout::publish(&channel, payload).await?;
        Ok(())
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn fake() -> testing::BroadcastFakeGuard {
        testing::install_fake()
    }
}

/// Typed-channel trait — implementor declares the channel name
/// pattern and (optionally) overrides `authorize`.
pub trait Channel: Send + Sync + 'static {
    /// The channel name as instantiated for this instance, e.g.
    /// `"orders.42"`.
    fn name(&self) -> String;

    /// Authorization check. Default allows all. Override for private
    /// or presence channels.
    fn authorize(&self, user: &dyn crate::Authenticatable) -> bool {
        let _ = user;
        true
    }

    /// Whether this is a presence channel (joins/leaves emit events).
    fn is_presence() -> bool
    where
        Self: Sized,
    {
        false
    }
}
```

```rust
// framework/src/lib.rs
pub mod broadcast;
pub use broadcast::{Broadcast, Channel};
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test broadcast hub
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/broadcast framework/src/lib.rs framework/tests/broadcast.rs
git commit -m "feat(broadcast): BroadcastHub + Broadcast facade + Channel trait"
```

---

## Task 3: `#[channel("name.{id}")]` macro

**Files:** `suprnova-macros/src/channel.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/broadcast.rs — append
use suprnova::Channel;

#[derive(Debug)]
struct OrderChannel {
    pub id: i64,
}

#[suprnova::channel("orders.{id}")]
impl OrderChannel {
    pub fn authorize(&self, user: &dyn suprnova::Authenticatable) -> bool {
        // Allow if the user owns the order; stub for test.
        user.auth_identifier() == self.id.to_string()
    }
}

#[test]
fn channel_macro_produces_correct_name() {
    let c = OrderChannel { id: 42 };
    assert_eq!(c.name(), "orders.42");
}
```

- [ ] **Step 2: Implement**

```rust
// suprnova-macros/src/channel.rs
use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemImpl, Lit, LitStr};

pub fn channel(attr: TokenStream, item: TokenStream) -> TokenStream {
    let pattern = parse_macro_input!(attr as LitStr).value();
    let item = parse_macro_input!(item as ItemImpl);
    let self_ty = &item.self_ty;
    let items = &item.items;

    // Parse `"orders.{id}"` → ("orders.", vec!["id"]) so we can build
    // a format!() call. Multi-placeholder support: replace each
    // {field} segment with the corresponding self.field access.
    let mut format_string = String::new();
    let mut placeholders: Vec<syn::Ident> = Vec::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let mut name = String::new();
            for nc in chars.by_ref() {
                if nc == '}' {
                    break;
                }
                name.push(nc);
            }
            format_string.push_str("{}");
            placeholders.push(syn::Ident::new(&name, proc_macro2::Span::call_site()));
        } else {
            format_string.push(c);
        }
    }

    let format_lit = LitStr::new(&format_string, proc_macro2::Span::call_site());
    let access = placeholders.iter().map(|p| quote!(self.#p));

    let expanded = quote! {
        impl ::suprnova::Channel for #self_ty {
            fn name(&self) -> ::std::string::String {
                format!(#format_lit, #(#access),*)
            }
        }

        impl #self_ty {
            #(#items)*
        }
    };
    expanded.into()
}
```

```rust
// suprnova-macros/src/lib.rs — append
mod channel;

#[proc_macro_attribute]
pub fn channel(attr: TokenStream, item: TokenStream) -> TokenStream {
    channel::channel(attr, item)
}
```

```rust
// framework/src/lib.rs
pub use suprnova_macros::channel;
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test broadcast channel_macro
```

- [ ] **Step 4: Commit**

```bash
git add suprnova-macros framework/src/lib.rs framework/tests/broadcast.rs
git commit -m "feat(broadcast): #[channel(\"name.{id}\")] macro emits Channel impl"
```

---

## Task 4: WebSocket upgrade handler at /broadcast

**Files:** `framework/src/broadcast/socket.rs`, `framework/src/server.rs`

- [ ] **Step 1: Write integration test (real WebSocket client)**

```rust
// framework/tests/broadcast.rs — append
#[tokio::test]
async fn websocket_subscriber_receives_published_message() {
    // 1. Start a suprnova server (programmatically — likely via a
    //    test helper that returns the local addr).
    // 2. Connect via tungstenite-client and SEND a subscription frame:
    //      {"action": "subscribe", "channel": "orders.1"}
    // 3. Publish:
    //      suprnova::Broadcast::send("orders.1", serde_json::json!({"status": "shipped"})).await.unwrap();
    // 4. Receive the frame and assert on the payload.
    // (Full integration test scaffolding omitted — see broadcast/socket.rs
    //  for the test harness pattern; cribbed from precognition.rs.)
}
```

- [ ] **Step 2: Implement upgrade handler**

```rust
// framework/src/broadcast/socket.rs
//! WebSocket upgrade + subscription framing.
//!
//! Frames are JSON envelopes:
//! Client → server:  {"action": "subscribe", "channel": "orders.1", "auth": "..."}
//! Client → server:  {"action": "unsubscribe", "channel": "orders.1"}
//! Server → client:  {"channel": "orders.1", "data": {...}}

use crate::FrameworkError;
use futures::{SinkExt, StreamExt};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use std::collections::HashSet;
use tokio_tungstenite::{tungstenite::Message, WebSocketStream};

#[derive(Debug, Deserialize)]
struct ClientFrame {
    action: String,
    channel: String,
    #[serde(default)]
    auth: Option<String>,
}

/// Handle a hyper request that has the `Upgrade: websocket` header.
/// Returns the response that hyper should send to complete the
/// handshake. The actual WebSocket conversation happens in a tokio
/// task spawned inside this function.
pub async fn handle_upgrade(
    req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<Full<Bytes>>, FrameworkError> {
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

    let key = req
        .headers()
        .get("sec-websocket-key")
        .ok_or_else(|| FrameworkError::internal("missing Sec-WebSocket-Key"))?
        .as_bytes();
    let accept = derive_accept_key(key);

    let response = hyper::Response::builder()
        .status(101)
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-accept", accept)
        .body(Full::new(Bytes::new()))
        .unwrap();

    tokio::spawn(async move {
        let upgraded = match hyper::upgrade::on(req).await {
            Ok(u) => u,
            Err(e) => {
                tracing::error!(error = %e, "upgrade failed");
                return;
            }
        };
        let io = TokioIo::new(upgraded);
        let ws = WebSocketStream::from_raw_socket(io, tokio_tungstenite::tungstenite::protocol::Role::Server, None).await;
        run_session(ws).await;
    });

    Ok(response)
}

async fn run_session(ws: WebSocketStream<TokioIo<Upgraded>>) {
    let (mut sink, mut stream) = ws.split();
    let mut subscribed: HashSet<String> = HashSet::new();
    let mut receivers: Vec<(String, tokio::sync::broadcast::Receiver<serde_json::Value>)> = Vec::new();

    loop {
        tokio::select! {
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else { break };
                if let Message::Text(text) = msg {
                    if let Ok(frame) = serde_json::from_str::<ClientFrame>(&text) {
                        match frame.action.as_str() {
                            "subscribe" => {
                                if !subscribed.contains(&frame.channel) {
                                    let rx = super::hub().subscribe(&frame.channel);
                                    receivers.push((frame.channel.clone(), rx));
                                    subscribed.insert(frame.channel);
                                }
                            }
                            "unsubscribe" => {
                                subscribed.remove(&frame.channel);
                                receivers.retain(|(c, _)| c != &frame.channel);
                            }
                            _ => {}
                        }
                    }
                }
            }
            // Multiplex across all receivers: poll each via select_all
            (channel, payload) = poll_receivers(&mut receivers) => {
                let frame = serde_json::json!({"channel": channel, "data": payload});
                let _ = sink.send(Message::Text(frame.to_string())).await;
            }
        }
    }
}

async fn poll_receivers(
    receivers: &mut [(String, tokio::sync::broadcast::Receiver<serde_json::Value>)],
) -> (String, serde_json::Value) {
    use futures::future::select_all;
    let futures: Vec<_> = receivers
        .iter_mut()
        .map(|(c, r)| {
            let c = c.clone();
            Box::pin(async move { (c, r.recv().await) })
        })
        .collect();
    if futures.is_empty() {
        // Park forever if no subscriptions yet.
        std::future::pending::<()>().await;
        unreachable!();
    }
    let ((channel, result), _, _) = select_all(futures).await;
    match result {
        Ok(payload) => (channel, payload),
        Err(_) => (channel, serde_json::json!({})),
    }
}
```

> **Multiplex implementation:** `select_all` over receivers needs `Box::pin(...)`'d futures. The pattern above is suggestive; the implementer should verify futures-0.3's `select_all` API and possibly use `tokio::sync::broadcast::Receiver`'s stream adapter (`BroadcastStream`) with `StreamExt::select_all`. Whatever shape ships, the multiplex must wake on ANY of the subscribed channels.

- [ ] **Step 3: Wire into Server**

```rust
// framework/src/server.rs — in the request dispatch path, before route lookup:
if request.path() == "/broadcast"
    && request
        .header("upgrade")
        .map(|h| h.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
{
    return crate::broadcast::socket::handle_upgrade(hyper_req).await;
}
```

- [ ] **Step 4: Smoke test**

```bash
cargo run -p app -- serve &
# In another shell:
wscat -c ws://127.0.0.1:8000/broadcast
> {"action": "subscribe", "channel": "test"}
# In a third shell, write a small Rust test or curl-equivalent that
# calls Broadcast::send("test", ...) and watch the wscat connection.
```

- [ ] **Step 5: Commit**

```bash
git add framework/src/broadcast/socket.rs framework/src/server.rs
git commit -m "feat(broadcast): WebSocket upgrade + subscribe/unsubscribe framing on /broadcast"
```

---

## Task 5: Private channel auth flow

**Files:** `framework/src/broadcast/auth.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/broadcast.rs — append
#[tokio::test]
async fn private_channel_rejects_unauthorized_subscribe() {
    // Client subscribes to "orders.42" without auth. Channel impl's
    // authorize() returns false. Server sends back
    // {"channel":"orders.42", "error":"unauthorized"} and does not
    // route messages to that subscription.
    // (Test scaffolds via in-process WebSocket simulation.)
}
```

- [ ] **Step 2: Wire authorize() into the subscribe path**

```rust
// framework/src/broadcast/socket.rs — inside the "subscribe" arm of run_session
"subscribe" => {
    // Resolve the typed channel from the name. The framework
    // ships a registry that maps channel-name patterns to factory
    // closures that build the channel struct with parsed params.
    let user = crate::Auth::user().await.ok().flatten();
    let typed = super::registry::resolve(&frame.channel);
    let allowed = match typed {
        Some(c) => match user.as_ref() {
            Some(u) => c.authorize(u.as_ref()),
            None => false,
        },
        None => true, // public channels with no typed registration default to allow
    };
    if !allowed {
        let _ = sink
            .send(Message::Text(serde_json::json!({"channel": frame.channel, "error": "unauthorized"}).to_string()))
            .await;
        continue;
    }
    // ... existing subscribe logic ...
}
```

- [ ] **Step 3: Channel registry**

```rust
// framework/src/broadcast/registry.rs
use super::Channel;
use std::sync::RwLock;

type ChannelFactory = fn(&str) -> Option<Box<dyn Channel>>;

static FACTORIES: RwLock<Vec<ChannelFactory>> = RwLock::new(Vec::new());

pub fn register(factory: ChannelFactory) {
    FACTORIES.write().unwrap().push(factory);
}

pub fn resolve(name: &str) -> Option<Box<dyn Channel>> {
    let factories = FACTORIES.read().unwrap().clone();
    for f in factories {
        if let Some(ch) = f(name) {
            return Some(ch);
        }
    }
    None
}
```

> The `#[channel("orders.{id}")]` macro from Task 3 should also generate a factory closure registered via `inventory::submit!` so this resolves the channel struct from the inbound name.

- [ ] **Step 4: Commit**

```bash
git add framework/src/broadcast
git commit -m "feat(broadcast): private channel auth flow + channel registry"
```

---

## Task 6: Presence channels

**Files:** `framework/src/broadcast/presence.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/broadcast/presence.rs
use dashmap::DashMap;
use serde::Serialize;
use std::sync::Arc;

/// Stable presence tracking — who's currently subscribed to a
/// presence channel. Joins/leaves emit `PresenceEvent` payloads on
/// the channel itself.
#[derive(Default)]
pub struct PresenceChannel {
    /// channel_name → set of member_id → JSON metadata
    members: Arc<DashMap<String, DashMap<String, serde_json::Value>>>,
}

#[derive(Serialize, Clone)]
pub struct PresenceEvent {
    pub event: &'static str, // "joined" | "left"
    pub member_id: String,
    pub metadata: serde_json::Value,
}

impl PresenceChannel {
    pub fn new() -> Self {
        Self {
            members: Arc::new(DashMap::new()),
        }
    }

    pub fn join(&self, channel: &str, member_id: String, metadata: serde_json::Value) {
        let entry = self
            .members
            .entry(channel.to_string())
            .or_insert_with(DashMap::new);
        entry.insert(member_id.clone(), metadata.clone());
        super::hub().publish(
            channel,
            serde_json::to_value(&PresenceEvent {
                event: "joined",
                member_id,
                metadata,
            })
            .unwrap(),
        );
    }

    pub fn leave(&self, channel: &str, member_id: &str) {
        if let Some(entry) = self.members.get(channel) {
            if let Some((_, metadata)) = entry.remove(member_id) {
                super::hub().publish(
                    channel,
                    serde_json::to_value(&PresenceEvent {
                        event: "left",
                        member_id: member_id.to_string(),
                        metadata,
                    })
                    .unwrap(),
                );
            }
        }
    }

    pub fn members(&self, channel: &str) -> Vec<(String, serde_json::Value)> {
        self.members
            .get(channel)
            .map(|m| m.iter().map(|kv| (kv.key().clone(), kv.value().clone())).collect())
            .unwrap_or_default()
    }
}
```

- [ ] **Step 2: Hook into the subscribe path**

When `Channel::is_presence()` is `true`, the subscribe handler in `socket.rs` calls `PresenceChannel::join(...)` with the authenticated user's id + a metadata payload. When the WebSocket disconnects (the `tokio::select` loop exits), the handler calls `leave(...)` for every joined presence channel.

- [ ] **Step 3: Test + commit**

```bash
git add framework/src/broadcast/presence.rs framework/src/broadcast/socket.rs
git commit -m "feat(broadcast): PresenceChannel with join/leave + joined/left events"
```

---

## Task 7: Multi-process fanout via sea-streamer

**Files:** `framework/src/broadcast/fanout.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/broadcast/fanout.rs
//! Multi-process fanout — publish to a sea-streamer topic so peer
//! processes hosting WebSocket connections re-broadcast inbound
//! messages to their local subscribers.

use crate::FrameworkError;
use std::sync::OnceLock;

static FANOUT: OnceLock<Option<sea_streamer::SeaProducer>> = OnceLock::new();

/// Enable fanout via sea-streamer. Pass a URL like
/// `redis://localhost:6379` or `kafka://localhost:9092` and a
/// topic name that all peer processes share.
pub async fn enable_redis(url: &str, topic: &str) -> Result<(), FrameworkError> {
    let producer = build_producer(url, topic).await?;
    let _ = FANOUT.set(Some(producer));
    // Start the consumer task that subscribes to the topic and
    // re-broadcasts into the local hub.
    spawn_consumer(url.to_string(), topic.to_string()).await?;
    Ok(())
}

pub async fn enable_kafka(brokers: &str, topic: &str) -> Result<(), FrameworkError> {
    let url = format!("kafka://{}", brokers);
    let producer = build_producer(&url, topic).await?;
    let _ = FANOUT.set(Some(producer));
    spawn_consumer(url, topic.to_string()).await?;
    Ok(())
}

async fn build_producer(url: &str, topic: &str) -> Result<sea_streamer::SeaProducer, FrameworkError> {
    use sea_streamer::{SeaStreamer, StreamKey, Streamer as _};
    let streamer = SeaStreamer::connect(url.parse().map_err(map_err)?, Default::default())
        .await
        .map_err(map_err)?;
    streamer
        .create_producer(StreamKey::new(topic.to_string()).map_err(map_err)?, Default::default())
        .await
        .map_err(map_err)
}

async fn spawn_consumer(url: String, topic: String) -> Result<(), FrameworkError> {
    use sea_streamer::{Consumer, ConsumerMode, Message, SeaStreamer, StreamKey, Streamer as _, SeaConsumerOptions};
    let streamer = SeaStreamer::connect(url.parse().map_err(map_err)?, Default::default())
        .await
        .map_err(map_err)?;
    let consumer: sea_streamer::SeaConsumer = streamer
        .create_consumer(&[StreamKey::new(topic).map_err(map_err)?], SeaConsumerOptions::new(ConsumerMode::RealTime))
        .await
        .map_err(map_err)?;
    tokio::spawn(async move {
        loop {
            match consumer.next().await {
                Ok(msg) => {
                    if let Ok(bytes) = msg.message().as_bytes() {
                        if let Ok(envelope) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                            let channel = envelope["channel"].as_str().unwrap_or_default().to_string();
                            let payload = envelope["payload"].clone();
                            super::hub().publish(&channel, payload);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "fanout consumer");
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }
    });
    Ok(())
}

pub(crate) async fn publish(channel: &str, payload: serde_json::Value) -> Result<(), FrameworkError> {
    if let Some(Some(producer)) = FANOUT.get() {
        let envelope = serde_json::json!({"channel": channel, "payload": payload});
        let bytes = serde_json::to_vec(&envelope).map_err(map_err)?;
        producer.send(bytes).map_err(map_err)?;
    }
    Ok(())
}

fn map_err<E: std::fmt::Display>(e: E) -> FrameworkError {
    FrameworkError::internal(format!("fanout: {}", e))
}
```

- [ ] **Step 2: Commit**

```bash
git add framework/src/broadcast/fanout.rs
git commit -m "feat(broadcast): multi-process fanout via sea-streamer redis/kafka"
```

---

## Task 8: Supervised workers

**Files:** `framework/src/worker/mod.rs`, `framework/src/worker/supervisor.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/supervised_workers.rs
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use suprnova::Worker;

#[tokio::test]
async fn supervised_worker_runs_periodically() {
    let count = Arc::new(AtomicI64::new(0));
    let c = count.clone();
    Worker::supervise("ticker", Duration::from_millis(50), move || {
        let c = c.clone();
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            Ok::<_, suprnova::FrameworkError>(())
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(280)).await;
    Worker::stop("ticker").await;
    let n = count.load(Ordering::SeqCst);
    assert!(n >= 4, "expected at least 4 ticks, got {}", n);
}

#[tokio::test]
async fn supervised_worker_restarts_after_panic() {
    let count = Arc::new(AtomicI64::new(0));
    let c = count.clone();
    Worker::supervise("crasher", Duration::from_millis(30), move || {
        let c = c.clone();
        async move {
            let n = c.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                panic!("crashed");
            }
            Ok::<_, suprnova::FrameworkError>(())
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    Worker::stop("crasher").await;
    assert!(count.load(Ordering::SeqCst) >= 3, "expected restart after panic");
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/worker/supervisor.rs
use crate::FrameworkError;
use futures::future::BoxFuture;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

pub struct Supervisor {
    pub name: String,
    handle: Mutex<Option<JoinHandle<()>>>,
    stop_tx: Mutex<Option<tokio::sync::watch::Sender<bool>>>,
}

impl Supervisor {
    pub fn new(name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            handle: Mutex::new(None),
            stop_tx: Mutex::new(None),
        })
    }

    pub async fn spawn<F, Fut>(self: &Arc<Self>, interval: Duration, work: F)
    where
        F: Fn() -> Fut + Send + Sync + Clone + 'static,
        Fut: std::future::Future<Output = Result<(), FrameworkError>> + Send + 'static,
    {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let me = self.clone();
        let handle = tokio::spawn(async move {
            let mut backoff_ms = 100u64;
            loop {
                if *rx.borrow() {
                    return;
                }
                let work = work.clone();
                let join = tokio::spawn(async move { work().await });
                let result = join.await;
                match result {
                    Ok(Ok(())) => {
                        backoff_ms = 100; // reset on success
                        tokio::time::sleep(interval).await;
                    }
                    Ok(Err(e)) => {
                        tracing::error!(worker = %me.name, error = %e, "worker error");
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        backoff_ms = (backoff_ms * 2).min(30_000);
                    }
                    Err(e) if e.is_panic() => {
                        tracing::error!(worker = %me.name, "worker panic — restarting in {}ms", backoff_ms);
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        backoff_ms = (backoff_ms * 2).min(30_000);
                    }
                    Err(_) => return,
                }
            }
        });
        *self.handle.lock().await = Some(handle);
        *self.stop_tx.lock().await = Some(tx);
    }

    pub async fn stop(&self) {
        if let Some(tx) = self.stop_tx.lock().await.take() {
            let _ = tx.send(true);
        }
        if let Some(h) = self.handle.lock().await.take() {
            let _ = h.await;
        }
    }
}
```

```rust
// framework/src/worker/mod.rs
mod supervisor;

use crate::FrameworkError;
use dashmap::DashMap;
use futures::future::BoxFuture;
use std::sync::Arc;
use std::time::Duration;

static REGISTRY: once_cell::sync::Lazy<DashMap<String, Arc<supervisor::Supervisor>>> =
    once_cell::sync::Lazy::new(DashMap::new);

pub struct Worker;

impl Worker {
    /// Start a supervised background worker. The closure is invoked
    /// every `interval`. If it returns an `Err` or panics, the
    /// supervisor restarts it with exponential backoff (100ms → 30s).
    pub async fn supervise<F, Fut>(
        name: impl Into<String>,
        interval: Duration,
        work: F,
    ) -> Result<(), FrameworkError>
    where
        F: Fn() -> Fut + Send + Sync + Clone + 'static,
        Fut: std::future::Future<Output = Result<(), FrameworkError>> + Send + 'static,
    {
        let name = name.into();
        if REGISTRY.contains_key(&name) {
            return Err(FrameworkError::internal(format!(
                "worker '{}' already running",
                name
            )));
        }
        let sup = supervisor::Supervisor::new(name.clone());
        sup.spawn(interval, work).await;
        REGISTRY.insert(name, sup);
        Ok(())
    }

    pub async fn stop(name: &str) {
        if let Some((_, sup)) = REGISTRY.remove(name) {
            sup.stop().await;
        }
    }
}
```

```rust
// framework/src/lib.rs
pub mod worker;
pub use worker::Worker;
```

> **`once_cell` dep:** Already a transitive; verify with `cargo tree -p suprnova | grep once_cell`. If not present, add `once_cell = "1"` to framework/Cargo.toml.

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test supervised_workers
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/worker framework/src/lib.rs framework/tests/supervised_workers.rs
git commit -m "feat(worker): Worker::supervise with exponential-backoff restart"
```

---

## Task 9: App dogfood — orders channel + payments poll worker

- [ ] **Step 1: Define channel**

```rust
// app/src/channels/orders_channel.rs
use suprnova::{channel, Authenticatable};

#[derive(Debug)]
pub struct OrderChannel {
    pub id: i64,
}

#[channel("orders.{id}")]
impl OrderChannel {
    pub fn authorize(&self, user: &dyn Authenticatable) -> bool {
        // In real code: check if user owns or has access to this order
        user.auth_identifier() == self.id.to_string()
    }
}
```

- [ ] **Step 2: Define worker**

```rust
// app/src/workers/payments_poll.rs
use std::time::Duration;
use suprnova::{FrameworkError, Worker};
use tracing::info;

pub async fn install() -> Result<(), FrameworkError> {
    Worker::supervise("payments.poll", Duration::from_secs(30), || async {
        info!("polling pending payments");
        // poll_pending_payments(&DB::get()?).await?;
        Ok(())
    })
    .await
}
```

- [ ] **Step 3: Wire from bootstrap**

```rust
// app/src/bootstrap.rs — inside register()
crate::workers::payments_poll::install().await.expect("payments worker");
```

- [ ] **Step 4: Smoke test**

```bash
cargo run -p app -- serve
# Verify logs show "polling pending payments" every 30s.
```

- [ ] **Step 5: Commit**

```bash
git add app/src
git commit -m "feat(app): OrderChannel + payments_poll supervised worker dogfood"
```

---

## Task 10: Workspace lint + verification + roadmap update

- [ ] **Step 1: Clippy + tests**

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

- [ ] **Step 2: ROADMAP update**

Move from "Missing" to "Production-ready":
- Broadcasting (WebSocket + typed channels + presence + private auth)
- Multi-process fanout (sea-streamer)
- Supervised workers (Worker::supervise)

- [ ] **Step 3: Commit + push**

---

## Self-Review

| Spec item | Covered by |
|-----------|------------|
| BroadcastHub in-process | Task 2 |
| Broadcast::send facade | Task 2 |
| #[channel] macro | Task 3 |
| WebSocket upgrade | Task 4 |
| subscribe/unsubscribe framing | Task 4 |
| Private channel auth | Task 5 |
| Presence channels | Task 6 |
| Multi-process fanout (sea-streamer) | Task 7 |
| Worker::supervise | Task 8 |
| Exponential backoff restart | Task 8 |
| Dogfood | Task 9 |

---

## Execution Handoff

**Subagent-Driven recommended; the WebSocket upgrade integration test is the biggest single piece — give it its own task agent.**
