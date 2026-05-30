# HTTP Tests

This chapter shows how to test your HTTP surface — routes, middleware,
auth flows, error responses — by driving the framework's request
pipeline through `suprnova::handle_request`. If you've written Laravel
feature tests with `$this->get('/users')` and asserted on
`$response->status()`, this is the Suprnova equivalent: the same
`Router` you mount in production runs in the test, every middleware
fires, the panic boundary still catches, and the response is
byte-for-byte what a real client sees.

## The test surface

There are exactly three building blocks:

| Piece | Role |
|---|---|
| `Router` | The routes under test — built the same way as in production |
| `MiddlewareRegistry` | The global middleware stack — also built the same way |
| `handle_request(router, registry, req) -> hyper::Response<…>` | The in-process driver — runs one request end-to-end |

`handle_request` is the same function `Server::run` calls per
request, exposed for tests and embedders. Anything that works in
production works here — the panic-recovery wrapper, the request-id
scope, the Inertia flash-bag scope, the auth request state scope, the
HEAD-body strip, post-response termination. There is no "test mode"
that swaps a quieter pipeline in.

`handle_request_with_peer` is the same call with an explicit
`Option<std::net::IpAddr>` for the connecting peer — useful when you
want to assert on `Request::ip()` resolution without setting up proxy
headers.

## The hyper body problem

The one wrinkle worth knowing about up front: `handle_request` takes a
`hyper::Request<hyper::body::Incoming>`. `Incoming` is hyper's
internal streaming body type; you cannot construct one with
`Full::new(bytes)` or any of the in-memory body types. It only comes
out of a hyper connection.

There are two clean ways around it:

1. **TCP loopback** — bind a `127.0.0.1:0` listener, serve one
   accept inside a `service_fn`, send the request through a hyper
   client, and let `Incoming` be produced naturally on the server
   side. This is what every integration test in the framework
   already does.
2. **In-process Request building** — for tests that only need to
   inspect `Request` accessors (headers, route params, IP, JSON
   parsing) without going through routing, use the same TCP-loopback
   capture pattern but with a service that pulls the `Request` out
   into a `oneshot::channel` instead of running it. The
   `framework/tests/http_request_accessors.rs` file has this
   `build_request()` helper verbatim.

Both patterns produce real `Incoming` bodies. The loopback is local,
synchronous in test wall-clock terms (microseconds), and never touches
the network outside `lo`. There is no slower or simpler way that
preserves the contract.

### Why Suprnova diverges

Laravel's `$this->get('/users')` works because PHP's request lifecycle
is "build a `Request` object, dispatch it through the kernel". The
kernel takes the in-memory object directly; there is no body type that
forces a transport. Suprnova's server is built on hyper, and hyper's
body type is opinionated for good reasons (streaming, backpressure,
zero-copy). The test surface inherits that constraint.

What you trade for the constraint is fidelity. Every detail of the
production request path — header parsing, body limits, connection
upgrades — runs the same way in tests. You will never have a test
pass because the test harness skipped a layer the real server runs.

## A first end-to-end test

Here is a complete, working test that mounts a single route, sends a
GET against it, and asserts on the status and body.

```rust
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use suprnova::http::text;
use suprnova::{MiddlewareRegistry, Request, Router, handle_request};

async fn spawn_server(router: Router, accepts: usize) -> SocketAddr {
    let router = Arc::new(router);
    let middleware = Arc::new(MiddlewareRegistry::new());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral listener");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        for _ in 0..accepts {
            let Ok((stream, _)) = listener.accept().await else { return };
            let io = TokioIo::new(stream);
            let router = router.clone();
            let middleware = middleware.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req: hyper::Request<Incoming>| {
                    let router = router.clone();
                    let middleware = middleware.clone();
                    async move {
                        Ok::<_, Infallible>(handle_request(router, middleware, req).await)
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });

    addr
}

async fn send_get(addr: SocketAddr, path: &str) -> (u16, Bytes) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io).await.unwrap();
    tokio::spawn(async move { let _ = conn.await; });

    let req = hyper::Request::builder()
        .method("GET")
        .uri(path)
        .header("Host", "localhost")
        .header("Content-Length", "0")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_get timeout")
        .expect("hyper send_request");
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    (parts.status.as_u16(), bytes)
}

#[tokio::test]
async fn get_root_returns_hello() {
    let router = Router::new().get("/", |_req: Request| async { text("hello") });
    let addr = spawn_server(router, 1).await;

    let (status, body) = send_get(addr, "/").await;
    assert_eq!(status, 200);
    assert_eq!(&body[..], b"hello");
}
```

