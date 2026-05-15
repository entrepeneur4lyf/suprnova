//! Per-request middleware that parses `?include=`/`?exclude=`/`?only=`/
//! `?except=` from the query string and binds the resulting
//! [`RequestIncludeSet`] to the `REQUEST_INCLUDE_SET` task-local so the
//! lazy-prop resolver and handlers can consult it.

use std::sync::Arc;

use async_trait::async_trait;

use crate::http::Response;
use crate::middleware::{Middleware, Next};
use crate::Request;

use super::include_set::{RequestIncludeSet, REQUEST_INCLUDE_SET};

pub struct IncludeMiddleware;

#[async_trait]
impl Middleware for IncludeMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let raw_query = request.query().unwrap_or("").to_string();
        let set = Arc::new(RequestIncludeSet::from_query(&raw_query));
        REQUEST_INCLUDE_SET.scope(set, next(request)).await
    }
}
