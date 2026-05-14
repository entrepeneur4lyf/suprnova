//! Dogfood endpoint for the Phase 2 pagination + cursor + Inertia
//! bridge. Builds a 100-row in-memory user list and serves it via
//! the full Inertia path — `Inertia::paginate("Users/Index", "users",
//! paginator)` — so the framework's `IntoInertiaScroll` wiring is
//! exercised end-to-end.
//!
//! Query params:
//! - `per_page` (default `20`)
//! - `cursor`   (opaque keyset cursor; first page omits it)
//! - `format=json`  → return the paginator as raw JSON instead of an
//!   Inertia response. Useful for hitting the route from `curl` /
//!   tests that don't speak Inertia.

use serde::Serialize;
use suprnova::{
    json_response, CursorPaginator, FrameworkError, HttpResponse, Inertia, IntoInertiaScroll,
    Request, Response, ResponseExt,
};

#[derive(Clone, Serialize)]
struct User {
    id: i64,
    name: String,
}

fn make_users() -> Vec<User> {
    (1..=100i64)
        .map(|id| User {
            id,
            name: format!("user-{:03}", id),
        })
        .collect()
}

fn query_param(qs: Option<&str>, key: &str) -> Option<String> {
    qs.and_then(|s| {
        s.split('&').find_map(|kv| {
            let mut it = kv.splitn(2, '=');
            let k = it.next()?;
            let v = it.next().unwrap_or("");
            if k == key { Some(v.to_string()) } else { None }
        })
    })
}

/// Build the paginator for the current request. Pure — no DB, no
/// shared state, just slicing a 100-user fixture by the cursor.
fn build_page(qs: Option<&str>) -> Result<CursorPaginator<User>, FrameworkError> {
    let per_page: u64 = query_param(qs, "per_page")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    let cursor_param: Option<String> = query_param(qs, "cursor");
    let boundary: Option<i64> = match cursor_param.as_deref() {
        Some(c) => Some(
            CursorPaginator::<User>::decode_cursor(c)?
                .parse::<i64>()
                .map_err(|e| FrameworkError::internal(format!("bad cursor int: {e}")))?,
        ),
        None => None,
    };

    let mut filtered: Vec<User> = make_users()
        .into_iter()
        .filter(|u| boundary.map(|b| u.id > b).unwrap_or(true))
        .collect();

    let has_more = filtered.len() as u64 > per_page;
    if has_more {
        filtered.truncate(per_page as usize);
    }
    let next_cursor = if has_more {
        filtered
            .last()
            .map(|u| CursorPaginator::<User>::encode_cursor(&u.id.to_string()))
    } else {
        None
    };
    // Provide a prev_cursor only when we entered via a forward cursor —
    // i.e. there *is* a previous page. The dogfood path mirrors the
    // bidirectional semantics of `Pagination::cursor` against a real
    // entity, so consumers get to see the same shape on the wire.
    let prev_cursor = if boundary.is_some() {
        filtered
            .first()
            .map(|u| CursorPaginator::<User>::encode_cursor(&u.id.to_string()))
    } else {
        None
    };

    Ok(CursorPaginator {
        data: filtered,
        next_cursor,
        prev_cursor,
    })
}

/// `GET /api/users[?cursor=<opaque>][&per_page=N][&format=json]`
///
/// Returns an Inertia response by default (the `Users/Index` page
/// component, with `props.users` set to the cursor-paginated rows and
/// scroll metadata wired through). Pass `?format=json` to receive the
/// raw paginator as JSON.
pub async fn index(req: Request) -> Response {
    let qs = req.inner().uri().query().map(|s| s.to_string());
    let want_json = query_param(qs.as_deref(), "format").as_deref() == Some("json");

    let paginator = match build_page(qs.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            return json_response!({ "error": e.to_string() }).status(400);
        }
    };

    if want_json {
        // Raw JSON view of the paginator — exercises the same
        // `IntoInertiaScroll` bridge so the wire shape stays in sync
        // with the Inertia path.
        let (meta, data) = paginator.into_inertia_scroll();
        return json_response!({
            "data": data,
            "meta": {
                "page_name": meta.page_name,
                "next": meta.next_page,
                "previous": meta.previous_page,
                "current": meta.current_page,
            },
        })
        .status(200);
    }

    Inertia::paginate("Users/Index", "users", paginator)
        .resolve(&req)
        .await
        .map_err(HttpResponse::from)
}
