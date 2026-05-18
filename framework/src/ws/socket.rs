//! `WsSocket` — typed send/recv API over a `tokio_tungstenite::WebSocketStream`.
//!
//! Handlers see this, not the raw tungstenite stream. Internally we split
//! the stream into Sink + Stream halves: a forwarder task owns the sink
//! and drains an mpsc; the handler-facing send methods push into the mpsc.
//! This means the framework can also push messages (heartbeat pings, future
//! broadcaster fanout) without locking the handler's send path.

use crate::error::FrameworkError;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{
    protocol::{frame::coding::CloseCode, CloseFrame},
    Message,
};
use tokio_tungstenite::WebSocketStream;

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
    Box<
        dyn futures_util::Stream<Item = tokio_tungstenite::tungstenite::Result<Message>>
            + Send,
    >,
>;

impl WsSocket {
    /// Build a `WsSocket` from a fully-upgraded `WebSocketStream`.
    /// Spawns the forwarder task that drains the outbound mpsc into
    /// the sink half of the stream.
    pub fn from_stream<S>(stream: WebSocketStream<S>) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (sink, stream) = stream.split();
        let (tx, rx) = mpsc::channel(SEND_CHANNEL_CAPACITY);
        tokio::spawn(forwarder_task(sink, rx));
        Self {
            sender: tx,
            receiver: Box::pin(stream),
        }
    }

    /// Clone the outbound channel sender wrapped to expose `Message`
    /// (not the internal `Outbound`). Used internally by the framework
    /// to spawn heartbeat tasks that can push pings without contending
    /// with the handler's `send_*` methods.
    #[allow(dead_code)] // used by heartbeat task in T8
    pub(crate) fn sender(&self) -> mpsc::Sender<Message> {
        let (bridge_tx, mut bridge_rx) = mpsc::channel::<Message>(SEND_CHANNEL_CAPACITY);
        let internal = self.sender.clone();
        tokio::spawn(async move {
            while let Some(msg) = bridge_rx.recv().await {
                if internal.send(Outbound::Msg(msg)).await.is_err() {
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
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                Some(Ok(Message::Close(_))) | None => return Ok(None),
                Some(Ok(Message::Frame(_))) => continue,
                Some(Err(e)) => return Err(FrameworkError::internal(format!("ws recv: {e}"))),
            }
        }
    }

    /// Receive the next message of any type.
    pub async fn recv(&mut self) -> Result<Option<Message>, FrameworkError> {
        match self.receiver.next().await {
            Some(Ok(msg)) => Ok(Some(msg)),
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
