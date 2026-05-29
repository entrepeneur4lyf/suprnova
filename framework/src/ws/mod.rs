//! WebSocket route primitive.
//!
//! Register WS handlers via `Router::ws(path, handler)` or the
//! `routes!` macro's `r.ws(...)` form. The server upgrades any
//! `Upgrade: websocket` request whose path matches; the handler
//! receives a [`WsSocket`] plus the original [`Request`] so it can
//! read cookies, session, headers, and captured route params.
//!
//! # Example
//!
//! ```rust,ignore
//! use async_trait::async_trait;
//! use suprnova::{FrameworkError, http::Request, ws::{WebSocketHandler, WsSocket}};
//!
//! pub struct EchoHandler;
//!
//! #[async_trait]
//! impl WebSocketHandler for EchoHandler {
//!     async fn handle(&self, mut socket: WsSocket, _req: Request) -> Result<(), FrameworkError> {
//!         while let Some(text) = socket.recv_text().await? {
//!             socket.send_text(format!("echo: {text}")).await?;
//!         }
//!         Ok(())
//!     }
//! }
//! ```

use crate::error::FrameworkError;
use crate::http::Request;
use async_trait::async_trait;
use std::sync::Arc;

mod socket;
pub use socket::WsSocket;

pub mod heartbeat;

/// Handle a single WebSocket connection. The framework upgrades the
/// HTTP request, builds a [`WsSocket`], and calls `handle`.
///
/// Returning `Ok(())` triggers a clean close (code 1000); returning
/// `Err(_)` logs the error and closes with code 1011 (internal error).
#[async_trait]
pub trait WebSocketHandler: Send + Sync + 'static {
    async fn handle(&self, socket: WsSocket, request: Request) -> Result<(), FrameworkError>;
}

/// Origin-header validation policy applied at WebSocket upgrade time.
///
/// Browsers always send an `Origin` header on WebSocket handshakes. Unlike
/// `fetch()` / `XMLHttpRequest`, browser WebSocket requests are not protected
/// by CSRF token middleware (the upgrade carries no token), so a same-origin
/// check on `Origin` is the only thing standing between a malicious page and
/// a privileged WS endpoint on a logged-in user's session. The framework
/// enforces this policy before [`hyper_tungstenite::upgrade`](https://docs.rs/hyper-tungstenite)
/// is called; a policy-violation returns HTTP 403 with no upgrade.
///
/// Non-browser clients (servers, CLIs, native apps) typically don't send an
/// `Origin` header. Routes that serve non-browser clients exclusively should
/// use [`OriginPolicy::AllowAny`]; routes serving both browsers and non-
/// browsers should use [`OriginPolicy::AllowList`] with the production
/// frontend origins.
#[derive(Clone, Debug, Default)]
pub enum OriginPolicy {
    /// Default. Allow upgrades only when the request's `Origin` host (and
    /// port, if present in `Origin`) matches the request's `Host` header.
    /// A missing `Origin` is rejected. Scheme is not compared (TLS is
    /// terminated upstream of a typical Suprnova process, so the server
    /// can't reliably tell whether the public scheme was https or http).
    #[default]
    SameOrigin,
    /// Skip origin validation. Suitable for non-browser endpoints (server-
    /// to-server, native apps, test mocks). DO NOT use this for browser
    /// endpoints that touch authenticated state.
    AllowAny,
    /// Allow upgrades only when the request's `Origin` header value is an
    /// exact case-insensitive match for one of the supplied origins. Each
    /// entry is the full `scheme://host[:port]` form a browser would send
    /// (e.g. `"https://app.example.com"`).
    AllowList(Vec<String>),
}

/// Per-route WebSocket configuration.
#[derive(Clone, Debug)]
pub struct WsConfig {
    /// Interval between framework-sent pings. Default 30s.
    pub ping_interval: std::time::Duration,
    /// Max message size in bytes. Default 64 MiB.
    pub max_message_size: usize,
    /// Max single-frame size in bytes. Default 16 MiB.
    pub max_frame_size: usize,
    /// Consecutive missed pongs before the connection is closed
    /// with code 1011. Default: 2. Set to `usize::MAX` to disable.
    pub max_missed_pings: usize,
    /// Origin header policy enforced at upgrade time. See [`OriginPolicy`].
    /// Default: [`OriginPolicy::SameOrigin`].
    pub origin_policy: OriginPolicy,
}

impl Default for WsConfig {
    fn default() -> Self {
        Self {
            ping_interval: std::time::Duration::from_secs(30),
            max_message_size: 64 * 1024 * 1024,
            max_frame_size: 16 * 1024 * 1024,
            max_missed_pings: 2,
            origin_policy: OriginPolicy::default(),
        }
    }
}

impl WsConfig {
    /// Convert to tungstenite's `WebSocketConfig` for passing to
    /// `hyper_tungstenite::upgrade`.
    #[allow(dead_code)] // used by upgrade wiring in T5
    pub(crate) fn to_tungstenite_config(
        &self,
    ) -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
        let mut cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
        cfg.max_message_size = Some(self.max_message_size);
        cfg.max_frame_size = Some(self.max_frame_size);
        cfg
    }
}

/// Type-erased boxed handler used internally by the router.
pub type BoxedWebSocketHandler = Arc<dyn WebSocketHandler>;
