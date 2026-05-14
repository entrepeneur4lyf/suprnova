//! Logging middleware - logs all requests via the framework's `tracing` integration.

use suprnova::{async_trait, current_request_id, Middleware, Next, Request, Response};

/// Middleware that logs all incoming requests.
///
/// Emits structured `tracing` events at INFO with the method, path,
/// and the per-request id installed by the framework's
/// `RequestIdMiddleware`.
pub struct LoggingMiddleware;

#[async_trait]
impl Middleware for LoggingMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let method = request.method().to_string();
        let path = request.path().to_string();
        let request_id = current_request_id()
            .map(|id| id.as_str().to_string())
            .unwrap_or_default();
        tracing::info!(method = %method, path = %path, request_id = %request_id, "request received");
        let response = next(request).await;
        tracing::info!(method = %method, path = %path, request_id = %request_id, "request complete");
        response
    }
}