That's the entire shape. Copy the two helpers per crate, tune them
for the suite (multiple accepts, header capture, body capture). The
framework itself uses near-identical helpers in
`framework/tests/cors_middleware.rs`,
`framework/tests/middleware_panic_safety.rs`, and
`framework/tests/email_verified_middleware.rs`.

The `accepts` argument bounds how many connections the accept loop
serves before exiting. One is enough for a single request; bump to
two-or-more when a test exercises post-panic recovery (see
[Testing the panic boundary](#testing-the-panic-boundary)).

## Building a request

Inside `send_get` you saw:

```rust
let req = hyper::Request::builder()
    .method("GET")
    .uri("/users/42")
    .header("Host", "localhost")
    .header("Content-Length", "0")
    .body(Full::new(Bytes::new()))
    .unwrap();
```

That's the canonical shape. A few things worth knowing:

- **`Host` header**. Hyper rejects HTTP/1.1 requests without one. Always
  include it; the value doesn't matter unless your handler keys on it.
- **`Content-Length: 0`**. Match the body. Hyper computes this for you
  with `Full::new(Bytes::new())`, but being explicit reads cleaner in
  tests.
- **Body types**. The client side sends `Full<Bytes>`. The server side
  receives `Incoming`. You only ever build `Full<Bytes>` requests in
  tests; the framework receives them as `Incoming` after hyper's
  per-connection conversion.

A POST with a JSON body:

```rust
let body_bytes = serde_json::to_vec(&serde_json::json!({
    "name": "Alice",
    "email": "alice@example.com"
})).unwrap();

let req = hyper::Request::builder()
    .method("POST")
    .uri("/users")
    .header("Host", "localhost")
    .header("content-type", "application/json")
    .header("content-length", body_bytes.len())
    .body(Full::new(Bytes::from(body_bytes)))
    .unwrap();
```

## Asserting on the response

The response that comes back from `handle_request` is a
`hyper::Response<BoxBody<Bytes, Infallible>>`. Three things you'll
read off it:

```rust
let (parts, body) = resp.into_parts();

// 1. Status.
assert_eq!(parts.status.as_u16(), 200);

// 2. Headers — case-insensitive lookup.
let location = parts.headers.get("location").and_then(|v| v.to_str().ok());
assert_eq!(location, Some("/login"));

// 3. Body — collect into bytes, then parse.
use http_body_util::BodyExt;
let bytes = body.collect().await.unwrap().to_bytes();

// As text:
let text = String::from_utf8_lossy(&bytes);

// As JSON:
let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
assert_eq!(value["message"], "ok");
```

For error responses, the body shape is fixed and documented in
[Error Model](error-model.md) — `message`, `errors`, `request_id`,
and an optional `debug_message`. The `request_id` key is always
present (may be `null` outside a request scope), which is what to
assert on when checking that the request-id middleware ran.

## Testing middleware

Middleware tests look identical to route tests; the only difference
is what you `.append()` to the registry before spawning.

### Testing global middleware

Pass the middleware to `MiddlewareRegistry::new().append(...)` and
use that registry — multiple middlewares run in append order,
`prepend` puts a new one at the front.

```rust
use suprnova::{CorsConfig, CorsMiddleware, MiddlewareRegistry};

fn cors_registry() -> MiddlewareRegistry {
    MiddlewareRegistry::new().append(CorsMiddleware::new(
        CorsConfig::allow_origins(["https://app.example"])
            .allow_credentials(true)
            .max_age(std::time::Duration::from_secs(600)),
    ))
}

#[tokio::test]
async fn cors_preflight_returns_204_with_headers() {
    let router = Router::new();
    // The 3-arg form of `spawn_server` lets you wire a non-empty
// MiddlewareRegistry — copy the helper from
// framework/tests/cors_middleware.rs (it's ~30 lines).
let addr = spawn_server(router, cors_registry(), 1).await;

    let (status, headers, _) = options(
        addr,
        "/anything",
        &[
            ("Origin", "https://app.example"),
            ("Access-Control-Request-Method", "POST"),
        ],
    ).await;

    assert_eq!(status, 204);
    assert_eq!(
        headers.get("access-control-allow-origin").map(String::as_str),
        Some("https://app.example"),
    );
}
```

This test proves more than the CORS logic itself: it proves that
global middleware runs on **unrouted** requests too, which is the
contract the framework guarantees (otherwise an OPTIONS preflight that
never matches a route would skip CORS). See `framework/tests/cors_middleware.rs`
for the full suite.

### Testing route-specific middleware

Attach with `.middleware(...)` on the route builder, exactly like
production. Then test the route as normal — the middleware chain is
built off the same registration.

```rust
let router = Router::new()
    .get("/admin/dashboard", |_req| async { text("admin") })
    .middleware(RequireRole::new("admin"));

let (status, _) = send_get(addr, "/admin/dashboard").await;
assert_eq!(status, 403); // unauthenticated request
```

### Stubbing the authenticated user

Real auth-flow tests need a logged-in user. The cleanest pattern is a
tiny one-off middleware that calls `Auth::set_user` ahead of the
middleware under test. The framework's own
`framework/tests/email_verified_middleware.rs` uses this:

```rust
use std::any::Any;
use std::sync::Arc;
use suprnova::{Auth, Authenticatable, Middleware, Next, Request, Response};

struct UserById(String);

impl Authenticatable for UserById {
    fn get_auth_identifier(&self) -> String { self.0.clone() }
    fn as_any(&self) -> &dyn Any { self }
}

struct LoginAs(String);

#[async_trait::async_trait]
impl Middleware for LoginAs {
    async fn handle(&self, request: Request, next: Next) -> Response {
        Auth::set_user(Arc::new(UserById(self.0.clone())));
        next(request).await
    }
}
```

Then in the test:

```rust
let registry = MiddlewareRegistry::new()
    .append(LoginAs("user-id-123".to_string()))
    .append(EnsureEmailVerifiedMiddleware::new());
```

`LoginAs` runs first, installs the user into the per-request auth
state, and the middleware under test sees `Auth::id() == Some(...)`
without ever issuing a real login. The auth state scope is set up by
`handle_request` itself — the same one that runs in production — so
the user is visible to every later middleware and the handler.

## Testing route model binding

Route model binding turns `/users/{id}` into a typed `User` argument.
The binding runs as part of the handler's extractor chain, so a normal
end-to-end test exercises it for free:

```rust
#[suprnova::model(table = "users")]
pub struct User {
    pub id: i64,
    pub email: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[tokio::test]
async fn show_user_binds_from_route_param() {
    // Insert a test user via the model. Database setup omitted —
    // see the testing chapter for `TestDatabase` patterns.
    let user = User::create(suprnova::attrs! {
        email: "bound@example.com"
    }).await.unwrap();

    let router = Router::new().get("/users/{id}", |req: Request| async move {
        let id: i64 = req.param("id")?.parse()
            .map_err(|_| suprnova::FrameworkError::param_parse("id", "i64"))?;
        let user = User::find_or_fail(id).await?;
        suprnova::http::json(serde_json::json!({ "email": user.email }))
    });

    let addr = spawn_server(router, 1).await;
    let (status, body) = send_get(addr, &format!("/users/{}", user.id)).await;

    assert_eq!(status, 200);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["email"], "bound@example.com");
}
```

For binding-in-isolation tests — no router, no TCP loop — synthesise
the route params yourself with `Request::with_params(...)` (see
[Builder hooks on `Request`](#builder-hooks-on-request) below). That
is the pattern `framework/tests/data_route_params.rs` uses for
testing `#[derive(Data)]` extractors against synthesised params.

## Testing auth flows end-to-end

A real auth flow test registers a user, drives the login route, pulls
the session cookie off the response, and re-sends it on a protected
route. Four steps, all wire-level:

```rust
#[tokio::test]
async fn login_flow_issues_session_cookie() {
    // 1. Bootstrap: create the user.
    Auth::password()
        .register("alice@example.com", "longpassword123")
        .await.expect("register");

    // 2. Mount the routes.
    let router = Router::new()
        .post("/login", login_handler)
        .get("/dashboard", |_req: Request| async { text("dashboard") });
    let addr = spawn_server(router, 2).await;

    // 3. Drive login; capture the Set-Cookie header.
    let login = post_json(addr, "/login", serde_json::json!({
        "email": "alice@example.com",
        "password": "longpassword123",
    })).await;
    assert_eq!(login.status, 200);
    let cookie = extract_session_cookie(&login.headers);

    // 4. Replay the cookie against the protected route.
    let (status, body) = get_with_cookie(addr, "/dashboard", &cookie).await;
    assert_eq!(status, 200);
    assert_eq!(&body[..], b"dashboard");
}
```

`extract_session_cookie` and `get_with_cookie` are straightforward
header-and-cookie plumbing — `framework/tests/auth_http_middleware.rs`
has a full implementation. The point: the entire flow runs through the
real `SessionMiddleware`, the real `Auth` guard, the real
`Authenticatable` resolution. The test verifies the wire contract,
not a mock of it.

## Testing the panic boundary

A panic inside a handler must not crash the server. The
panic-recovery wrapper (`execute_chain_safely`) catches it and
converts to a 500 through the same path returned errors flow through.
You can verify this without any special test infrastructure — set
`accepts >= 2` so the listener survives the panic:

```rust
#[tokio::test]
async fn panicking_handler_yields_500_and_server_survives() {
    let router = Router::new()
        .get("/panic", |_req: Request| async {
            panic!("intentional test panic");
            #[allow(unreachable_code)] text("unreachable")
        })
        .get("/ok", |_req: Request| async { text("ok") });

    let addr = spawn_server(router, 4).await;

    // First: the panic translates to a sanitised 500.
    let (s1, body) = send_get(addr, "/panic").await;
    assert_eq!(s1, 500);
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["message"], "Internal Server Error");
    assert!(parsed.get("request_id").is_some());

    // Second: the listener survives. The next request is normal.
    let (s2, body2) = send_get(addr, "/ok").await;
    assert_eq!(s2, 200);
    assert_eq!(&body2[..], b"ok");
}
```

## Testing accessors without going through routing

Sometimes you want to test a `Request` accessor (`bearer_token`,
`is_method`, `ip`, `is_json`, etc.) without spinning up a router at
all. The trick is a tiny harness that runs a hyper service whose only
job is to construct the `Request` and ship it back through a
`tokio::sync::oneshot::channel`:

```rust
let (req_tx, req_rx) = tokio::sync::oneshot::channel::<suprnova::Request>();
// ... loopback hyper service whose service_fn does:
//     let req = suprnova::Request::new(hyper_req);
//     let _  = req_tx.send(req);
//     return a 200 with an empty body
let req = req_rx.await.unwrap();
```

`framework/tests/http_request_accessors.rs` has the full
`build_request(builder, body) -> Request` helper. Copy it once per
crate and every accessor test reads cleanly:

```rust
#[tokio::test]
async fn bearer_token_extracts_simple_token() {
    let req = build_request(
        hyper::Request::builder()
            .method("GET")
            .uri("/api/users")
            .header("Authorization", "Bearer secret-token-123"),
        "",
    ).await;
    assert_eq!(req.bearer_token().as_deref(), Some("secret-token-123"));
}
```

The Request is real (produced by hyper from a real wire exchange), but
no routing or middleware ran — exactly what you want when the unit
under test is the accessor itself.

## Builder hooks on `Request`

When you have a `Request` in hand and need to fake one piece of the
routing layer, three builder methods help:

```rust
impl Request {
    pub fn with_params(mut self, params: HashMap<String, String>) -> Self;
    pub fn with_route_pattern(mut self, pattern: String) -> Self;
    pub fn with_peer_addr(mut self, addr: std::net::IpAddr) -> Self;
}
```

These are the same methods the server calls when it dispatches a
matched route — `Router` calls `with_params` after `matchit`
returns, `with_route_pattern` so `req.route_pattern()` resolves, and
`with_peer_addr` once it knows the accepted-TCP socket's IP. In
tests you call them yourself to short-circuit the same setup.

```rust
let req = Request::new(hyper_req)
    .with_params(HashMap::from([("id".into(), "42".into())]))
    .with_route_pattern("/users/{id}".into())
    .with_peer_addr("192.168.1.10".parse().unwrap());

assert_eq!(req.param("id").unwrap(), "42");
assert_eq!(req.ip(), Some("192.168.1.10".parse().unwrap()));
```

## Things to know

A short list of footguns that catch first-time authors:

- **`Incoming` is server-side only.** You cannot build one in your test.
  The TCP loopback (or in-process service capture) is the only path —
  there is no "build a `Request` from a `Vec<u8>` body" constructor.
- **Don't share state between tests.** Each `#[tokio::test]` gets its
  own runtime; cross-test pollution usually means you're sharing a
  global (`once_cell`, `lazy_static`, env var). For DB state see
  `TestDatabase` in [Testing](testing.md).
- **Cookies need a real client.** No automatic cookie jar — thread
  `Set-Cookie` from one response into `Cookie` on the next. See
  `framework/tests/auth_http_middleware.rs` for the pattern.
- **The post-response termination spawn is non-blocking.** If you
  want to assert on side effects that run via `Terminable`, poll
  for them — the response returns to the client before the hook runs.

## Where each piece lives

| Piece | File |
|---|---|
| `handle_request`, `handle_request_with_peer` | `framework/src/server.rs` |
| `Request::new`, `with_params`, `with_route_pattern`, `with_peer_addr` | `framework/src/http/request.rs` |
| `MiddlewareRegistry::new`, `append`, `prepend` | `framework/src/middleware/registry.rs` |
| Loopback test harness (canonical) | `framework/tests/cors_middleware.rs` |
| In-process `Request` capture harness | `framework/tests/http_request_accessors.rs` |
| Panic-boundary test pattern | `framework/tests/middleware_panic_safety.rs` |
| Auth + middleware end-to-end pattern | `framework/tests/email_verified_middleware.rs` |

## Next

- [Testing](testing.md) — `#[suprnova_test]`, `TestDatabase`, the
  `describe!`/`test!`/`expect!` macros, and the unit-level surface
- [Error Model](error-model.md) — the JSON shape every error response
  uses, the 5xx sanitisation rule, and what `request_id` means in a
  test body
- [Middleware](middleware.md) — writing the middleware you test here,
  and the global-vs-route lifecycle
- [Routing](routing.md) — the `Router` you mount in both production
  and tests, route params, route names, signed URLs
- [Authentication](authentication.md) — the `Auth` facade,
  `Authenticatable`, guards, and how `Auth::set_user` interacts with
  the request scope `handle_request` installs
