//! #380d (Augment) — route registration no longer forces a panic on a
//! recoverable `matchit` insert error (duplicate or malformed pattern).
//!
//! Closes the Codex MEDIUM "infallible public surfaces still convert
//! recoverable errors into panics" for the routing surface. The panicking
//! registration helpers are RETAINED as ergonomic, fail-loud-at-boot escape
//! hatches: route registration runs once at startup and a duplicate route is
//! a programmer error worth crashing on. The new fallible siblings return
//! `Err(FrameworkError)` (naming the method + path) so registration driven by
//! a source you don't control at compile time — dynamic config, plugins —
//! becomes a recoverable error instead of a process-ending panic:
//!
//! - HTTP: `Router::{get,post,put,delete}` gain `try_*` siblings (returning
//!   `Result<RouteBuilder, FrameworkError>`); `RouteBuilder::{get,post,put,
//!   delete}` mirror them for mid-chain registration.
//! - WebSocket: the whole `ws*` family gains `try_*` siblings (returning
//!   `Result<Router, FrameworkError>`), all funnelling through the canonical
//!   `try_ws_boxed_with_middleware_and_config`.
//! - Groups: `GroupBuilder::try_finalize` is the fallible counterpart of the
//!   `From<GroupBuilder> for Router` / `.into()` conversion.
//!
//! Teeth: against the pre-#380d code these `try_*` paths did not exist; the
//! only way to register a route was the panicking helper. Each infallible
//! path is also asserted to STILL panic (with the original message) so the
//! delegate-via-`expect` refactor cannot silently swallow a conflict.

use std::panic::{AssertUnwindSafe, catch_unwind};

use async_trait::async_trait;
use suprnova::http::{Request, text};
use suprnova::routing::try_register_route_name;
use suprnova::ws::{WebSocketHandler, WsSocket};
use suprnova::{FrameworkError, Response, Router};

/// Minimal HTTP handler — the body is irrelevant; we only exercise
/// registration, not dispatch.
async fn h(_req: Request) -> Response {
    text("ok")
}

/// Minimal WebSocket handler. A unit struct so two independent instances can
/// be registered on the same path to force a duplicate-registration error.
struct NoopWs;

#[async_trait]
impl WebSocketHandler for NoopWs {
    async fn handle(&self, _socket: WsSocket, _request: Request) -> Result<(), FrameworkError> {
        Ok(())
    }
}

/// Extract a panic payload as a `String` regardless of whether it was raised
/// via `panic!(format!(...))` (String payload) or a `&'static str`.
fn panic_message_of(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else {
        "<unrecognised panic payload type>".to_string()
    }
}

// ---- HTTP: try_* return Err on a duplicate ----------------------------

#[test]
fn try_get_returns_err_naming_method_and_path() {
    let router: Router = Router::new()
        .try_get("/dup", h)
        .expect("first GET registration must succeed")
        .into();
    let Err(err) = router.try_get("/dup", h) else {
        panic!("second try_get on the same path must return Err, not Ok");
    };
    let msg = err.to_string();
    assert!(
        msg.contains("Failed to register GET route '/dup'"),
        "error must name the GET method + path; got: {msg}",
    );
}

#[test]
fn try_post_returns_err_naming_method_and_path() {
    let router: Router = Router::new()
        .try_post("/dup", h)
        .expect("first POST registration must succeed")
        .into();
    let Err(err) = router.try_post("/dup", h) else {
        panic!("second try_post on the same path must return Err");
    };
    assert!(
        err.to_string()
            .contains("Failed to register POST route '/dup'"),
        "error must name the POST method + path; got: {err}",
    );
}

#[test]
fn try_put_returns_err_naming_method_and_path() {
    let router: Router = Router::new()
        .try_put("/dup", h)
        .expect("first PUT registration must succeed")
        .into();
    let Err(err) = router.try_put("/dup", h) else {
        panic!("second try_put on the same path must return Err");
    };
    assert!(
        err.to_string()
            .contains("Failed to register PUT route '/dup'"),
        "error must name the PUT method + path; got: {err}",
    );
}

