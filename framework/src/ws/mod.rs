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
    /// Max reassembled message size in bytes. Default 1 MiB —
    /// safe for public, browser-facing endpoints. Routes that
    /// expect larger payloads (file upload, audio streaming,
    /// trusted internal feeds) should raise this explicitly, or
    /// start from [`WsConfig::generous`].
    pub max_message_size: usize,
    /// Max single-frame size in bytes. Default 64 KiB — fits
    /// typical chat / notification frames with headroom. Routes
    /// sending unfragmented large frames should raise this
    /// explicitly, or start from [`WsConfig::generous`].
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
            // Public-endpoint-safe defaults. Each upgraded
            // connection costs a tungstenite buffer sized to
            // `max_message_size`, so generous defaults are a DoS
            // foot-gun on routes open to the internet. Trusted
            // feeds opt into the higher limits via
            // [`WsConfig::generous`].
            max_message_size: 1024 * 1024,
            max_frame_size: 64 * 1024,
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

    /// Generous-limit variant of [`WsConfig::default`] for trusted
    /// internal feeds (server-to-server fan-out, bulk data export,
    /// large binary transfers). Raises `max_message_size` to 64 MiB
    /// and `max_frame_size` to 16 MiB; other fields keep their
    /// defaults.
    ///
    /// Do not use on routes reachable from the public internet
    /// without an explicit decision — every active connection
    /// reserves a buffer sized to `max_message_size`, and these
    /// limits multiply across concurrent sockets.
    ///
    /// ```rust,ignore
    /// use suprnova::ws::WsConfig;
    /// let cfg = WsConfig::generous();
    /// assert_eq!(cfg.max_message_size, 64 * 1024 * 1024);
    /// assert_eq!(cfg.max_frame_size, 16 * 1024 * 1024);
    /// ```
    pub fn generous() -> Self {
        Self {
            max_message_size: 64 * 1024 * 1024,
            max_frame_size: 16 * 1024 * 1024,
            ..Self::default()
        }
    }
}

/// Type-erased boxed handler used internally by the router.
pub type BoxedWebSocketHandler = Arc<dyn WebSocketHandler>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity bound on the default. Public WebSocket endpoints
    /// reserve a buffer per connection sized to `max_message_size`;
    /// a generous default is a DoS foot-gun. If a future change
    /// raises this above 4 MiB without flipping the policy
    /// (e.g. lazy buffers, per-route opt-in), this test forces
    /// the decision to be explicit.
    #[test]
    fn default_max_message_size_is_safe_for_public_endpoints() {
        assert!(
            WsConfig::default().max_message_size <= 4 * 1024 * 1024,
            "WsConfig::default().max_message_size = {} — public WS \
             defaults must stay <= 4 MiB; use WsConfig::generous() for \
             trusted-feed deployments",
            WsConfig::default().max_message_size
        );
    }

    #[test]
    fn generous_raises_message_and_frame_limits() {
        let cfg = WsConfig::generous();
        assert_eq!(cfg.max_message_size, 64 * 1024 * 1024);
        assert_eq!(cfg.max_frame_size, 16 * 1024 * 1024);
        // Other fields stay aligned with the public defaults.
        assert_eq!(cfg.ping_interval, WsConfig::default().ping_interval);
        assert_eq!(cfg.max_missed_pings, WsConfig::default().max_missed_pings);
    }
}
