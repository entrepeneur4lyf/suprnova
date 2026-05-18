//! `routes!` macro `ws!(...)` integration tests.
//!
//! The `routes!` macro expands to `pub fn register() -> Router`,
//! so each test invokes the macro inside an inner module and
//! calls `register()` to materialize the router.

use async_trait::async_trait;
use suprnova::http::Request;
use suprnova::ws::{WebSocketHandler, WsSocket};
use suprnova::FrameworkError;

#[derive(Clone)]
pub struct NoopHandler;

#[async_trait]
impl WebSocketHandler for NoopHandler {
    async fn handle(
        &self,
        _socket: WsSocket,
        _request: Request,
    ) -> Result<(), FrameworkError> {
        Ok(())
    }
}

mod ws_only_routes {
    use super::NoopHandler;
    use suprnova::{routes, ws};

    routes! {
        ws!("/ws/echo", NoopHandler),
    }
}

#[test]
fn routes_macro_supports_ws_form() {
    let router = ws_only_routes::register();
    assert!(router.match_ws("/ws/echo").is_some());
}

mod mixed_http_and_ws_routes {
    use super::NoopHandler;
    use suprnova::http::{Request, Response};
    use suprnova::{get, routes, ws};

    async fn home(_req: Request) -> Response {
        suprnova::http::text("hi")
    }

    routes! {
        get!("/", home).name("home"),
        ws!("/ws/echo", NoopHandler),
    }
}

#[test]
fn routes_macro_mixes_http_and_ws() {
    let router = mixed_http_and_ws_routes::register();
    assert!(router.match_ws("/ws/echo").is_some(), "ws route registered");
    assert!(
        router.match_route(&hyper::Method::GET, "/").is_some(),
        "http route registered alongside"
    );
}