#[test]
fn try_delete_returns_err_naming_method_and_path() {
    let router: Router = Router::new()
        .try_delete("/dup", h)
        .expect("first DELETE registration must succeed")
        .into();
    let Err(err) = router.try_delete("/dup", h) else {
        panic!("second try_delete on the same path must return Err");
    };
    assert!(
        err.to_string()
            .contains("Failed to register DELETE route '/dup'"),
        "error must name the DELETE method + path; got: {err}",
    );
}

// ---- HTTP: the infallible escape hatches STILL panic ------------------

#[test]
fn get_still_panics_on_duplicate() {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _ = Router::new().get("/dup", h).get("/dup", h);
    }));
    let Err(payload) = result else {
        panic!("Router::get must still panic on a duplicate route");
    };
    let msg = panic_message_of(payload);
    assert!(
        msg.contains("Failed to register GET route '/dup'"),
        "panic must preserve the original message; got: {msg}",
    );
}

#[test]
fn post_still_panics_on_duplicate() {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _ = Router::new().post("/dup", h).post("/dup", h);
    }));
    let Err(payload) = result else {
        panic!("Router::post must still panic on a duplicate route");
    };
    assert!(
        panic_message_of(payload).contains("Failed to register POST route '/dup'"),
        "panic must preserve the original POST message",
    );
}

#[test]
fn put_still_panics_on_duplicate() {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _ = Router::new().put("/dup", h).put("/dup", h);
    }));
    let Err(payload) = result else {
        panic!("Router::put must still panic on a duplicate route");
    };
    assert!(
        panic_message_of(payload).contains("Failed to register PUT route '/dup'"),
        "panic must preserve the original PUT message",
    );
}

#[test]
fn delete_still_panics_on_duplicate() {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _ = Router::new().delete("/dup", h).delete("/dup", h);
    }));
    let Err(payload) = result else {
        panic!("Router::delete must still panic on a duplicate route");
    };
    assert!(
        panic_message_of(payload).contains("Failed to register DELETE route '/dup'"),
        "panic must preserve the original DELETE message",
    );
}

// ---- HTTP: happy path + RouteBuilder mid-chain ------------------------

#[test]
fn try_get_ok_path_registers_a_matchable_route() {
    let router: Router = Router::new()
        .try_get("/users/:id", h)
        .expect("clean registration must succeed")
        .into();
    let matched = router.match_route(&hyper::Method::GET, "/users/42");
    let (pattern, _handler, params) =
        matched.expect("the route registered via try_get must be matchable");
    assert_eq!(pattern, "/users/{id}");
    assert_eq!(params.get("id"), Some(&"42".to_string()));
}

#[test]
fn route_builder_try_get_returns_err_mid_chain() {
    // First `.get` returns a RouteBuilder; the chained `try_get` on the same
    // path exercises RouteBuilder::try_get and must surface the conflict.
    let Err(err) = Router::new().get("/a", h).try_get("/a", h) else {
        panic!("RouteBuilder::try_get must return Err on a mid-chain duplicate");
    };
    assert!(
        err.to_string()
            .contains("Failed to register GET route '/a'"),
        "error must name the GET method + path; got: {err}",
    );
}

// ---- WebSocket --------------------------------------------------------

#[test]
fn try_ws_returns_err_on_duplicate() {
    let router = Router::new()
        .try_ws("/ws/x", NoopWs)
        .expect("first WS registration must succeed");
    let Err(err) = router.try_ws("/ws/x", NoopWs) else {
        panic!("second try_ws on the same path must return Err");
    };
    assert!(
        err.to_string()
            .contains("Failed to register WS route '/ws/x'"),
        "error must name the WS route; got: {err}",
    );
}

#[test]
fn ws_still_panics_on_duplicate() {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _ = Router::new().ws("/ws/x", NoopWs).ws("/ws/x", NoopWs);
    }));
    let Err(payload) = result else {
        panic!("Router::ws must still panic on a duplicate route");
    };
    assert!(
        panic_message_of(payload).contains("Failed to register WS route '/ws/x'"),
        "panic must preserve the original WS message",
    );
}

