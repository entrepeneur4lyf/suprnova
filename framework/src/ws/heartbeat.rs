//! Per-connection ping heartbeat.
//!
//! Spawned alongside the handler task; sends `Ping(b"")` at a
//! configurable interval. tokio-tungstenite auto-responds to peer
//! pings (its default), so this only covers the framework-initiated
//! direction. Close-on-no-pong lands in 7B alongside broadcasting.

use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Drive periodic pings. The caller passes an `mpsc::Sender<Message>`
/// (obtained from `WsSocket::sender()`) that feeds into the websocket
/// sink via the forwarder task. This task does NOT exit on its own
/// when the connection ends — the caller is responsible for aborting
/// the task (`spawn(run(...)).abort_handle()`) when the handler
/// future resolves, otherwise the bridge task spawned by
/// `WsSocket::sender()` keeps an internal sender clone alive, the
/// forwarder doesn't see channel-close, and the TCP connection lingers.
/// See the `WsSocket::sender()` doc for details.
pub async fn run(sender: mpsc::Sender<Message>, interval: Duration) {
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
    }
}
