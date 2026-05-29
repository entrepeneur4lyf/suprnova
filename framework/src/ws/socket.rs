//! `WsSocket` — typed send/recv API over a `tokio_tungstenite::WebSocketStream`.
//!
//! Handlers see this, not the raw tungstenite stream. Internally we split
//! the stream into Sink + Stream halves: a forwarder task owns the sink
//! and drains an mpsc; the handler-facing send methods push into the mpsc.
//! This means the framework can also push messages (heartbeat pings, future
//! broadcaster fanout) without locking the handler's send path.

use crate::error::FrameworkError;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{
    Message,
    protocol::{CloseFrame, frame::coding::CloseCode},
};

/// Channel depth between handler/heartbeat senders and the forwarder
/// task. 32 is comfortably above the typical interleave (1 ping + a
/// burst of handler sends) and bounded so a pathological handler
/// can't OOM the process.
const SEND_CHANNEL_CAPACITY: usize = 32;

/// A bidirectional WebSocket connection.
///
/// `send_text` / `send_binary` enqueue onto an internal mpsc that a
/// dedicated forwarder task drains into the underlying sink. The
/// receiver half of the stream is owned directly by `WsSocket` — only
/// the handler reads, so no split is needed there.
pub struct WsSocket {
    sender: mpsc::Sender<Outbound>,
    receiver: ReceiverHalf,
    missed_pings: Arc<AtomicUsize>,
    /// JoinHandle of the spawned forwarder task. The framework's
    /// upgrade path extracts it via [`WsSocket::take_forwarder_handle`]
    /// before moving the socket into the handler future, so it can
    /// `.await` the forwarder after the handler returns and `outbound`
    /// is dropped — ensuring the close handshake completes before the
    /// connection's task is reported as joined to `WS_TASKS`.
    ///
    /// `None` after `take_forwarder_handle` is called (the framework
    /// owns the JoinHandle from that point on). `None` is harmless on
    /// drop because the forwarder is detached and self-terminates when
    /// all `Sender<Outbound>` clones drop.
    forwarder_handle: Option<JoinHandle<()>>,
}

/// What the forwarder task drains. `Close(_)` is special-cased so the
/// forwarder can finish the sink cleanly and drop.
enum Outbound {
    Msg(Message),
    Close(CloseFrame),
}

/// Type-erased receiver half so `WsSocket` doesn't have to be generic
/// in the public API. We box the stream behind a trait object that
/// exposes only `next()`.
type ReceiverHalf = std::pin::Pin<
    Box<dyn futures_util::Stream<Item = tokio_tungstenite::tungstenite::Result<Message>> + Send>,
>;