#[test]
fn try_ws_ok_path_registers_a_matchable_route() {
    let router = Router::new()
        .try_ws("/ws/rooms/:id", NoopWs)
        .expect("clean WS registration must succeed");
    let m = router
        .match_ws("/ws/rooms/7")
        .expect("the WS route registered via try_ws must be matchable");
    assert_eq!(m.params().get("id").map(String::as_str), Some("7"));
}

// ---- Groups: try_finalize vs From/into --------------------------------

#[test]
fn group_try_finalize_returns_err_on_duplicate() {
    // Two GET routes on the same in-group path collide once the prefix is
    // applied (`/api/x`), so finalising the group is recoverable via try_*.
    let builder = Router::new().group("/api", |r| r.get("/x", h).get("/x", h));
    let Err(err) = builder.try_finalize() else {
        panic!("GroupBuilder::try_finalize must return Err on a duplicate route");
    };
    assert!(
        err.to_string()
            .contains("Failed to register GET route '/api/x'"),
        "error must name the prefixed method + path; got: {err}",
    );
}

#[test]
fn group_into_still_panics_on_duplicate() {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _router: Router = Router::new()
            .group("/api", |r| r.get("/x", h).get("/x", h))
            .into();
    }));
    let Err(payload) = result else {
        panic!("From<GroupBuilder> for Router (.into()) must still panic on a duplicate");
    };
    assert!(
        panic_message_of(payload).contains("Failed to register GET route '/api/x'"),
        "panic must preserve the original group message",
    );
}

#[test]
fn group_try_finalize_ok_path_merges_routes() {
    let router = Router::new()
        .group("/api", |r| r.get("/users", h).post("/users", h))
        .try_finalize()
        .expect("a conflict-free group must finalise cleanly");
    assert!(
        router
            .match_route(&hyper::Method::GET, "/api/users")
            .is_some(),
        "GET /api/users must be registered",
    );
    assert!(
        router
            .match_route(&hyper::Method::POST, "/api/users")
            .is_some(),
        "POST /api/users must be registered",
    );
}

// ---- Route NAME registration (#380e) ----------------------------------
//
// The route-name registry is a process-global, so every test here uses a
// uniquely-prefixed name to avoid cross-test pollution under parallel runs.

#[test]
fn route_builder_try_name_returns_err_on_duplicate_name_different_path() {
    // First binding of the name to /a succeeds...
    let _router: Router = Router::new()
        .get("/a", h)
        .try_name("try380e.dup")
        .expect("first try_name binding must succeed");
    // ...rebinding the same name to a DIFFERENT path is the recoverable error.
    let Err(err) = Router::new().get("/b", h).try_name("try380e.dup") else {
        panic!("try_name must return Err when the name is already bound elsewhere");
    };
    assert!(
        err.to_string().contains("Route name 'try380e.dup'"),
        "error must name the conflicting route name; got: {err}",
    );
}

#[test]
fn route_builder_name_still_panics_on_duplicate_name() {
    let _router = Router::new().get("/a", h).name("try380e.panic");
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _ = Router::new().get("/b", h).name("try380e.panic");
    }));
    let Err(payload) = result else {
        panic!("RouteBuilder::name must still panic on a duplicate name");
    };
    assert!(
        panic_message_of(payload).contains("Route name 'try380e.panic'"),
        "panic must preserve the original route-name message",
    );
}

#[test]
fn try_name_same_name_same_path_is_idempotent() {
    let _first: Router = Router::new()
        .get("/same", h)
        .try_name("try380e.idem")
        .expect("first binding ok");
    // Re-binding the SAME (name, path) pair is a no-op, not an error.
    let second = Router::new().get("/same", h).try_name("try380e.idem");
    assert!(
        second.is_ok(),
        "re-registering the same (name, path) must stay Ok (idempotent)",
    );
}

#[test]
fn try_register_route_name_primitive_returns_err_on_conflict() {
    try_register_route_name("try380e.direct", "/x").expect("first binding ok");
    let Err(err) = try_register_route_name("try380e.direct", "/y") else {
        panic!("try_register_route_name must return Err on a name->different-path conflict");
    };
    assert!(
        err.to_string().contains("Route name 'try380e.direct'"),
        "error must name the conflicting route name; got: {err}",
    );
}
