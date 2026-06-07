//! `BroadcastingWsHandler` — wires the JSON-envelope subscribe
//! protocol against a `BroadcastHub` + `ChannelRegistry`.
//!
//! Drop into `ws!()` with the resolved hub and registry:
//!
//! ```rust,ignore
//! ws!("/ws/broadcast", BroadcastingWsHandler::new(hub, registry))
//!     .middleware(SessionMiddleware::new()),
//! ```
//!
//! # Security note
//!
//! Client-initiated `Publish` frames are gated by **two** checks:
//!
//! 1. The connection MUST hold an authorized subscription to the
//!    target channel (i.e. an entry in the per-connection forwarders
//!    map placed there by a successful `Subscribe`). Publishes from
//!    connections that never subscribed — or whose subscription was
//!    rejected — are refused even if `authorize_publish` would have
//!    returned `true`. This mirrors the Pusher client-event contract
//!    where client events require an established private/presence
//!    subscription.
//! 2. `Channel::authorize_publish` is then consulted on the resolved
//!    channel. The default implementation returns `false` (deny), so
//!    only channels that explicitly override the hook accept client
//!    publishes.
//!
//! Unknown channels always reject. Server-side `hub.publish()` calls
//! bypass both gates entirely (server is already trusted).

use crate::FrameworkError;
use crate::broadcasting::channel::ChannelRegistry;
use crate::broadcasting::hub::{BroadcastEnvelope, BroadcastHub};
use crate::broadcasting::protocol::{ClientFrame, ServerFrame};
use crate::http::Request;
use crate::ws::{WebSocketHandler, WsSocket};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Presence state carried alongside each forwarder.
// ---------------------------------------------------------------------------

/// Presence metadata for a single channel subscription. `None` for
/// non-presence channels.
struct PresenceState {
    member_id: String,
    info: Value,
}

/// Combined forwarder entry stored in the per-connection map.
struct ForwarderEntry {
    handle: JoinHandle<()>,
    presence: Option<PresenceState>,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// The framework's reusable WS handler that implements the
/// broadcasting subscribe/unsubscribe/publish protocol over the
/// JSON envelope wire format defined in `protocol.rs`.
///
/// Construct with `BroadcastingWsHandler::new(hub, registry)` and
/// register with `Router::ws`:
///
/// ```rust,ignore
/// let handler = BroadcastingWsHandler::new(hub.clone(), registry.clone());
/// let router = Router::new().ws("/ws/broadcast", handler);
/// ```
/// Default per-connection cap on distinct channel subscriptions. A
/// well-behaved client subscribes to a handful of channels; the cap
/// only matters as a guardrail against a malicious or buggy client
/// minting thousands of `orders.{id}` permutations on one connection
/// to fill the per-connection forwarder map and tie up tasks.
pub const DEFAULT_MAX_SUBSCRIPTIONS_PER_CONNECTION: usize = 100;

pub struct BroadcastingWsHandler {
    hub: Arc<dyn BroadcastHub>,
    registry: Arc<ChannelRegistry>,
    /// Per-connection cap on the count of distinct channel keys held
    /// in the `forwarders` map. Re-subscribes to an already-present
    /// channel are not counted (idempotent), so a client can refresh
    /// an existing subscription regardless of the cap. See
    /// [`Self::with_max_subscriptions`].
    max_subscriptions: usize,
}

impl BroadcastingWsHandler {
    /// Create a new handler backed by the given hub and channel registry.
    ///
    /// `hub` accepts any `Arc<H>` where `H: BroadcastHub`; the
    /// coercion to `Arc<dyn BroadcastHub>` happens at the call site.
    pub fn new(hub: Arc<dyn BroadcastHub>, registry: Arc<ChannelRegistry>) -> Self {
        Self {
            hub,
            registry,
            max_subscriptions: DEFAULT_MAX_SUBSCRIPTIONS_PER_CONNECTION,
        }
    }

