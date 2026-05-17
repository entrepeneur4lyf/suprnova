//! Ping controller — used to exercise the rate-limit middleware dogfood.

use suprnova::{Request, Response};

pub async fn pong(_req: Request) -> Response {
    Ok(suprnova::HttpResponse::text("pong"))
}
