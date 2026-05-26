//! Authentication middleware - checks for Authorization header

use suprnova::{HttpResponse, Middleware, Next, Request, Response, async_trait};

/// Middleware that requires an Authorization header
pub struct AuthMiddleware;

#[async_trait]
impl Middleware for AuthMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        if request.header("Authorization").is_none() {
            println!("Unauthorized request blocked");
            return Err(HttpResponse::text("Unauthorized").status(401));
        }
        next(request).await
    }
}
