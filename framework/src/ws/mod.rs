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
//! ```rust,no_run
//! use suprnova::async_trait;
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
    /// Run the per-connection handler loop. Returning `Ok(())` triggers
    /// a clean close (code 1000); returning `Err(_)` logs the error and
    /// closes with code 1011 (internal error).
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
    /// Accepted application-level subprotocols for `Sec-WebSocket-Protocol`
    /// negotiation. Empty (the default) skips negotiation — the upgrade
    /// response omits `Sec-WebSocket-Protocol` and the client falls back
    /// to its default protocol handling.
    ///
    /// When non-empty, the upgrade picks the first client-offered token
    /// (read from the request's `Sec-WebSocket-Protocol` header, in client
    /// preference order per RFC 6455 §4.2.2) that appears in this list,
    /// and echoes it on the 101 handshake response. If the client offered
    /// protocols but none match the accepted list, the upgrade still
    /// succeeds with no `Sec-WebSocket-Protocol` header — RFC 6455
    /// requires browsers to fail the connection in that case, which is
    /// the correct behavior (a server that proceeds without negotiating
    /// would be silently speaking the wrong protocol).
    ///
    /// Compare case-insensitively on protocol tokens; protocol names are
    /// ASCII per the RFC.
    ///
    /// ```rust,no_run
    /// use suprnova::ws::WsConfig;
    /// let cfg = WsConfig {
    ///     accepted_protocols: vec!["graphql-transport-ws".into(), "graphql-ws".into()],
    ///     ..Default::default()
    /// };
    /// ```
    pub accepted_protocols: Vec<String>,
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
            accepted_protocols: Vec::new(),
        }
    }
}

impl WsConfig {
    /// Convert to tungstenite's `WebSocketConfig` for passing to
    /// `hyper_tungstenite::upgrade`.
    pub(crate) fn to_tungstenite_config(
        &self,
    ) -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
        let mut cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
        cfg.max_message_size = Some(self.max_message_size);
        cfg.max_frame_size = Some(self.max_frame_size);
        cfg
    }

    /// Validate the config's runtime-fatal invariants. Run at WS
    /// registration time so a misconfigured route fails boot rather
    /// than panicking inside the per-connection task at first use:
    ///
    /// - `ping_interval` is non-zero. `tokio::time::interval(Duration::ZERO)`
    ///   panics on construction; the heartbeat task would tear down the
    ///   process the first time the route was hit.
    /// - `max_missed_pings` is at least 2. The heartbeat increments the
    ///   counter on each ping send and checks the bound before the peer
    ///   has had its grace interval to pong, so a threshold of 1 closes
    ///   the connection with 1011 on the very first tick — identical to
    ///   the rejected value 0. A usable threshold needs at least one grace
    ///   cycle (ping, then a pong can reset the counter). Routes that want
    ///   to disable close-on-no-pong should set this to [`usize::MAX`],
    ///   which the heartbeat path documents.
    ///
    /// Size knobs (`max_message_size`, `max_frame_size`) are intentionally
    /// not validated here: their safety is a per-deployment policy, not a
    /// runtime invariant, and the public-safe defaults plus
    /// [`WsConfig::generous`] cover the two common shapes. See the field
    /// docs on `WsConfig` for the rationale.
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.ping_interval.is_zero() {
            return Err(
                "WsConfig.ping_interval must be > 0 (tokio::time::interval panics on Duration::ZERO)",
            );
        }
        if self.max_missed_pings < 2 {
            return Err(
                "WsConfig.max_missed_pings must be >= 2 (1 closes every connection on its first ping, before a pong can reset the counter); set usize::MAX to disable close-on-no-pong",
            );
        }
        Ok(())
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
    /// ```rust,no_run
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

