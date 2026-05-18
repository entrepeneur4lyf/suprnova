//! Per-connection ping heartbeat + close-on-no-pong enforcement.
//!
//! Spawned alongside the handler task; sends `Ping(b"")` at a
//! configurable interval. On each ping send the shared `missed_pings`
//! counter is incremented. When the peer responds with a Pong,
//! `WsSocket`'s recv path resets the counter to 0. If the counter
//! reaches `max_missed`, the heartbeat sends a Close(1011) frame and
//! returns — the connection is considered dead.
//!
//! The caller is responsible for aborting this task (`spawn(run(...))
//! .abort_handle()`) when the handler future resolves. See the
//! `WsSocket::sender()` doc for the teardown contract.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{
    protocol::{frame::coding::CloseCode, CloseFrame},
    Message,
};

/// Drive periodic pings and enforce close-on-no-pong.
///
/// The caller passes an `mpsc::Sender<Message>` (obtained from
/// `WsSocket::sender()`) that feeds into the websocket sink via the
/// forwarder task.
///
/// `missed_pings` is shared with `WsSocket`'s recv path: recv resets
/// it to 0 on each Pong; this task increments it on each Ping send.
/// When `missed_pings >= max_missed`, a Close(1011) frame is sent and
/// the task returns.
///
/// Set `max_missed` to `usize::MAX` to disable close-on-no-pong (the
/// task will still send pings but never close the connection for lack
/// of pong response).
pub async fn run(
    sender: mpsc::Sender<Message>,
    interval: Duration,
    missed_pings: Arc<AtomicUsize>,
    max_missed: usize,
) {
    let mut ticker = tokio::time::interval(interval);
    // The first tick fires immediately; skip it so the peer gets
    // at least one full interval of grace before the first ping.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        // Empty payload is the conventional "are you alive" ping.
        // tungstenite 0.29's Ping variant takes bytes::Bytes.
        if sender
            .send(Message::Ping(bytes::Bytes::new()))
            .await
            .is_err()
        {
            // Forwarder dropped — connection over (or caller aborted).
            return;
        }

        // Increment the missed-ping counter. If the new value meets or
        // exceeds the threshold, close with 1011 ("internal error" —
        // RFC 6455 §7.4 uses this for unexpected server-side conditions,
        // which includes a peer that stopped responding to pings).
        let prev = missed_pings.fetch_add(1, Ordering::AcqRel);
        if prev + 1 >= max_missed {
            let close = Message::Close(Some(CloseFrame {
                code: CloseCode::Error, // wire code 1011
                reason: "no pong response".into(),
            }));
            let _ = sender.send(close).await;
            return;
        }
    }
}