impl WsSocket {
    /// Build a `WsSocket` from a fully-upgraded `WebSocketStream`.
    /// Spawns the forwarder task that drains the outbound mpsc into
    /// the sink half of the stream.
    pub fn from_stream<S>(stream: WebSocketStream<S>) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        Self::from_stream_with_heartbeat(stream, Arc::new(AtomicUsize::new(0)))
    }

    /// Build a `WsSocket` with a shared missed-pings counter.
    ///
    /// The counter is incremented by the heartbeat task on each ping
    /// send and reset to 0 by `WsSocket`'s recv path whenever a Pong
    /// is received from the peer. Pass the same `Arc` to
    /// `heartbeat::run` so the two halves share state.
    pub fn from_stream_with_heartbeat<S>(
        stream: WebSocketStream<S>,
        missed_pings: Arc<AtomicUsize>,
    ) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (sink, stream) = stream.split();
        let (tx, rx) = mpsc::channel(SEND_CHANNEL_CAPACITY);
        let forwarder_handle = tokio::spawn(forwarder_task(sink, rx));
        Self {
            sender: tx,
            receiver: Box::pin(stream),
            missed_pings,
            forwarder_handle: Some(forwarder_handle),
        }
    }

    /// Hand over the forwarder JoinHandle to the framework's upgrade
    /// path. Called once, before the socket is moved into the user's
    /// handler future, so the upgrade path can `.await` the forwarder's
    /// drain after the handler returns. Subsequent calls return `None`.
    ///
    /// The forwarder is detached after `WsSocket` is constructed; if a
    /// caller never extracts the handle the task still self-terminates
    /// when all `Sender` clones drop. The point of explicit extraction
    /// is *waiting* for that termination so the WS_TASKS drain
    /// transitively covers the forwarder rather than racing it.
    pub(crate) fn take_forwarder_handle(&mut self) -> Option<JoinHandle<()>> {
        self.forwarder_handle.take()
    }

    /// Clone the outbound channel sender, wrapped to expose `Message`
    /// (not the internal `Outbound`). Used internally by the framework
    /// to spawn auxiliary tasks (heartbeat pings, broadcaster fanout)
    /// that can push messages without contending with the handler's
    /// `send_*` methods.
    ///
    /// # Close-frame fast path
    ///
    /// A `Message::Close` received on this bridge is rewrapped as
    /// `Outbound::Close`, taking the explicit close path through the
    /// forwarder (the sink is `close()`'d and the forwarder task
    /// terminates). Without this mapping, a heartbeat or fanout task
    /// that sends a close frame would just enqueue an `Outbound::Msg`
    /// the forwarder writes to the wire but never acts on — the
    /// underlying sink would stay open until every other Sender
    /// dropped, defeating the close intent.
    ///
    /// A `Message::Close` without a payload becomes a default `CloseFrame`
    /// (code 1000, empty reason) so the forwarder's `Outbound::Close`
    /// arm has something to send. Callers that care about close codes
    /// should always pass `Message::Close(Some(frame))`.
    ///
    /// # Caller contract
    ///
    /// The caller **must** drop or abort the returned `Sender` before
    /// (or alongside) the `WsSocket` itself. The bridge task spawned
    /// here holds an internal `Sender<Outbound>` clone for the lifetime
    /// of the returned `Sender<Message>`; if it outlives the WsSocket,
    /// the forwarder task will not detect channel-close and the
    /// underlying TCP connection will remain open until the peer
    /// drops it.
    ///
    /// In practice this means the framework spawns the auxiliary task
    /// with an `AbortHandle` and aborts it when the handler future
    /// resolves (see `server::handle_ws_upgrade`).
    ///
    /// # Multiple callers
    ///
    /// Each invocation spawns a fresh bridge task and adds an extra
    /// `SEND_CHANNEL_CAPACITY`-deep buffer. Call once per connection.
    pub(crate) fn sender(&self) -> mpsc::Sender<Message> {
        let (bridge_tx, mut bridge_rx) = mpsc::channel::<Message>(SEND_CHANNEL_CAPACITY);
        let internal = self.sender.clone();
        tokio::spawn(async move {
            while let Some(msg) = bridge_rx.recv().await {
                let outbound = match msg {
                    Message::Close(Some(frame)) => Outbound::Close(frame),
                    Message::Close(None) => Outbound::Close(CloseFrame {
                        code: CloseCode::Normal,
                        reason: Default::default(),
                    }),
                    other => Outbound::Msg(other),
                };
                let was_close = matches!(outbound, Outbound::Close(_));
                if internal.send(outbound).await.is_err() {
                    return;
                }
                if was_close {
                    // The forwarder finishes the sink on Outbound::Close.
                    // Anything we forward after that hits a dropped receiver
                    // (or, worse, races a half-closed sink). Stop the bridge.
                    return;
                }
            }
        });
        bridge_tx
    }

    /// Send a text frame.
    pub async fn send_text(&mut self, text: impl Into<String>) -> Result<(), FrameworkError> {
        self.sender
            .send(Outbound::Msg(Message::text(text.into())))
            .await
            .map_err(|_| FrameworkError::internal("ws send: connection closed"))
    }

    /// Send a binary frame.
    pub async fn send_binary(&mut self, bytes: impl Into<Vec<u8>>) -> Result<(), FrameworkError> {
        let data: Vec<u8> = bytes.into();
        self.sender
            .send(Outbound::Msg(Message::binary(data)))
            .await
            .map_err(|_| FrameworkError::internal("ws send: connection closed"))
    }

    /// Receive the next text message, skipping non-text frames that
    /// the handler isn't expected to care about. Returns `Ok(None)`
    /// when the peer closes or the connection ends.
    pub async fn recv_text(&mut self) -> Result<Option<String>, FrameworkError> {
        loop {
            match self.receiver.next().await {
                Some(Ok(Message::Text(t))) => return Ok(Some(t.to_string())),
                Some(Ok(Message::Binary(_))) => continue,
                Some(Ok(Message::Pong(_))) => {
                    // Pong from peer — reset the missed-ping counter.
                    self.missed_pings.store(0, Ordering::Release);
                    continue;
                }
                Some(Ok(Message::Ping(_))) => continue,
                Some(Ok(Message::Close(_))) | None => return Ok(None),
                Some(Ok(Message::Frame(_))) => continue,
                Some(Err(e)) => return Err(FrameworkError::internal(format!("ws recv: {e}"))),
            }
        }
    }

    /// Receive the next message of any type.
    pub async fn recv(&mut self) -> Result<Option<Message>, FrameworkError> {
        match self.receiver.next().await {
            Some(Ok(msg)) => {
                if matches!(msg, Message::Pong(_)) {
                    // Pong from peer — reset the missed-ping counter.
                    self.missed_pings.store(0, Ordering::Release);
                }
                Ok(Some(msg))
            }
            Some(Err(e)) => Err(FrameworkError::internal(format!("ws recv: {e}"))),
            None => Ok(None),
        }
    }

    /// Send a close frame. Idempotent — subsequent sends will Err
    /// because the forwarder will have terminated.
    pub async fn close(
        &mut self,
        code: u16,
        reason: impl Into<String>,
    ) -> Result<(), FrameworkError> {
        let frame = CloseFrame {
            code: CloseCode::from(code),
            reason: reason.into().into(),
        };
        self.sender
            .send(Outbound::Close(frame))
            .await
            .map_err(|_| FrameworkError::internal("ws close: connection already closed"))
    }
}

/// Forwarder task: owns the sink half of the WebSocket stream and
/// drains the outbound mpsc into it. Exits cleanly when the channel
/// closes (all `Sender`s dropped) or after a Close frame is sent.
async fn forwarder_task<S>(
    mut sink: futures_util::stream::SplitSink<WebSocketStream<S>, Message>,
    mut rx: mpsc::Receiver<Outbound>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    while let Some(outbound) = rx.recv().await {
        match outbound {
            Outbound::Msg(msg) => {
                if let Err(e) = sink.send(msg).await {
                    tracing::warn!(error = %e, "ws forwarder send failed; closing");
                    let _ = sink.close().await;
                    return;
                }
            }
            Outbound::Close(frame) => {
                let _ = sink.send(Message::Close(Some(frame))).await;
                let _ = sink.close().await;
                return;
            }
        }
    }
    // Channel closed — drop the sink to release the connection.
    let _ = sink.close().await;
}