/// Negotiate a single `Sec-WebSocket-Protocol` token from the client's
/// offer list and the route's `accepted_protocols` list.
///
/// `client_offer` is the raw `Sec-WebSocket-Protocol` header value the
/// client sent: a comma-separated list of protocol tokens in client
/// preference order (RFC 6455 §4.2.2). `accepted` is the server's list
/// of acceptable protocols (the route's `WsConfig::accepted_protocols`).
///
/// Returns the first client-offered protocol that appears in `accepted`,
/// matched case-insensitively per RFC 6455 (protocol tokens are ASCII).
/// The returned value preserves the casing from `accepted` so the
/// server's canonical spelling is what the client sees on the 101.
///
/// Returns `None` when:
/// - `accepted` is empty (negotiation disabled — server is protocol-agnostic),
/// - the client did not send `Sec-WebSocket-Protocol` (`client_offer` is `None`),
/// - none of the client's offered tokens overlap with `accepted`.
pub(crate) fn negotiate_subprotocol(
    accepted: &[String],
    client_offer: Option<&str>,
) -> Option<String> {
    if accepted.is_empty() {
        return None;
    }
    let offer = client_offer?;
    for token in offer.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        for accept in accepted {
            if accept.eq_ignore_ascii_case(token) {
                return Some(accept.clone());
            }
        }
    }
    None
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

    /// Default config must pass validation — a default ctor that
    /// produces an invalid config would block every registration.
    #[test]
    fn default_config_is_valid() {
        WsConfig::default().validate().expect("default valid");
        WsConfig::generous().validate().expect("generous valid");
    }

    /// `tokio::time::interval(Duration::ZERO)` panics on construction,
    /// so the heartbeat would tear down the connection task the first
    /// time the route was hit. We refuse the config at registration
    /// time instead.
    #[test]
    fn zero_ping_interval_rejected() {
        let cfg = WsConfig {
            ping_interval: std::time::Duration::ZERO,
            ..Default::default()
        };
        let err = cfg.validate().expect_err("zero ping_interval invalid");
        assert!(err.contains("ping_interval"), "msg: {err}");
    }

    /// `max_missed_pings = 0` means the very first ping send increments
    /// past the threshold and closes the connection with 1011 before
    /// the peer can possibly pong. That's not a useful runtime mode —
    /// to disable close-on-no-pong, use `usize::MAX`.
    #[test]
    fn zero_max_missed_pings_rejected() {
        let cfg = WsConfig {
            max_missed_pings: 0,
            ..Default::default()
        };
        let err = cfg.validate().expect_err("zero max_missed_pings invalid");
        assert!(err.contains("max_missed_pings"), "msg: {err}");
    }

    /// `max_missed_pings = 1` passes a naive "non-zero" check but behaves
    /// identically to `0`: the heartbeat increments the counter to 1 on
    /// the first ping send and the `>= max_missed` check fires before the
    /// peer's grace interval, closing every connection on its first tick.
    /// A usable threshold needs at least one grace cycle, so validation
    /// must reject 1 too.
    #[test]
    fn one_max_missed_pings_rejected() {
        let cfg = WsConfig {
            max_missed_pings: 1,
            ..Default::default()
        };
        let err = cfg.validate().expect_err("one max_missed_pings invalid");
        assert!(err.contains("max_missed_pings"), "msg: {err}");
    }

    /// The smallest threshold that affords a grace cycle (ping, then a
    /// pong can reset the counter) must pass validation — it's the
    /// boundary the runtime relies on.
    #[test]
    fn two_max_missed_pings_is_valid() {
        let cfg = WsConfig {
            max_missed_pings: 2,
            ..Default::default()
        };
        cfg.validate()
            .expect("two is the smallest usable threshold");
    }

    #[test]
    fn usize_max_missed_pings_disables_close_and_is_valid() {
        let cfg = WsConfig {
            max_missed_pings: usize::MAX,
            ..Default::default()
        };
        cfg.validate()
            .expect("usize::MAX is the documented disable");
    }

    /// Empty `accepted_protocols` skips negotiation regardless of
    /// what the client offered — the upgrade proceeds protocol-agnostic.
    #[test]
    fn negotiate_returns_none_when_accepted_empty() {
        assert_eq!(negotiate_subprotocol(&[], Some("graphql-ws")), None);
        assert_eq!(negotiate_subprotocol(&[], None), None);
    }

    #[test]
    fn negotiate_returns_none_when_client_offers_nothing() {
        let accepted = vec!["graphql-ws".to_string()];
        assert_eq!(negotiate_subprotocol(&accepted, None), None);
    }

    /// Client preference order wins: when the client offers multiple,
    /// we pick the FIRST client offer that the server accepts.
    #[test]
    fn negotiate_picks_first_client_offer_in_accepted_list() {
        let accepted = vec!["graphql-transport-ws".to_string(), "graphql-ws".to_string()];
        // Client prefers graphql-ws; we honor that even though
        // graphql-transport-ws comes first in accepted.
        let pick = negotiate_subprotocol(&accepted, Some("graphql-ws, graphql-transport-ws"));
        assert_eq!(pick.as_deref(), Some("graphql-ws"));
    }

    #[test]
    fn negotiate_skips_unknown_client_offers() {
        let accepted = vec!["jsonrpc-2.0".to_string()];
        let pick = negotiate_subprotocol(&accepted, Some("mqtt, jsonrpc-2.0, custom-x"));
        assert_eq!(pick.as_deref(), Some("jsonrpc-2.0"));
    }

    /// Case-insensitive match per RFC 6455; preserve server casing in
    /// the response so the client sees the canonical spelling.
    #[test]
    fn negotiate_case_insensitive_and_preserves_server_case() {
        let accepted = vec!["GraphQL-WS".to_string()];
        let pick = negotiate_subprotocol(&accepted, Some("graphql-ws"));
        assert_eq!(pick.as_deref(), Some("GraphQL-WS"));
    }

    #[test]
    fn negotiate_returns_none_on_no_overlap() {
        let accepted = vec!["graphql-ws".to_string()];
        assert_eq!(
            negotiate_subprotocol(&accepted, Some("jsonrpc-2.0, mqtt")),
            None
        );
    }

    #[test]
    fn negotiate_tolerates_extra_whitespace_and_empty_tokens() {
        let accepted = vec!["jsonrpc-2.0".to_string()];
        // Header per RFC 7230 allows OWS around list separators.
        let pick = negotiate_subprotocol(&accepted, Some("  , jsonrpc-2.0 ,  ,"));
        assert_eq!(pick.as_deref(), Some("jsonrpc-2.0"));
    }
}
