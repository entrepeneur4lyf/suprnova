//! Per-route WS middleware tests — registration shape, middleware
//! invocation on upgrade, short-circuit on non-2xx response.

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use suprnova::http::{Request, Response};
use suprnova::middleware::{into_boxed, Middleware, Next};
use suprnova::routing::Router;
use suprnova::ws::{WebSocketHandler, WsSocket};
use suprnova::{FrameworkError, HttpResponse};

struct NoopHandler;

#[async_trait]
impl WebSocketHandler for NoopHandler {
    async fn handle(&self, _socket: WsSocket, _req: Request) -> Result<(), FrameworkError> {
        Ok(())
    }
}

struct CountingMiddleware {
    counter: Arc<AtomicUsize>,
}

#[async_trait]
impl Middleware for CountingMiddleware {
    async fn handle(&self, req: Request, next: Next) -> Response {
        self.counter.fetch_add(1, Ordering::SeqCst);
        next(req).await
    }
}

struct Rejector;

#[async_trait]
impl Middleware for Rejector {
    async fn handle(&self, _req: Request, _next: Next) -> Response {
        Err(HttpResponse::text("unauthorized").status(401))
    }
}

#[test]
fn ws_route_with_middleware_surfaces_in_match() {
    let counter = Arc::new(AtomicUsize::new(0));
    let router = Router::new().ws_with_middleware(
        "/ws/protected",
        NoopHandler,
        vec![into_boxed(CountingMiddleware {
            counter: counter.clone(),
        })],
    );
    let m = router.match_ws("/ws/protected").expect("matches");
    assert_eq!(m.middleware().len(), 1, "one middleware attached");
}

#[test]
fn ws_route_without_middleware_has_empty_chain() {
    let router = Router::new().ws("/ws/public", NoopHandler);
    let m = router.match_ws("/ws/public").expect("matches");
    assert!(m.middleware().is_empty());
}

#[test]
fn ws_route_def_chained_middleware_surfaces_in_match() {
    use suprnova::routing::WsRouteDef;
    let counter = Arc::new(AtomicUsize::new(0));
    // WsRouteDef::new is used directly (same shape as __ws_impl + ws! macro)
    let route_def = WsRouteDef::new("/ws/macro-protected", NoopHandler)
        .middleware(CountingMiddleware { counter: counter.clone() })
        .middleware(Rejector);
    let router = route_def.register(Router::new());
    let m = router.match_ws("/ws/macro-protected").expect("matches");
    assert_eq!(m.middleware().len(), 2, "two middleware attached via WsRouteDef chaining");
}

#[test]
fn ws_macro_middleware_chaining_compiles_and_surfaces_in_match() {
    // Pin that the ws! macro syntax (headline API) chains middleware
    // correctly end-to-end. ws! expands to __ws_impl → WsRouteDef::new,
    // so this also validates the macro expansion path.
    let router = suprnova::ws!("/ws/gated", NoopHandler)
        .middleware(Rejector)
        .register(Router::new());
    let m = router.match_ws("/ws/gated").expect("matches");
    assert_eq!(m.middleware().len(), 1, "ws! macro .middleware() chain");
}

// The end-to-end "middleware actually runs on upgrade" assertion
// lands in broadcasting_e2e.rs (T5) where we have the full upgrade
// fixture. This file pins the wiring.
