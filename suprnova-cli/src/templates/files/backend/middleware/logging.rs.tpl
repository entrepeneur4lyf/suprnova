//! Logging middleware - logs all requests

use suprnova::{async_trait, Middleware, Next, Request, Response};

/// Middleware that logs all incoming requests
pub struct LoggingMiddleware;

#[async_trait]
impl Middleware for LoggingMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        println!("--> {} {}", request.method(), request.path());
        let response = next(request).await;
        println!("<-- Request complete");
        response
    }
}
