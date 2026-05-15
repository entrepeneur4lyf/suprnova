//! Dogfood endpoint for the Phase 2 pagination + cursor + Inertia
//! bridge. Builds a 100-row in-memory user list and serves it via
//! the full Inertia path — `Inertia::paginate("Users/Index", "users",
//! paginator)` — so the framework's `IntoInertiaScroll` wiring is
//! exercised end-to-end.
//!
//! The cursor uses the typed `sea_orm::Value::BigInt` wire format
//! (not the legacy `Value::String` path), so the dogfood matches what
//! a real entity-backed `Pagination::cursor` would emit on the wire.
//!
//! Query params:
//! - `per_page` (default `20`)
//! - `cursor`   (opaque keyset cursor; first page omits it)
//! - `format=json`  → return the paginator as raw JSON instead of an
//!   Inertia response. Useful for hitting the route from `curl` /
//!   tests that don't speak Inertia.

use suprnova::{
    json_response, CursorDirection, CursorPaginator, FrameworkError, HttpResponse, Inertia,
    IntoInertiaScroll, Request, Response, ResponseExt,
};

use crate::props::UserProps;

fn make_users() -> Vec<UserProps> {
    (1..=100i64)
        .map(|id| UserProps {
            id,
            email: format!("user-{:03}@example.com", id),
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

/// Decode the inbound cursor — typed `Value::BigInt` wire format —
/// into a tuple of `(boundary id, direction)`. Returns `None` when
/// the caller didn't pass a cursor (first page).
fn decode_id_cursor(
    raw: Option<&str>,
) -> Result<Option<(i64, CursorDirection)>, FrameworkError> {
    match raw {
        None => Ok(None),
        Some(c) => {
            let (value, direction) = CursorPaginator::<UserProps>::decode_value(c)?;
            let id = match value {
                sea_orm::Value::BigInt(Some(i)) => i,
                other => {
                    return Err(FrameworkError::internal(format!(
                        "Expected BigInt cursor for /api/users, got {other:?}"
                    )));
                }
            };
            Ok(Some((id, direction)))
        }
    }
}

/// Build the paginator for the current request. Pure — no DB, no
/// shared state, just slicing a 100-user fixture by the cursor.
fn build_page(qs: Option<&str>) -> Result<CursorPaginator<UserProps>, FrameworkError> {
    let per_page: u64 = query_param(qs, "per_page")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    let cursor_input = query_param(qs, "cursor");
    let decoded = decode_id_cursor(cursor_input.as_deref())?;

    let all_users = make_users();
    let (mut filtered, scan_dir): (Vec<UserProps>, CursorDirection) = match decoded {
        None => (all_users, CursorDirection::Next),
        Some((boundary, CursorDirection::Next)) => (
            all_users.into_iter().filter(|u| u.id > boundary).collect(),
            CursorDirection::Next,
        ),
        Some((boundary, CursorDirection::Prev)) => {
            // Back-scan: rows < boundary, DESC; then reverse to ASC.
            let mut rows: Vec<UserProps> = all_users
                .into_iter()
                .filter(|u| u.id < boundary)
                .collect();
            rows.reverse();
            (rows, CursorDirection::Prev)
        }
    };

    let overflow = filtered.len() as u64 > per_page;
    if overflow {
        match scan_dir {
            CursorDirection::Next => filtered.truncate(per_page as usize),
            CursorDirection::Prev => {
                // Back-scan: we DESC-fetched then reversed → rows
                // are already ASC. The overflow row (if any) is now
                // at index 0; drop it so the kept slice is still ASC.
                let drop = filtered.len() - per_page as usize;
                filtered.drain(0..drop);
            }
        }
    }

    let entered_via_next = matches!(decoded, Some((_, CursorDirection::Next)));
    let entered_via_prev = matches!(decoded, Some((_, CursorDirection::Prev)));

    let next_cursor = {
        let has_next = match scan_dir {
            CursorDirection::Next => overflow,
            CursorDirection::Prev => true,
        };
        if has_next && !filtered.is_empty() {
            let last = filtered.last().unwrap();
            Some(CursorPaginator::<UserProps>::encode_value(
                &sea_orm::Value::BigInt(Some(last.id)),
                CursorDirection::Next,
            )?)
        } else {
            None
        }
    };

    let prev_cursor = {
        let has_prev = match scan_dir {
            CursorDirection::Next => entered_via_next || entered_via_prev,
            CursorDirection::Prev => overflow,
        };
        if has_prev && !filtered.is_empty() {
            let first = filtered.first().unwrap();
            Some(CursorPaginator::<UserProps>::encode_value(
                &sea_orm::Value::BigInt(Some(first.id)),
                CursorDirection::Prev,
            )?)
        } else {
            None
        }
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
