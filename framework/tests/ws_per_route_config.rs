//! Per-route WsConfig override tests — assert WsRouteDef.config()
//! threads through the registry into WsMatch.

use async_trait::async_trait;
use std::time::Duration;
use suprnova::FrameworkError;
use suprnova::http::Request;
use suprnova::http::Response;
use suprnova::middleware::{Middleware, Next, into_boxed};
use suprnova::routing::Router;
use suprnova::ws::{WebSocketHandler, WsConfig, WsSocket};

struct NoopHandler;

#[async_trait]
impl WebSocketHandler for NoopHandler {
    async fn handle(&self, _socket: WsSocket, _req: Request) -> Result<(), FrameworkError> {
        Ok(())
    }
}

#[derive(Clone)]
struct PassthroughMiddleware;

#[async_trait]
impl Middleware for PassthroughMiddleware {
    async fn handle(&self, req: Request, next: Next) -> Response {
        next(req).await
    }
}

#[test]
fn ws_route_without_config_surfaces_none() {
    let router = Router::new().ws("/ws/default", NoopHandler);
    let m = router.match_ws("/ws/default").expect("matches");
    assert!(
        m.config().is_none(),
        "no .config() -> WsMatch::config() = None"
    );
}

#[test]
fn ws_route_with_explicit_config_surfaces_it() {
    let cfg = WsConfig {
        ping_interval: Duration::from_secs(5),
        max_message_size: 1024,
        max_frame_size: 512,
        // Explicit non-default value (default is 2); must be >= 2 since a
        // threshold of 1 closes every connection on its first ping.
        max_missed_pings: 4,
        ..Default::default()
    };
    let router = Router::new().ws_with_config("/ws/fast", NoopHandler, cfg);
    let m = router.match_ws("/ws/fast").expect("matches");
    let surfaced = m.config().expect("config set");
    assert_eq!(surfaced.ping_interval, Duration::from_secs(5));
    assert_eq!(surfaced.max_message_size, 1024);
    assert_eq!(surfaced.max_frame_size, 512);
    assert_eq!(surfaced.max_missed_pings, 4);
}

#[test]
fn ws_with_middleware_and_config_surfaces_both() {
    let cfg = WsConfig {
        ping_interval: Duration::from_secs(3),
        max_message_size: 8192,
        max_frame_size: 4096,
        max_missed_pings: 5,
        ..Default::default()
    };
    let router = Router::new().ws_with_middleware_and_config(
        "/ws/combined",
        NoopHandler,
        vec![into_boxed(PassthroughMiddleware)],
        cfg,
    );
    let m = router.match_ws("/ws/combined").expect("matches");
    assert_eq!(m.middleware().len(), 1, "middleware threaded through");
    let surfaced = m.config().expect("config set");
    assert_eq!(surfaced.ping_interval, Duration::from_secs(3));
    assert_eq!(surfaced.max_message_size, 8192);
}

mod ws_macro_with_config {
    use super::NoopHandler;
    use std::time::Duration;
    use suprnova::ws::WsConfig;
    use suprnova::{routes, ws};

    routes! {
        ws!("/ws/macro_cfg", NoopHandler).config(WsConfig {
            ping_interval: Duration::from_secs(10),
            max_message_size: 2048,
            max_frame_size: 1024,
            max_missed_pings: 3,
            ..Default::default()
        }),
    }
}

#[test]
fn ws_macro_config_chain_threads_through() {
    let router = ws_macro_with_config::register();
    let m = router.match_ws("/ws/macro_cfg").expect("matches");
    let cfg = m.config().expect("macro config set");
    assert_eq!(cfg.ping_interval, Duration::from_secs(10));
    assert_eq!(cfg.max_message_size, 2048);
    assert_eq!(cfg.max_frame_size, 1024);
    assert_eq!(cfg.max_missed_pings, 3);
}

mod ws_macro_config_then_middleware {
    use super::NoopHandler;
    use super::PassthroughMiddleware;
    use std::time::Duration;
    use suprnova::ws::WsConfig;
    use suprnova::{routes, ws};

    routes! {
        ws!("/ws/cfg_then_mw", NoopHandler)
            .config(WsConfig { ping_interval: Duration::from_secs(7), ..Default::default() })
            .middleware(PassthroughMiddleware),
    }
}

#[test]
fn ws_macro_config_before_middleware_composes() {
    let router = ws_macro_config_then_middleware::register();
    let m = router.match_ws("/ws/cfg_then_mw").expect("matches");
    let cfg = m.config().expect("config present");
    assert_eq!(cfg.ping_interval, Duration::from_secs(7));
    assert_eq!(m.middleware().len(), 1, "middleware also threaded");
}

mod ws_macro_middleware_then_config {
    use super::NoopHandler;
    use super::PassthroughMiddleware;
    use std::time::Duration;
    use suprnova::ws::WsConfig;
    use suprnova::{routes, ws};

    routes! {
        ws!("/ws/mw_then_cfg", NoopHandler)
            .middleware(PassthroughMiddleware)
            .config(WsConfig { ping_interval: Duration::from_secs(2), ..Default::default() }),
    }
}

#[test]
fn ws_macro_middleware_before_config_composes() {
    let router = ws_macro_middleware_then_config::register();
    let m = router.match_ws("/ws/mw_then_cfg").expect("matches");
    let cfg = m.config().expect("config present");
    assert_eq!(cfg.ping_interval, Duration::from_secs(2));
    assert_eq!(m.middleware().len(), 1, "middleware also threaded");
}
