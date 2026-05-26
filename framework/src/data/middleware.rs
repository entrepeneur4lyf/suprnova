//! Per-request middleware that parses `?include=`/`?exclude=`/`?only=`/
//! `?except=` from the query string and binds the resulting
//! [`RequestIncludeSet`] to the `REQUEST_INCLUDE_SET` task-local so the
//! lazy-prop resolver and handlers can consult it.

use std::sync::Arc;

use async_trait::async_trait;

use crate::Request;
use crate::http::Response;
use crate::middleware::{Middleware, Next};

use super::include_set::{REQUEST_INCLUDE_SET, RequestIncludeSet};

/// Per-request middleware that parses `?include=`/`?exclude=`/`?only=`/
/// `?except=` from the request URI and binds the resulting
/// [`RequestIncludeSet`] into the [`REQUEST_INCLUDE_SET`] task-local for
/// the duration of the request.
///
/// # When to install
///
/// Install this middleware **globally** (or at the root of any router
/// that serves Data-derived endpoints). It should sit BEFORE any
/// middleware that resolves a `Prop::Lazy`-owned field, because the
/// lazy resolver consults the task-local. A safe position in the
/// standard stack is between session middleware and authorization
/// middleware:
///
/// ```text
/// SessionMiddleware → IncludeMiddleware → AuthMiddleware → handlers
/// ```
///
/// # What it enables
///
/// Handlers and the `#[derive(Data)]`-generated `Inertia::data` path
/// can call [`super::include_set::current_include_set`] to inspect the request's
/// `?include=`/`?exclude=`/`?only=`/`?except=` parameters. The
/// per-DTO allowlist (Task 8) enforces default-deny: only fields
/// listed in `#[data(allow_include)]` are eligible.
///
/// # Performance
///
/// Cost per request: one [`RequestIncludeSet::from_query`] parse, one
/// [`crate::resources::RequestFieldsetSet::from_query`] parse, and one
/// `Arc::new` allocation. Both parsers run `url::form_urlencoded::parse`
/// over the raw query string so percent-encoded commas, brackets, and
/// reserved characters decode correctly before the value-split step.
pub struct IncludeMiddleware;

#[async_trait]
impl Middleware for IncludeMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let raw_query = request.query().unwrap_or("").to_string();
        let set = Arc::new(RequestIncludeSet::from_query(&raw_query));
        let fieldset = crate::resources::RequestFieldsetSet::from_query(&raw_query);
        REQUEST_INCLUDE_SET
            .scope(set, async move {
                crate::resources::REQUEST_FIELDSET
                    .scope(fieldset, next(request))
                    .await
            })
            .await
    }
}