    /// Override the per-connection subscription cap. Once a connection
    /// holds `max` distinct channel keys in its forwarder map, further
    /// `Subscribe` frames for *new* channel names are rejected with a
    /// `ServerFrame::Error { reason: "subscription limit reached" }`
    /// — re-subscribes to an already-active channel are still allowed
    /// (they replace the forwarder in place and don't grow the map).
    ///
    /// The default is [`DEFAULT_MAX_SUBSCRIPTIONS_PER_CONNECTION`]
    /// (`100`). Lower it on memory-constrained deployments or when an
    /// app declaratively bounds the channel surface; raise it for a
    /// handful of trusted-internal clients that legitimately fan out
    /// many channels per socket.
    pub fn with_max_subscriptions(mut self, max: usize) -> Self {
        self.max_subscriptions = max;
        self
    }
}

#[async_trait]
impl WebSocketHandler for BroadcastingWsHandler {
    async fn handle(&self, mut socket: WsSocket, req: Request) -> Result<(), FrameworkError> {
        // Per-channel forwarder entries.  Aborted on unsubscribe or
        // when the connection ends.
        let forwarders: Arc<Mutex<HashMap<String, ForwarderEntry>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Outbound mpsc: forwarders push serialised ServerFrame::Event
        // strings here; the select! arm below drains them to the socket.
        // Using a String channel rather than WsSocket::sender() (which
        // is pub(crate) to the ws module) keeps serialisation concerns
        // inside this module.
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<String>(64);

        // Assign this connection a socket id and announce it first, so the
        // client can echo it as `X-Socket-ID` and a server-side
        // `broadcast_to_others` can exclude this connection. Mirrors Pusher's
        // `connection_established`.
        let socket_id = Uuid::new_v4().to_string();
        socket
            .send_text(
                serde_json::to_string(&ServerFrame::Connected {
                    socket_id: socket_id.clone(),
                })
                .unwrap_or_default(),
            )
            .await?;

        // Inner-async-block pattern: every exit out of the loop body
        // (clean break on `Ok(None)`, `?` on outbound/inbound IO, `?` from
        // helper functions) lands here in `result`, after which the
        // teardown loop below runs unconditionally. Without this wrapping
        // the typical browser disconnect — tab close, network drop, OS
        // RST — would skip teardown entirely: presence members would leak
        // forever, forwarder tasks would detach blocked on `rx.recv()`,
        // and the hub channel would stay pinned by their receiver count.
        let result: Result<(), FrameworkError> = async {
            loop {
                tokio::select! {
                    // Outbound arm: a forwarder pushed an event.
                    Some(text) = outbound_rx.recv() => {
                        socket.send_text(text).await?;
                    }
                    // Inbound arm: client sent a frame.
                    inbound = socket.recv_text() => {
                        let text = match inbound? {
                            Some(t) => t,
                            None => break, // connection closed cleanly
                        };

                        match serde_json::from_str::<ClientFrame>(&text) {
                            Ok(ClientFrame::Subscribe { channel, data }) => {
                                handle_subscribe(
                                    &channel,
                                    &data,
                                    &req,
                                    &self.hub,
                                    &self.registry,
                                    &forwarders,
                                    &outbound_tx,
                                    &socket_id,
                                    self.max_subscriptions,
                                    &mut socket,
                                )
                                .await?;
                            }
                            Ok(ClientFrame::Unsubscribe { channel }) => {
                                handle_unsubscribe(
                                    &channel,
                                    &self.hub,
                                    &forwarders,
                                    &mut socket,
                                )
                                .await?;
                            }
                            Ok(ClientFrame::Publish { channel, event, data }) => {
                                // Two-stage publish authorization. Fail closed on:
                                //   - Connection never subscribed: no entry in
                                //     `forwarders` → reject (Pusher client-event
                                //     contract requires an established subscription)
                                //   - Unknown channel: no impl to consult → reject
                                //   - Channel says no: reject with Error frame
                                //   - Channel says yes: proceed to hub.publish
                                let is_subscribed = {
                                    let map = forwarders.lock().await;
                                    map.contains_key(&channel)
                                };

                                let allowed = if !is_subscribed {
                                    false
                                } else {
                                    match self.registry.resolve(&channel) {
                                        Some((ch, params)) => {
                                            ch.authorize_publish(&req, &params, &event, &data).await
                                        }
                                        None => false,
                                    }
                                };

                                if !allowed {
                                    let err = ServerFrame::Error {
                                        channel: Some(channel.clone()),
                                        reason: "publish unauthorized".into(),
                                    };
                                    socket
                                        .send_text(
                                            serde_json::to_string(&err).unwrap_or_default(),
                                        )
                                        .await?;
                                } else {
                                    // Client publishes are not socket-excluded — the
                                    // publisher receives its own event like any other
                                    // subscriber (see broadcasting docs).
                                    let chan_for_err = channel.clone();
                                    if let Err(e) = self
                                        .hub
                                        .publish(BroadcastEnvelope::new(channel, event, data))
                                        .await
                                    {
                                        // Surface broker / fanout failures back to
                                        // the originating client so it knows the
                                        // publish didn't reach other processes.
                                        let err = ServerFrame::Error {
                                            channel: Some(chan_for_err),
                                            reason: format!("publish failed: {e}"),
                                        };
                                        socket
                                            .send_text(
                                                serde_json::to_string(&err).unwrap_or_default(),
                                            )
                                            .await?;
                                    }
                                }
                            }
                            Err(e) => {
                                let err = ServerFrame::Error {
                                    channel: None,
                                    reason: format!("malformed envelope: {e}"),
                                };
                                socket
                                    .send_text(serde_json::to_string(&err).unwrap_or_default())
                                    .await?;
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        .await;

        // Teardown runs on every exit path, not just the clean `Ok(None)`
        // break above. Publish `presence.left` for any remaining presence
        // subscriptions, then abort each forwarder task deterministically
        // — relying on `JoinHandle`'s detach-on-drop semantics would let
        // the task block on `rx.recv().await` indefinitely if the broadcast
        // sender is kept alive elsewhere. A hub publish failure on shutdown
        // is logged but doesn't replace the original exit reason in
        // `result`.
        let mut map = forwarders.lock().await;
        for (channel, entry) in map.drain() {
            if let Some(ps) = entry.presence {
                self.hub.untrack_member(&channel, &ps.member_id).await;
                if let Err(e) = self
                    .hub
                    .publish(BroadcastEnvelope::new(
                        channel.clone(),
                        "presence.left",
                        ps.info,
                    ))
                    .await
                {
                    tracing::warn!(
                        channel = %channel,
                        error = %e,
                        "broadcasting handler: presence.left publish failed during teardown"
                    );
                }
            }
            entry.handle.abort();
        }
        drop(map);

        // Re-raise the inner loop's exit reason after teardown ran.
        result
    }
}

// ---------------------------------------------------------------------------
// Internal helpers (free functions to avoid the borrow-checker difficulties
// of `&self` methods that also mutably borrow `socket`).
// ---------------------------------------------------------------------------

// The subscribe path needs all these parameters; a struct would require
// explicit lifetime annotations that add more noise than the lint saves.
#[allow(clippy::too_many_arguments)]
async fn handle_subscribe(
    channel: &str,
    data: &serde_json::Value,
    req: &Request,
    hub: &Arc<dyn BroadcastHub>,
    registry: &Arc<ChannelRegistry>,
    forwarders: &Arc<Mutex<HashMap<String, ForwarderEntry>>>,
    outbound_tx: &tokio::sync::mpsc::Sender<String>,
    socket_id: &str,
    max_subscriptions: usize,
    socket: &mut WsSocket,
) -> Result<(), FrameworkError> {
    // Per-connection subscription cap. Re-subscribes to an existing
    // channel are exempt (they REPLACE the forwarder in place — see
    // the `map.remove(channel)` below — so the map size doesn't grow);
    // first-time subscribes to a brand-new channel name count against
    // the cap. Without this gate a malicious client could subscribe
    // to `orders.{id}` with thousands of distinct ids on one socket
    // and inflate the per-connection forwarder map to exhaust memory
    // and tokio task slots. Check this BEFORE `hub.subscribe` and the
    // `tokio::spawn` so we never spawn a forwarder we'd refuse to
    // register — frames on a single connection are processed
    // sequentially in the `select!` loop, so reading the map here and
    // inserting later is race-free per connection.
    {
        let map = forwarders.lock().await;
        if !map.contains_key(channel) && map.len() >= max_subscriptions {
            let err = ServerFrame::Error {
                channel: Some(channel.to_string()),
                reason: "subscription limit reached".into(),
            };
            drop(map);
            socket
                .send_text(serde_json::to_string(&err).unwrap_or_default())
                .await?;
            return Ok(());
        }
    }

    // Resolve the channel from the registry, capturing any params bound from a
    // parameterized name (e.g. `{id}` for `orders.{id}` subscribed as `orders.42`).
    let Some((ch, params)) = registry.resolve(channel) else {
        let err = ServerFrame::Error {
            channel: Some(channel.to_string()),
            reason: "no such channel".into(),
        };
        socket
            .send_text(serde_json::to_string(&err).unwrap_or_default())
            .await?;
        return Ok(());
    };

    // Authorize the subscription.
    if !ch.authorize(req, &params, data).await {
        let err = ServerFrame::Error {
            channel: Some(channel.to_string()),
            reason: "unauthorized".into(),
        };
        socket
            .send_text(serde_json::to_string(&err).unwrap_or_default())
            .await?;
        return Ok(());
    }

    // Collect presence bootstrap data (snapshot + member id + info) for use
    // after the forwarder is inserted so hub.subscribe() is already live.
    let presence_bootstrap: Option<(Vec<Value>, String, Value)> =
        if let Some(pc) = ch.presence_info() {
            let existing = hub.list_members(channel).await;
            let info = pc.member_info(req, &params).await?;
            let member_id = Uuid::new_v4().to_string();
            Some((existing, member_id, info))
        } else {
            None
        };

    // Subscribe to the hub and spawn a forwarder.
    let mut rx = hub.subscribe(channel);
    let tx = outbound_tx.clone();
    let self_socket = socket_id.to_string();
    // Capture the channel name so the forwarder can name the channel
    // when it emits a Lagged frame after a `broadcast::RecvError::Lagged(_)`.
    let forwarder_channel = channel.to_string();
    let forwarder = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(envelope) => {
                    // Skip the connection this broadcast excludes
                    // (`broadcast_to_others` / per-dispatch `except`); every
                    // other subscriber still receives it.
                    if envelope.except.as_deref() == Some(self_socket.as_str()) {
                        continue;
                    }
                    let frame = ServerFrame::Event {
                        channel: envelope.channel,
                        event: envelope.event,
                        data: envelope.data,
                    };
                    let text = match serde_json::to_string(&frame) {
                        Ok(t) => t,
                        Err(_) => continue,
                    };
                    if tx.send(text).await.is_err() {
                        return; // outbound channel closed — connection gone
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    // The subscriber fell behind the per-channel ring
                    // buffer; `skipped` envelopes were dropped on this
                    // connection. Surface this so the client knows its
                    // local state is stale and must refetch — silently
                    // skipping events would let bugs hide as "we lost a
                    // tick" rather than "the client's state diverged
                    // from the server's".
                    let frame = ServerFrame::Lagged {
                        channel: forwarder_channel.clone(),
                        skipped,
                    };
                    if let Ok(text) = serde_json::to_string(&frame)
                        && tx.send(text).await.is_err()
                    {
                        return; // outbound closed mid-Lagged send
                    }
                    continue;
                }
            }
        }
    });

    // Destructure bootstrap data — used after the forwarder is inserted.
    let (presence_here_members, presence_member_id, presence_info) =
        if let Some((existing, mid, info)) = presence_bootstrap {
            (Some(existing), Some(mid), Some(info))
        } else {
            (None, None, None)
        };

    // Replace any existing forwarder for this channel (idempotent re-subscribe).
    {
        let mut map = forwarders.lock().await;
        if let Some(old) = map.remove(channel) {
            // Existing subscription being replaced — clean up presence if needed.
            if let Some(ps) = old.presence {
                hub.untrack_member(channel, &ps.member_id).await;
                // Cleanup-path publish: log a hub failure but continue —
                // the user just re-subscribed, we shouldn't fail the new
                // sub because the prior presence.left couldn't be
                // forwarded cross-process.
                if let Err(e) = hub
                    .publish(BroadcastEnvelope::new(
                        channel.to_string(),
                        "presence.left",
                        ps.info,
                    ))
                    .await
                {
                    tracing::warn!(
                        channel = %channel,
                        error = %e,
                        "broadcasting handler: presence.left publish failed during resubscribe cleanup"
                    );
                }
            }
            old.handle.abort();
        }

        let final_presence = match (presence_member_id.as_deref(), presence_info.as_ref()) {
            (Some(mid), Some(info)) => Some(PresenceState {
                member_id: mid.to_string(),
                info: info.clone(),
            }),
            _ => None,
        };

        map.insert(
            channel.to_string(),
            ForwarderEntry {
                handle: forwarder,
                presence: final_presence,
            },
        );
    }

    // Send Subscribed ack first.
    let ack = ServerFrame::Subscribed {
        channel: channel.to_string(),
    };
    socket
        .send_text(serde_json::to_string(&ack).unwrap_or_default())
        .await?;

    // Presence post-subscribe steps — forwarder is now live so
    // hub.subscribe() receiver is already active.
    if let (Some(existing), Some(mid), Some(info)) =
        (presence_here_members, presence_member_id, presence_info)
    {
        // Track member AFTER taking the snapshot so self is absent from
        // the presence.here payload (standard Pusher behaviour).
        hub.track_member(channel, &mid, info.clone()).await;

        // presence.here — sent directly to this socket only (not via hub).
        let here = ServerFrame::Event {
            channel: channel.to_string(),
            event: "presence.here".into(),
            data: json!({ "members": existing }),
        };
        socket
            .send_text(serde_json::to_string(&here).unwrap_or_default())
            .await?;

        // presence.joined — published via hub so all subscribers receive it
        // (including the new subscriber via their forwarder — that's the
        // standard Pusher self-join behaviour; clients filter by member_id).
        // A hub failure here is the subscriber being announced; surface
        // via an Error frame on this socket. The local member entry
        // already exists, so cross-process fanout is the only thing
        // that could have dropped.
        if let Err(e) = hub
            .publish(BroadcastEnvelope::new(
                channel.to_string(),
                "presence.joined",
                info,
            ))
            .await
        {
            let err = ServerFrame::Error {
                channel: Some(channel.to_string()),
                reason: format!("presence.joined publish failed: {e}"),
            };
            socket
                .send_text(serde_json::to_string(&err).unwrap_or_default())
                .await?;
        }
    }

    Ok(())
}

async fn handle_unsubscribe(
    channel: &str,
    hub: &Arc<dyn BroadcastHub>,
    forwarders: &Arc<Mutex<HashMap<String, ForwarderEntry>>>,
    socket: &mut WsSocket,
) -> Result<(), FrameworkError> {
    let entry = {
        let mut map = forwarders.lock().await;
        map.remove(channel)
    };

    if let Some(e) = entry {
        if let Some(ps) = e.presence {
            hub.untrack_member(channel, &ps.member_id).await;
            // Cleanup-path publish: a hub failure here doesn't stop the
            // client from getting their Unsubscribed ack below.
            if let Err(err) = hub
                .publish(BroadcastEnvelope::new(
                    channel.to_string(),
                    "presence.left",
                    ps.info,
                ))
                .await
            {
                tracing::warn!(
                    channel = %channel,
                    error = %err,
                    "broadcasting handler: presence.left publish failed during unsubscribe"
                );
            }
        }
        e.handle.abort();
    }

    let ack = ServerFrame::Unsubscribed {
        channel: channel.to_string(),
    };
    socket
        .send_text(serde_json::to_string(&ack).unwrap_or_default())
        .await?;
    Ok(())
}
