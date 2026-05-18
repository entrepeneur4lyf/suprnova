//! Echo WebSocket handler — Phase 7A dogfood.

use async_trait::async_trait;
use suprnova::http::Request;
use suprnova::ws::{WebSocketHandler, WsSocket};
use suprnova::FrameworkError;

pub struct EchoHandler;

#[async_trait]
impl WebSocketHandler for EchoHandler {
    async fn handle(
        &self,
        mut socket: WsSocket,
        _req: Request,
    ) -> Result<(), FrameworkError> {
        while let Some(text) = socket.recv_text().await? {
            socket.send_text(format!("echo: {text}")).await?;
        }
        Ok(())
    }
}
