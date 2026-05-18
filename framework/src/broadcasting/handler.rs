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
//! v1 accepts `ClientFrame::Publish` from any authenticated subscriber
//! without per-channel publish-side authorization. Applications that
//! need to restrict which clients may publish should implement a
//! channel-level publish gate; a `can_publish` hook on `Channel` lands
//! in Phase 7B+.

use crate::broadcasting::channel::ChannelRegistry;
use crate::broadcasting::hub::{BroadcastEnvelope, BroadcastHub};
use crate::broadcasting::protocol::{ClientFrame, ServerFrame};
use crate::http::Request;
use crate::ws::{WebSocketHandler, WsSocket};
use crate::FrameworkError;
use async_trait::async_trait;
use serde_json::{json, Value};
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
pub struct BroadcastingWsHandler {
    hub: Arc<dyn BroadcastHub>,
    registry: Arc<ChannelRegistry>,
}

impl BroadcastingWsHandler {
    /// Create a new handler backed by the given hub and channel registry.
    ///
    /// `hub` accepts any `Arc<H>` where `H: BroadcastHub`; the
    /// coercion to `Arc<dyn BroadcastHub>` happens at the call site.
    pub fn new(hub: Arc<dyn BroadcastHub>, registry: Arc<ChannelRegistry>) -> Self {
        Self { hub, registry }
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
                            self.hub
                                .publish(BroadcastEnvelope { channel, event, data })
                                .await;
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

        // Connection closed — publish presence.left for any remaining
        // presence subscriptions, then abort all forwarder tasks.
        let mut map = forwarders.lock().await;
        for (channel, entry) in map.drain() {
            if let Some(ps) = entry.presence {
                self.hub.untrack_member(&channel, &ps.member_id).await;
                self.hub
                    .publish(BroadcastEnvelope {
                        channel: channel.clone(),
                        event: "presence.left".into(),
                        data: ps.info,
                    })
                    .await;
            }
            entry.handle.abort();
        }
        Ok(())
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
    socket: &mut WsSocket,
) -> Result<(), FrameworkError> {
    // Resolve the channel from the registry.
    let Some(ch) = registry.resolve(channel) else {
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
    if !ch.authorize(req, data).await {
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
    let presence_bootstrap: Option<(Vec<Value>, String, Value)> = if let Some(pc) = ch.presence_info()
    {
        let existing = hub.list_members(channel).await;
        let info = pc.member_info(req).await?;
        let member_id = Uuid::new_v4().to_string();
        Some((existing, member_id, info))
    } else {
        None
    };

    // Subscribe to the hub and spawn a forwarder.
    let mut rx = hub.subscribe(channel);
    let tx = outbound_tx.clone();
    let forwarder = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(envelope) => {
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
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Lag: the subscriber fell behind the ring buffer.
                    // v1 skips missed frames and continues.  Phase 7B+
                    // can surface a "lagged" event so the client can
                    // refetch state.
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
                hub.publish(BroadcastEnvelope {
                    channel: channel.to_string(),
                    event: "presence.left".into(),
                    data: ps.info,
                })
                .await;
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
        hub.publish(BroadcastEnvelope {
            channel: channel.to_string(),
            event: "presence.joined".into(),
            data: info,
        })
        .await;
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
            hub.publish(BroadcastEnvelope {
                channel: channel.to_string(),
                event: "presence.left".into(),
                data: ps.info,
            })
            .await;
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
