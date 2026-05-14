//! Dogfood endpoint for the Phase 2 pagination + cursor + Inertia
//! bridge. Builds a 100-row in-memory user list and serves it via
//! cursor pagination as a JSON response — no SeaORM entity required.

use serde::Serialize;
use suprnova::{json_response, CursorPaginator, IntoInertiaScroll, Request, Response, ResponseExt};

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

/// GET /api/users[?cursor=<opaque>][&per_page=N]
///
/// Walks a 100-user fixture forward in pages of `per_page` (default
/// 20). Returns the page rows plus next/prev cursors as Inertia
/// scroll metadata, JSON-encoded.
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

pub async fn index(req: Request) -> Response {
    let qs = req.inner().uri().query();
    let per_page: u64 = query_param(qs, "per_page")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    let cursor_param: Option<String> = query_param(qs, "cursor");
    let boundary: Option<i64> = match cursor_param.as_deref() {
        Some(c) => match CursorPaginator::<User>::decode_cursor(c) {
            Ok(plain) => plain.parse().ok(),
            Err(e) => return json_response!({ "error": e.to_string() }).status(400),
        },
        None => None,
    };

    let users = make_users();
    let mut filtered: Vec<User> = users
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

    let paginator = CursorPaginator {
        data: filtered,
        next_cursor,
        prev_cursor: None,
    };
    let (meta, data) = paginator.into_inertia_scroll();

    json_response!({
        "data": data,
        "meta": {
            "page_name": meta.page_name,
            "next": meta.next_page,
            "previous": meta.previous_page,
            "current": meta.current_page,
        },
    })
    .status(200)
}
