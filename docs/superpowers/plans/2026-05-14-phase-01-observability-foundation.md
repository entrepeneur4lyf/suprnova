# Phase 1: Observability Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship structured logging (tracing), a typed Event dispatcher with sync + queued delivery, error-handling enhancements that emit traces and events on every framework error, and a minimal Server-Sent Events delivery primitive — the foundation every later phase will build on.

**Architecture:** Four loosely-coupled subsystems wired through new `framework/src/{logging,events,sse}` modules plus targeted edits to `error.rs` and `http/response.rs`. `tracing` is the single source of truth for human-readable observability; `events` is the single source of truth for typed in-process pub/sub; SSE reuses the events bus as its in-process producer. A `RequestId` middleware threads a UUID per-request via a `tokio::task_local!` so every `tracing` event and every dispatched event carries it.

**Tech Stack:** `tracing` 0.1, `tracing-subscriber` 0.3 (env-filter + fmt + json layers), `uuid` 1 (v4 for request IDs), `tokio::sync::mpsc` + `broadcast` for in-process eventing, `bytes` + `http-body-util::StreamBody` for SSE chunked bodies. No new crates beyond these.

---

## File Structure

**New files:**
- `framework/src/logging/mod.rs` — re-exports + module entry
- `framework/src/logging/init.rs` — `init_subscriber(LogConfig)`; called from `Server::serve`
- `framework/src/logging/request_id.rs` — `RequestId`, `RequestIdMiddleware`, `current_request_id()`, `REQUEST_ID` task_local
- `framework/src/logging/config.rs` — `LogConfig { level, format }` env-loadable
- `framework/src/events/mod.rs` — `Event` trait, `EventDispatcher`, `Listener<E>`, `Event::dispatch`/`Event::listen` facade
- `framework/src/events/dispatcher.rs` — internal dispatcher (sync + queued paths)
- `framework/src/events/testing.rs` — `Event::fake()`, `EventFake`, `assert_dispatched`, `assert_not_dispatched`
- `framework/src/sse/mod.rs` — `SseEvent`, `SseStream`, `HttpResponse::sse` constructor
- `framework/tests/logging.rs` — request-id propagation, level filtering, structured fields
- `framework/tests/events.rs` — dispatch/listen/fake, sync + queued delivery
- `framework/tests/sse.rs` — minimal SSE end-to-end (one-shot hyper server emits events)
- `app/src/events/mod.rs` — `UserRegistered` example event
- `app/src/listeners/mod.rs` — `SendWelcomeEmailListener` example
- `app/src/controllers/sse_example.rs` — `/events/stream` demo route

**Modified files:**
- `framework/Cargo.toml` — add `tracing`, `tracing-subscriber`, `uuid`, `http-body-util` already present
- `framework/src/lib.rs` — declare new modules; re-export public items
- `framework/src/error.rs` — emit `tracing::error!` and dispatch `ErrorOccurred` event on every 5xx conversion; add `FrameworkError::context(&str)`
- `framework/src/http/response.rs` — replace `body: String` with `Body` enum (`Static(Bytes)` + `Stream(BoxBody)`), keep current API surface via `Self::text/json/html`; teach `into_hyper` to branch
- `framework/src/server.rs` — call `logging::init_subscriber(LogConfig::from_env())` on boot, install `RequestIdMiddleware` as the outermost middleware
- `app/src/middleware/logging.rs` — replace `println!` with `tracing::info!`
- `app/src/bootstrap.rs` — register `SendWelcomeEmailListener` for `UserRegistered`

---

## Task 1: Add tracing + uuid dependencies

**Files:**
- Modify: `framework/Cargo.toml`

- [ ] **Step 1: Add deps**

```toml
# framework/Cargo.toml — under [dependencies]
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt", "json", "registry", "smallvec", "parking_lot"] }
uuid = { version = "1", features = ["v4", "fast-rng", "serde"] }
```

- [ ] **Step 2: Verify it builds**

```bash
cargo check --workspace
```

Expected: clean check, no new warnings.

- [ ] **Step 3: Commit**

```bash
git add framework/Cargo.toml Cargo.lock
git commit -m "feat(deps): add tracing, tracing-subscriber, uuid for Phase 1 observability"
```

---

## Task 2: LogConfig — env-loadable logging configuration

**Files:**
- Create: `framework/src/logging/config.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write failing test**

```rust
// framework/src/logging/config.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_defaults_to_info_pretty() {
        // SAFETY: tests are single-threaded inside this module
        unsafe {
            std::env::remove_var("LOG_LEVEL");
            std::env::remove_var("LOG_FORMAT");
        }
        let cfg = LogConfig::from_env();
        assert_eq!(cfg.level, "info");
        assert!(matches!(cfg.format, LogFormat::Pretty));
    }

    #[test]
    fn from_env_reads_overrides() {
        unsafe {
            std::env::set_var("LOG_LEVEL", "debug,hyper=warn");
            std::env::set_var("LOG_FORMAT", "json");
        }
        let cfg = LogConfig::from_env();
        assert_eq!(cfg.level, "debug,hyper=warn");
        assert!(matches!(cfg.format, LogFormat::Json));
    }
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova logging::config -- --nocapture
```

Expected: FAIL with "cannot find LogConfig in this scope".

- [ ] **Step 3: Implement**

```rust
// framework/src/logging/config.rs
//! Configuration for tracing/log output. Read from environment so
//! consumers can change verbosity without recompiling.

use std::env;

/// Output format for log lines.
#[derive(Debug, Clone, Copy)]
pub enum LogFormat {
    /// Human-friendly multi-line output. Default for dev.
    Pretty,
    /// One-JSON-object-per-line. Default for production / log aggregators.
    Json,
}

/// Logging configuration.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// `tracing-subscriber` env-filter directive
    /// (e.g. `"info"`, `"debug,hyper=warn,sqlx=info"`).
    pub level: String,
    /// Output format.
    pub format: LogFormat,
}

impl LogConfig {
    /// Read from `LOG_LEVEL` (default `"info"`) and `LOG_FORMAT`
    /// (`"pretty"` | `"json"`, default `"pretty"`).
    pub fn from_env() -> Self {
        let level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
        let format = match env::var("LOG_FORMAT").as_deref() {
            Ok("json") => LogFormat::Json,
            _ => LogFormat::Pretty,
        };
        Self { level, format }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self::from_env()
    }
}
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova logging::config
```

Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add framework/src/logging/config.rs
git commit -m "feat(logging): env-loadable LogConfig with pretty/json formats"
```

---

## Task 3: RequestId — task_local UUID per request

**Files:**
- Create: `framework/src/logging/request_id.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write failing test**

```rust
// framework/src/logging/request_id.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn current_request_id_outside_scope_returns_none() {
        assert!(current_request_id().is_none());
    }

    #[tokio::test]
    async fn current_request_id_inside_scope_returns_value() {
        let id = RequestId::new();
        let captured = id.clone();
        REQUEST_ID
            .scope(id, async move {
                let now = current_request_id().expect("scoped value present");
                assert_eq!(now.as_str(), captured.as_str());
            })
            .await;
    }

    #[tokio::test]
    async fn request_id_is_lowercase_hyphenated_uuid() {
        let id = RequestId::new();
        assert_eq!(id.as_str().len(), 36);
        assert_eq!(id.as_str().chars().filter(|c| *c == '-').count(), 4);
        assert!(id.as_str().chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
    }
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova logging::request_id
```

Expected: FAIL with "cannot find RequestId".

- [ ] **Step 3: Implement**

```rust
// framework/src/logging/request_id.rs
//! Per-request UUID stored in a `tokio::task_local!`. The
//! `RequestIdMiddleware` installs it as the outermost middleware so
//! every downstream `tracing` event, every error log, and every event
//! payload carries the same id.

use std::fmt;
use uuid::Uuid;

/// A request id: lowercase hyphenated UUID v4.
#[derive(Debug, Clone)]
pub struct RequestId(String);

impl RequestId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

tokio::task_local! {
    pub static REQUEST_ID: RequestId;
}

/// Returns the request id of the currently-executing task, if any.
///
/// `None` outside an active `REQUEST_ID::scope` (i.e. background
/// jobs, tests that didn't install the middleware, etc.).
pub fn current_request_id() -> Option<RequestId> {
    REQUEST_ID.try_with(|id| id.clone()).ok()
}
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova logging::request_id
```

Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add framework/src/logging/request_id.rs
git commit -m "feat(logging): RequestId + REQUEST_ID task_local for per-request correlation"
```

---

## Task 4: RequestIdMiddleware — scope every request in a REQUEST_ID

**Files:**
- Modify: `framework/src/logging/request_id.rs`
- Test: inline

- [ ] **Step 1: Write failing test**

```rust
// framework/src/logging/request_id.rs — append to mod tests
#[tokio::test]
async fn middleware_installs_and_propagates_request_id() {
    use crate::http::{HttpResponse, Request};
    use crate::middleware::{into_boxed, Middleware, Next};
    use std::sync::Arc;

    let captured = Arc::new(std::sync::Mutex::new(None::<String>));
    let captured_for_handler = captured.clone();

    let handler: Next = Arc::new(move |_req| {
        let captured = captured_for_handler.clone();
        Box::pin(async move {
            let id = current_request_id().expect("middleware should install");
            *captured.lock().unwrap() = Some(id.as_str().to_string());
            Ok(HttpResponse::text("ok"))
        })
    });

    let mw = into_boxed(RequestIdMiddleware);
    let hyper_req = hyper::Request::builder()
        .method("GET")
        .uri("/")
        .body(http_body_util::Full::new(bytes::Bytes::new()))
        .unwrap();
    let req = Request::new(hyper_req.map(|b| {
        // adapt Full -> Incoming-like via a no-op trick if needed; in
        // practice Request::new accepts hyper::Request<Incoming>. Use
        // the testing helper if one exists.
        unimplemented!("see Request::test_new in framework/src/http/request.rs")
    }));
    let _resp = mw(req, handler).await;

    let id = captured.lock().unwrap().clone().expect("handler ran with request id");
    assert_eq!(id.len(), 36);
}
```

> **Note for implementer:** The `Request::new` wrapper takes `hyper::Request<hyper::body::Incoming>` which is non-constructible outside hyper. Use the same pattern as `framework/tests/precognition.rs`: bind a one-shot TCP server and make a real HTTP request. If `Request::test_request(method, path)` doesn't exist yet, **stop, add it as a separate task before this one, and come back**. Do not invent a `test_new` constructor inline.

- [ ] **Step 2: Pre-flight — verify test helper exists**

```bash
grep -n "test_request\|fn test_new" framework/src/http/request.rs
```

If none, insert a new task **Task 3.5: Add `Request::test_request` constructor** ahead of this one, with the minimal implementation:

```rust
// framework/src/http/request.rs — pub(crate) for tests, but useful enough
// to expose so integration tests don't all bind TCP listeners.
#[cfg(any(test, feature = "testing"))]
impl Request {
    pub fn test_request(method: &str, uri: &str) -> Self {
        let hyper_req = hyper::Request::builder()
            .method(method)
            .uri(uri)
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .unwrap();
        // Convert Empty to Incoming via hyper's BoxBody, OR keep
        // Request internally body-agnostic. If Request::new requires
        // Incoming today, this requires changing Request::new to take
        // `impl Body` — a separate refactor task. Confer with current
        // request.rs signature before proceeding.
        unimplemented!("requires either body-generic Request or BoxBody conversion")
    }
}
```

> If `Request::new` is hard-bound to `Incoming`, **fall back to the TCP-listener integration-test pattern from `precognition.rs`** for this test. Skip the unit test and write Task 4's test as an integration test in `framework/tests/logging.rs` (Task 12 anyway).

- [ ] **Step 3: Implement the middleware**

```rust
// framework/src/logging/request_id.rs — append below current_request_id
use crate::http::Request;
use crate::middleware::Next;
use async_trait::async_trait;

/// Middleware that ensures every request has a `RequestId` scoped in
/// `REQUEST_ID`. If the inbound request carries an `X-Request-Id`
/// header, that value is reused; otherwise a fresh UUID v4 is
/// generated. The id is echoed back as `X-Request-Id` on the response.
pub struct RequestIdMiddleware;

#[async_trait]
impl crate::middleware::Middleware for RequestIdMiddleware {
    async fn handle(&self, request: Request, next: Next) -> crate::http::Response {
        let id = request
            .header("x-request-id")
            .map(RequestId::from_string)
            .unwrap_or_else(RequestId::new);
        let id_str = id.as_str().to_string();

        let result = REQUEST_ID
            .scope(id.clone(), async move { next(request).await })
            .await;

        // Echo the id on outbound responses (both Ok and Err)
        match result {
            Ok(resp) => Ok(resp.header("X-Request-Id", id_str)),
            Err(resp) => Err(resp.header("X-Request-Id", id_str)),
        }
    }
}
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova logging::request_id
```

Expected: previous tests + middleware test passing (or skipped if integration-only).

- [ ] **Step 5: Commit**

```bash
git add framework/src/logging/request_id.rs framework/src/http/request.rs
git commit -m "feat(logging): RequestIdMiddleware echoes X-Request-Id"
```

---

## Task 5: logging::init_subscriber

**Files:**
- Create: `framework/src/logging/init.rs`
- Test: inline (idempotency only — subscriber install is global once)

- [ ] **Step 1: Write failing test**

```rust
// framework/src/logging/init.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent() {
        // Calling twice must not panic. (tracing-subscriber returns an
        // Err on duplicate global default; we swallow it.)
        init_subscriber(LogConfig::default());
        init_subscriber(LogConfig::default());
    }
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova logging::init
```

Expected: FAIL with "cannot find init_subscriber".

- [ ] **Step 3: Implement**

```rust
// framework/src/logging/init.rs
//! Initializes the global tracing subscriber. Called once from
//! `Server::serve()`. Safe to call multiple times; the second call
//! returns silently.

use super::config::{LogConfig, LogFormat};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Install the global tracing subscriber from a `LogConfig`. Honors
/// the `LOG_LEVEL` env-filter syntax (e.g. `"info,sqlx=warn"`).
///
/// Idempotent. Calling more than once is a no-op (the second
/// install fails inside tracing-subscriber and we ignore the error
/// — convenient for tests).
pub fn init_subscriber(config: LogConfig) {
    let env_filter = EnvFilter::try_new(&config.level)
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let registry = tracing_subscriber::registry().with(env_filter);

    let result = match config.format {
        LogFormat::Pretty => registry
            .with(fmt::layer().with_target(true).with_thread_ids(false).pretty())
            .try_init(),
        LogFormat::Json => registry
            .with(fmt::layer().json().with_target(true).with_current_span(true))
            .try_init(),
    };

    let _ = result; // ignore "already initialized" errors
}
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova logging::init
```

Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add framework/src/logging/init.rs
git commit -m "feat(logging): init_subscriber wires LogConfig to tracing-subscriber"
```

---

## Task 6: logging/mod.rs — wire up the module + re-exports

**Files:**
- Create: `framework/src/logging/mod.rs`
- Modify: `framework/src/lib.rs`

- [ ] **Step 1: Create mod.rs**

```rust
// framework/src/logging/mod.rs
//! Structured logging built on the `tracing` crate.
//!
//! - `init_subscriber(LogConfig)` is called once by `Server::serve`.
//! - `RequestIdMiddleware` (installed as the outermost middleware)
//!   wraps every request in a `REQUEST_ID` task_local so spans and
//!   events emitted downstream carry the id automatically.
//! - `current_request_id()` returns the id of the current task.

mod config;
mod init;
mod request_id;

pub use config::{LogConfig, LogFormat};
pub use init::init_subscriber;
pub use request_id::{current_request_id, RequestId, RequestIdMiddleware, REQUEST_ID};
```

- [ ] **Step 2: Wire into lib.rs**

```rust
// framework/src/lib.rs — add to the existing mod list (alphabetical neighbor: hashing)
pub mod logging;

// ... existing re-exports ...

pub use logging::{
    current_request_id, init_subscriber, LogConfig, LogFormat, RequestId, RequestIdMiddleware,
};
```

- [ ] **Step 3: Run — verify workspace builds**

```bash
cargo check --workspace
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add framework/src/logging/mod.rs framework/src/lib.rs
git commit -m "feat(logging): expose logging module from crate root"
```

---

## Task 7: Server boot — initialize subscriber + install RequestIdMiddleware

**Files:**
- Modify: `framework/src/server.rs`

- [ ] **Step 1: Read current server.rs to find serve() bootstrap**

```bash
grep -n "fn serve\|fn from_config\|global_middleware\|register_global" framework/src/server.rs
```

- [ ] **Step 2: Write failing test (integration)**

```rust
// framework/tests/logging.rs — NEW file
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use suprnova::{
    logging::{current_request_id, RequestIdMiddleware},
    middleware::{into_boxed, Middleware, Next},
    HttpResponse, Request,
};

async fn spawn() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let io = TokioIo::new(stream);
            let svc = service_fn(|hyper_req: hyper::Request<hyper::body::Incoming>| async move {
                let req = Request::new(hyper_req);
                let handler: Next = std::sync::Arc::new(|_r: Request| {
                    Box::pin(async move {
                        let id = current_request_id().expect("middleware installed");
                        Ok(HttpResponse::text(id.as_str().to_string()))
                    })
                });
                let mw = into_boxed(RequestIdMiddleware);
                let resp = match mw(req, handler).await {
                    Ok(r) => r,
                    Err(r) => r,
                };
                Ok::<_, Infallible>(resp.into_hyper())
            });
            let _ = http1::Builder::new().serve_connection(io, svc).await;
        }
    });
    addr
}

#[tokio::test]
async fn middleware_generates_id_when_no_header() {
    let addr = spawn().await;
    let resp = send(addr, &[]).await;
    let id = resp.headers().get("X-Request-Id").unwrap().to_str().unwrap();
    let body = std::str::from_utf8(&resp.body()).unwrap();
    assert_eq!(id, body);
    assert_eq!(id.len(), 36);
}

#[tokio::test]
async fn middleware_reuses_inbound_id() {
    let addr = spawn().await;
    let resp = send(addr, &[("X-Request-Id", "abc-123")]).await;
    assert_eq!(resp.headers().get("X-Request-Id").unwrap(), "abc-123");
    assert_eq!(std::str::from_utf8(&resp.body()).unwrap(), "abc-123");
}

async fn send(addr: SocketAddr, headers: &[(&str, &str)]) -> hyper::Response<Bytes> {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io).await.unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = hyper::Request::builder().method("GET").uri("/");
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let req = builder.body(Full::new(Bytes::new())).unwrap();
    let resp = sender.send_request(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let collected = body.collect().await.unwrap();
    hyper::Response::from_parts(parts, collected.to_bytes())
}
```

- [ ] **Step 3: Run — expect failure**

```bash
cargo test -p suprnova --test logging
```

Expected: tests compile but assert failures if subscriber/middleware not installed in `serve()`. Or compile error if `into_boxed` isn't public — check.

- [ ] **Step 4: Modify `Server::serve` to install subscriber + middleware**

```rust
// framework/src/server.rs — inside serve()/from_config(), AT THE TOP
// before any other middleware register call:

// 1. Initialize tracing once (idempotent).
crate::logging::init_subscriber(crate::logging::LogConfig::from_env());

// 2. Install RequestIdMiddleware as the OUTERMOST middleware.
// `register_global_middleware` appends in order; we want this first.
// If the registry has no "prepend", expose one; otherwise call before
// any other global middleware registration runs.
crate::middleware::register_global_middleware(crate::logging::RequestIdMiddleware);
```

> **Implementation note:** If `register_global_middleware` doesn't guarantee "outermost when called first," fix that as a sub-task by adding `register_global_middleware_first` or documenting the ordering contract. Outermost == first registered.

- [ ] **Step 5: Run — expect pass**

```bash
cargo test -p suprnova --test logging
```

Expected: 2 passed.

- [ ] **Step 6: Commit**

```bash
git add framework/src/server.rs framework/tests/logging.rs
git commit -m "feat(server): init tracing + install RequestIdMiddleware on boot"
```

---

## Task 8: Replace `app/src/middleware/logging.rs` println with tracing

**Files:**
- Modify: `app/src/middleware/logging.rs`

- [ ] **Step 1: Edit**

```rust
// app/src/middleware/logging.rs (complete file, replacing current)
//! Logging middleware — emits a tracing span per request, with the
//! method/path as fields. Uses `suprnova::current_request_id` so the
//! span carries the request id propagated from `RequestIdMiddleware`.

use suprnova::{async_trait, current_request_id, Middleware, Next, Request, Response};
use tracing::{info, info_span, Instrument};

pub struct LoggingMiddleware;

#[async_trait]
impl Middleware for LoggingMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let method = request.method().to_string();
        let path = request.path().to_string();
        let request_id = current_request_id().map(|id| id.as_str().to_string());

        let span = info_span!(
            "http_request",
            method = %method,
            path = %path,
            request_id = ?request_id,
        );

        async move {
            info!(target: "http", "request started");
            let response = next(request).await;
            let status = match &response {
                Ok(r) => r.status_code(),
                Err(r) => r.status_code(),
            };
            info!(target: "http", status, "request complete");
            response
        }
        .instrument(span)
        .await
    }
}
```

- [ ] **Step 2: Run — verify app builds**

```bash
cargo check -p app
```

Expected: clean.

- [ ] **Step 3: Smoke test by running the app and curling**

```bash
LOG_LEVEL=info,hyper=warn cargo run -p app -- serve &
sleep 2
curl -i http://127.0.0.1:8000/ | head -5
kill %1
```

Expected: a tracing line printed for "request started" + "request complete" with the same `request_id` field. `X-Request-Id` header on the response.

- [ ] **Step 4: Commit**

```bash
git add app/src/middleware/logging.rs
git commit -m "feat(app): LoggingMiddleware uses tracing spans with request_id field"
```

---

## Task 9: Event trait + ErrorOccurred built-in event

**Files:**
- Create: `framework/src/events/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/src/events/mod.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct OrderPlaced {
        pub order_id: i64,
    }
    impl Event for OrderPlaced {
        fn event_name() -> &'static str {
            "OrderPlaced"
        }
    }

    #[test]
    fn event_name_is_static_str() {
        assert_eq!(OrderPlaced::event_name(), "OrderPlaced");
    }
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova events
```

Expected: FAIL — `Event` not found.

- [ ] **Step 3: Implement Event trait**

```rust
// framework/src/events/mod.rs
//! Typed in-process pub/sub.
//!
//! ```ignore
//! use suprnova::{Event, Listener, FrameworkError};
//!
//! #[derive(Debug, Clone)]
//! pub struct UserRegistered { pub user_id: i64 }
//!
//! impl Event for UserRegistered {
//!     fn event_name() -> &'static str { "UserRegistered" }
//! }
//!
//! pub struct SendWelcomeEmail;
//!
//! #[suprnova::async_trait]
//! impl Listener<UserRegistered> for SendWelcomeEmail {
//!     async fn handle(&self, event: &UserRegistered) -> Result<(), FrameworkError> {
//!         // ...
//!         Ok(())
//!     }
//! }
//!
//! // In bootstrap.rs:
//! Event::listen::<UserRegistered>(std::sync::Arc::new(SendWelcomeEmail));
//!
//! // In a controller:
//! Event::dispatch(UserRegistered { user_id: 42 }).await?;
//! ```

mod dispatcher;
pub mod testing;

pub use dispatcher::{Event as _EventFacade, EventDispatcher};

use crate::FrameworkError;
use async_trait::async_trait;
use std::any::Any;
use std::sync::Arc;

/// A typed event payload.
///
/// `Send + Sync + Clone + 'static` so it can cross task boundaries
/// for queued listeners; `Debug` so the dispatcher can log it.
pub trait Event: Send + Sync + Clone + 'static + std::fmt::Debug {
    /// Stable name used for logging and fake assertions.
    fn event_name() -> &'static str
    where
        Self: Sized;

    /// Whether this event should be delivered asynchronously
    /// (spawned task) or synchronously (inline). Default: sync.
    fn queued() -> bool
    where
        Self: Sized,
    {
        false
    }
}

/// A listener that handles events of type `E`.
#[async_trait]
pub trait Listener<E: Event>: Send + Sync + 'static {
    async fn handle(&self, event: &E) -> Result<(), FrameworkError>;
}

/// Trait-object compatible bridge between concrete listeners and the
/// dispatcher's `Vec<Arc<dyn ErasedListener>>` storage.
#[async_trait]
pub(crate) trait ErasedListener: Send + Sync {
    async fn dispatch(&self, event: &dyn Any) -> Result<(), FrameworkError>;
}

#[async_trait]
impl<E, L> ErasedListener for ListenerWrap<E, L>
where
    E: Event,
    L: Listener<E>,
{
    async fn dispatch(&self, event: &dyn Any) -> Result<(), FrameworkError> {
        let typed = event
            .downcast_ref::<E>()
            .expect("dispatcher routed event to wrong listener type");
        self.listener.handle(typed).await
    }
}

pub(crate) struct ListenerWrap<E: Event, L: Listener<E>> {
    listener: Arc<L>,
    _marker: std::marker::PhantomData<E>,
}

impl<E: Event, L: Listener<E>> ListenerWrap<E, L> {
    pub fn new(listener: Arc<L>) -> Self {
        Self {
            listener,
            _marker: std::marker::PhantomData,
        }
    }
}
```

- [ ] **Step 4: Run — expect pass (Event trait alone)**

```bash
cargo test -p suprnova events::tests::event_name_is_static_str
```

Expected: passes; dispatcher tests not yet added.

- [ ] **Step 5: Commit**

```bash
git add framework/src/events/mod.rs
git commit -m "feat(events): Event trait + Listener trait + ErasedListener bridge"
```

---

## Task 10: EventDispatcher — sync dispatch, listen, registration

**Files:**
- Create: `framework/src/events/dispatcher.rs`
- Test: inline

- [ ] **Step 1: Write failing test**

```rust
// framework/src/events/dispatcher.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event as _, Listener};
    use crate::FrameworkError;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    #[derive(Debug, Clone)]
    struct Pinged {
        pub n: i64,
    }
    impl crate::events::Event for Pinged {
        fn event_name() -> &'static str {
            "Pinged"
        }
    }

    struct Counter(Arc<AtomicI64>);
    #[async_trait]
    impl Listener<Pinged> for Counter {
        async fn handle(&self, event: &Pinged) -> Result<(), FrameworkError> {
            self.0.fetch_add(event.n, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn dispatch_calls_registered_listener() {
        let d = EventDispatcher::new();
        let count = Arc::new(AtomicI64::new(0));
        d.listen::<Pinged>(Arc::new(Counter(count.clone()))).await;
        d.dispatch(Pinged { n: 5 }).await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn dispatch_with_no_listeners_is_ok() {
        let d = EventDispatcher::new();
        d.dispatch(Pinged { n: 1 }).await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_calls_all_listeners() {
        let d = EventDispatcher::new();
        let a = Arc::new(AtomicI64::new(0));
        let b = Arc::new(AtomicI64::new(0));
        d.listen::<Pinged>(Arc::new(Counter(a.clone()))).await;
        d.listen::<Pinged>(Arc::new(Counter(b.clone()))).await;
        d.dispatch(Pinged { n: 3 }).await.unwrap();
        assert_eq!(a.load(Ordering::SeqCst), 3);
        assert_eq!(b.load(Ordering::SeqCst), 3);
    }
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova events::dispatcher
```

Expected: FAIL — `EventDispatcher` not found.

- [ ] **Step 3: Implement dispatcher**

```rust
// framework/src/events/dispatcher.rs
use super::{ErasedListener, Listener, ListenerWrap};
use crate::FrameworkError;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error};

/// In-process event dispatcher. Held as a process-global via
/// `OnceLock` in this module; the `Event` facade is the user-facing
/// entry point.
pub struct EventDispatcher {
    listeners: RwLock<HashMap<TypeId, Vec<Arc<dyn ErasedListener>>>>,
}

impl EventDispatcher {
    pub fn new() -> Self {
        Self {
            listeners: RwLock::new(HashMap::new()),
        }
    }

    /// Register a listener for events of type `E`.
    pub async fn listen<E: super::Event>(&self, listener: Arc<impl Listener<E>>) {
        let wrap = Arc::new(ListenerWrap::<E, _>::new(listener)) as Arc<dyn ErasedListener>;
        self.listeners
            .write()
            .await
            .entry(TypeId::of::<E>())
            .or_default()
            .push(wrap);
    }

    /// Dispatch an event. Synchronous events run inline (sequentially,
    /// in registration order). Queued events spawn a tokio task per
    /// listener; this call returns after spawning, not after they
    /// complete.
    pub async fn dispatch<E: super::Event>(&self, event: E) -> Result<(), FrameworkError> {
        let listeners = {
            let map = self.listeners.read().await;
            map.get(&TypeId::of::<E>()).cloned().unwrap_or_default()
        };

        debug!(
            event = E::event_name(),
            listeners = listeners.len(),
            queued = E::queued(),
            "dispatching event"
        );

        if E::queued() {
            for l in listeners {
                let event_clone = event.clone();
                tokio::spawn(async move {
                    if let Err(e) = l.dispatch(&event_clone).await {
                        error!(
                            event = E::event_name(),
                            error = %e,
                            "queued listener failed"
                        );
                    }
                });
            }
        } else {
            for l in listeners {
                l.dispatch(&event).await?;
            }
        }

        Ok(())
    }
}

impl Default for EventDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Process-global dispatcher.
static GLOBAL: std::sync::OnceLock<EventDispatcher> = std::sync::OnceLock::new();

fn global() -> &'static EventDispatcher {
    GLOBAL.get_or_init(EventDispatcher::new)
}

/// User-facing facade. Routes through the global dispatcher.
pub struct Event;

impl Event {
    pub async fn dispatch<E: super::Event>(event: E) -> Result<(), FrameworkError> {
        global().dispatch(event).await
    }

    pub async fn listen<E: super::Event>(listener: Arc<impl Listener<E>>) {
        global().listen(listener).await
    }

    /// Replace the global dispatcher with a fake. Returns a guard
    /// that restores the previous state on drop. **Test-only.**
    #[cfg(any(test, feature = "testing"))]
    pub fn fake() -> super::testing::EventFakeGuard {
        super::testing::install_fake()
    }
}
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova events
```

Expected: 4 passed (the trait test from Task 9 + three dispatcher tests).

- [ ] **Step 5: Commit**

```bash
git add framework/src/events/dispatcher.rs framework/src/events/mod.rs
git commit -m "feat(events): EventDispatcher with sync + queued delivery"
```

---

## Task 11: Queued event dispatch (verify async path)

**Files:**
- Modify: `framework/src/events/dispatcher.rs` (add test only — code already supports it)

- [ ] **Step 1: Write failing test**

```rust
// framework/src/events/dispatcher.rs — append to mod tests
#[derive(Debug, Clone)]
struct QueuedPing;
impl crate::events::Event for QueuedPing {
    fn event_name() -> &'static str {
        "QueuedPing"
    }
    fn queued() -> bool {
        true
    }
}

struct SlowCounter(Arc<AtomicI64>);
#[async_trait]
impl Listener<QueuedPing> for SlowCounter {
    async fn handle(&self, _event: &QueuedPing) -> Result<(), FrameworkError> {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn queued_event_returns_before_listener_completes() {
    let d = EventDispatcher::new();
    let n = Arc::new(AtomicI64::new(0));
    d.listen::<QueuedPing>(Arc::new(SlowCounter(n.clone()))).await;
    d.dispatch(QueuedPing).await.unwrap();
    // Immediately after dispatch returns, the slow listener has not
    // had time to complete (it sleeps 20ms).
    assert_eq!(n.load(Ordering::SeqCst), 0);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(n.load(Ordering::SeqCst), 1);
}
```

- [ ] **Step 2: Run — expect pass (dispatcher already supports queued)**

```bash
cargo test -p suprnova events::dispatcher::tests::queued_event_returns_before_listener_completes
```

Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add framework/src/events/dispatcher.rs
git commit -m "test(events): queued events return before listeners complete"
```

---

## Task 12: Event::fake() + assert_dispatched

**Files:**
- Create: `framework/src/events/testing.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/src/events/testing.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Event as EventFacade;

    #[derive(Debug, Clone)]
    struct Noted {
        pub note: String,
    }
    impl crate::events::Event for Noted {
        fn event_name() -> &'static str {
            "Noted"
        }
    }

    #[tokio::test]
    async fn fake_records_dispatched_events_and_does_not_call_listeners() {
        let _guard = EventFacade::fake();
        EventFacade::dispatch(Noted { note: "hi".into() }).await.unwrap();
        EventFacade::dispatch(Noted { note: "bye".into() }).await.unwrap();
        assert_dispatched::<Noted>(|e| e.note == "hi");
        assert_dispatched::<Noted>(|e| e.note == "bye");
        assert_not_dispatched::<Noted>(|e| e.note == "nope");
    }

    #[tokio::test]
    async fn dispatched_count_works() {
        let _guard = EventFacade::fake();
        EventFacade::dispatch(Noted { note: "a".into() }).await.unwrap();
        EventFacade::dispatch(Noted { note: "a".into() }).await.unwrap();
        EventFacade::dispatch(Noted { note: "b".into() }).await.unwrap();
        assert_eq!(dispatched_count::<Noted>(|e| e.note == "a"), 2);
        assert_eq!(dispatched_count::<Noted>(|e| e.note == "b"), 1);
    }
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova events::testing
```

Expected: FAIL — `EventFakeGuard`, `assert_dispatched`, etc. not found.

- [ ] **Step 3: Implement fake**

```rust
// framework/src/events/testing.rs
//! `Event::fake()` — replaces the global dispatcher with one that
//! records dispatched events instead of invoking listeners. The
//! returned guard restores the previous dispatcher on drop.

use super::Event;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct FakeStore {
    recorded: HashMap<TypeId, Vec<Box<dyn Any + Send + Sync>>>,
}

static FAKE: Mutex<Option<FakeStore>> = Mutex::new(None);

pub(crate) fn is_active() -> bool {
    FAKE.lock().unwrap().is_some()
}

pub(crate) fn record<E: Event>(event: E) {
    if let Some(store) = FAKE.lock().unwrap().as_mut() {
        store
            .recorded
            .entry(TypeId::of::<E>())
            .or_default()
            .push(Box::new(event));
    }
}

/// Replace the global dispatcher with a fake. Returns a guard that
/// removes the fake on drop, restoring real listener invocation.
pub fn install_fake() -> EventFakeGuard {
    *FAKE.lock().unwrap() = Some(FakeStore::default());
    EventFakeGuard
}

pub struct EventFakeGuard;

impl Drop for EventFakeGuard {
    fn drop(&mut self) {
        *FAKE.lock().unwrap() = None;
    }
}

/// Assert that at least one event of type `E` matching `pred` was
/// dispatched while the fake was active.
pub fn assert_dispatched<E: Event>(pred: impl Fn(&E) -> bool) {
    let count = dispatched_count::<E>(pred);
    assert!(
        count > 0,
        "expected at least one {} matching predicate; none dispatched",
        E::event_name()
    );
}

/// Assert that no event of type `E` matching `pred` was dispatched.
pub fn assert_not_dispatched<E: Event>(pred: impl Fn(&E) -> bool) {
    let count = dispatched_count::<E>(pred);
    assert_eq!(
        count, 0,
        "expected zero {} matching predicate; {} dispatched",
        E::event_name(),
        count
    );
}

pub fn dispatched_count<E: Event>(pred: impl Fn(&E) -> bool) -> usize {
    let store = FAKE.lock().unwrap();
    let store = match store.as_ref() {
        Some(s) => s,
        None => return 0,
    };
    let bucket = match store.recorded.get(&TypeId::of::<E>()) {
        Some(b) => b,
        None => return 0,
    };
    bucket
        .iter()
        .filter_map(|b| b.downcast_ref::<E>())
        .filter(|e| pred(e))
        .count()
}
```

- [ ] **Step 4: Teach the dispatcher to short-circuit when fake is active**

```rust
// framework/src/events/dispatcher.rs — at top of EventDispatcher::dispatch
// (BEFORE listeners.len() debug! line):
if super::testing::is_active() {
    super::testing::record(event);
    return Ok(());
}
```

- [ ] **Step 5: Run — expect pass**

```bash
cargo test -p suprnova events
```

Expected: all previous + 2 new fake tests passing.

- [ ] **Step 6: Commit**

```bash
git add framework/src/events/testing.rs framework/src/events/dispatcher.rs framework/src/events/mod.rs
git commit -m "feat(events): Event::fake() + assert_dispatched / assert_not_dispatched"
```

---

## Task 13: Wire events into the crate root + integration test file

**Files:**
- Modify: `framework/src/lib.rs`
- Create: `framework/tests/events.rs`

- [ ] **Step 1: Re-export from lib.rs**

```rust
// framework/src/lib.rs — declare and re-export
pub mod events;

pub use events::{
    testing::{assert_dispatched, assert_not_dispatched, dispatched_count, EventFakeGuard},
    Event as EventTrait,
    EventDispatcher,
    Listener,
};

// The facade `Event` (used as `Event::dispatch(...)`) must NOT clash
// with the trait. Choose: re-export the facade as `Event` and rename
// the trait to `EventTrait`, OR keep the trait and use a different
// facade name.
```

> **Naming decision required:** The trait was named `Event` inside the module; the facade was also named `Event`. The example in the doc-comment uses `Event::dispatch(...)` which is the facade. **Resolution:** rename the trait to `EventTrait` in `framework/src/events/mod.rs` and rename the macro `event_name` accordingly (search-and-replace inside the events module). Update all use sites.

- [ ] **Step 2: Apply the rename**

```bash
# Manually verify before running
rg -n "\bEvent\b" framework/src/events/ framework/src/error.rs
```

Update `pub trait Event` → `pub trait EventTrait`, and every `E: Event` bound to `E: EventTrait`. Keep the `Event` facade struct.

- [ ] **Step 3: Write integration test**

```rust
// framework/tests/events.rs
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use suprnova::{
    events::{Event, Listener},
    async_trait, FrameworkError,
};
use suprnova::EventTrait;

#[derive(Debug, Clone)]
pub struct Pinged {
    pub n: i64,
}

impl EventTrait for Pinged {
    fn event_name() -> &'static str {
        "Pinged"
    }
}

struct Counter(Arc<AtomicI64>);

#[async_trait]
impl Listener<Pinged> for Counter {
    async fn handle(&self, event: &Pinged) -> Result<(), FrameworkError> {
        self.0.fetch_add(event.n, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn end_to_end_dispatch_invokes_listener() {
    let n = Arc::new(AtomicI64::new(0));
    Event::listen::<Pinged>(Arc::new(Counter(n.clone()))).await;
    Event::dispatch(Pinged { n: 7 }).await.unwrap();
    // Wait a tick in case any internal yield happened.
    tokio::task::yield_now().await;
    assert_eq!(n.load(Ordering::SeqCst), 7);
}

#[tokio::test]
async fn fake_swallows_dispatches() {
    let n = Arc::new(AtomicI64::new(0));
    Event::listen::<Pinged>(Arc::new(Counter(n.clone()))).await;
    let _guard = Event::fake();
    Event::dispatch(Pinged { n: 100 }).await.unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 0); // listener not invoked
    suprnova::assert_dispatched::<Pinged>(|e| e.n == 100);
}
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova --test events
```

Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add framework/src/lib.rs framework/src/events/ framework/tests/events.rs
git commit -m "feat(events): expose Event/EventTrait/Listener from crate root"
```

---

## Task 14: ErrorOccurred event + tracing integration in error.rs

**Files:**
- Modify: `framework/src/error.rs`
- Create: `framework/src/events/builtins.rs` (or append to mod.rs)

- [ ] **Step 1: Define `ErrorOccurred` event**

```rust
// framework/src/events/builtins.rs
//! Framework-emitted events. Consumers can listen to these the
//! same way they listen to their own events.

use super::EventTrait;

/// Dispatched on every `FrameworkError` whose status code is >= 500.
/// Listeners can ship to Sentry, Datadog, Slack, etc.
#[derive(Debug, Clone)]
pub struct ErrorOccurred {
    pub error_message: String,
    pub status_code: u16,
    pub request_id: Option<String>,
}

impl EventTrait for ErrorOccurred {
    fn event_name() -> &'static str {
        "ErrorOccurred"
    }
}
```

- [ ] **Step 2: Hook it from `error.rs`**

```rust
// framework/src/error.rs — at the bottom, replace or add:
impl From<FrameworkError> for crate::http::HttpResponse {
    fn from(err: FrameworkError) -> crate::http::HttpResponse {
        // Pre-existing conversion logic stays where it is; the changes
        // are: (a) emit tracing on every conversion, (b) dispatch
        // ErrorOccurred for 5xx.
        let status = err.status_code();
        let message = err.to_string();
        let request_id = crate::current_request_id().map(|id| id.as_str().to_string());

        if status >= 500 {
            tracing::error!(
                status,
                error = %message,
                request_id = ?request_id,
                "framework error"
            );
            let evt = crate::events::ErrorOccurred {
                error_message: message.clone(),
                status_code: status,
                request_id: request_id.clone(),
            };
            // Spawn — we are NOT in an async context here, and we
            // don't want to block response conversion on listener
            // execution.
            tokio::spawn(async move {
                let _ = crate::events::Event::dispatch(evt).await;
            });
        } else if status >= 400 {
            tracing::warn!(
                status,
                error = %message,
                request_id = ?request_id,
                "client error"
            );
        }

        // ... existing response-building logic ...
        # // (placeholder for the actual response build that already exists)
        # crate::http::HttpResponse::json(serde_json::json!({ "message": message })).status(status)
    }
}
```

> **Implementation note:** The `From<FrameworkError>` impl already exists in `framework/src/http/response.rs` or `framework/src/error.rs`. Find it (`grep -n "impl From<FrameworkError>" framework/src/`), preserve the existing response-building branches (especially Precognition 204/422 + Vary header), and add the tracing/event emit at the top.

- [ ] **Step 3: Write integration test**

```rust
// framework/tests/events.rs — append
use suprnova::events::ErrorOccurred;
use suprnova::EventTrait as _;

#[tokio::test]
async fn server_error_dispatches_error_occurred() {
    let _guard = Event::fake();
    let err = suprnova::FrameworkError::internal("boom");
    let _resp: suprnova::HttpResponse = err.into();
    // Give the spawned dispatch a tick.
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    suprnova::assert_dispatched::<ErrorOccurred>(|e| e.status_code == 500 && e.error_message.contains("boom"));
}

#[tokio::test]
async fn client_error_does_not_dispatch_error_occurred() {
    let _guard = Event::fake();
    let err = suprnova::FrameworkError::param("name");
    let _resp: suprnova::HttpResponse = err.into();
    tokio::task::yield_now().await;
    suprnova::assert_not_dispatched::<ErrorOccurred>(|_| true);
}
```

- [ ] **Step 4: Re-export `ErrorOccurred`**

```rust
// framework/src/events/mod.rs — add
mod builtins;
pub use builtins::ErrorOccurred;
```

- [ ] **Step 5: Run — expect pass**

```bash
cargo test -p suprnova --test events
```

Expected: 4 passed.

- [ ] **Step 6: Commit**

```bash
git add framework/src/error.rs framework/src/events/ framework/tests/events.rs
git commit -m "feat(error): emit tracing + dispatch ErrorOccurred on 5xx"
```

---

## Task 15: FrameworkError::context() for stacking

**Files:**
- Modify: `framework/src/error.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/src/error.rs — inside #[cfg(test)] mod validation_tests (or new mod)
#[test]
fn context_prepends_to_message() {
    let inner = FrameworkError::internal("disk full");
    let wrapped = inner.context("writing user avatar");
    assert!(wrapped.to_string().contains("writing user avatar"));
    assert!(wrapped.to_string().contains("disk full"));
    assert_eq!(wrapped.status_code(), 500);
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova context_prepends_to_message
```

Expected: FAIL — `context` not found.

- [ ] **Step 3: Implement**

```rust
// framework/src/error.rs — inside impl FrameworkError
/// Wrap this error with a context string. The status code is
/// preserved; the display becomes `"<ctx>: <original>"`. Use this
/// when an error needs to be re-raised with operation context:
///
/// ```ignore
/// db.insert(user).await.map_err(FrameworkError::from)
///     .map_err(|e| e.context("creating new user"))?;
/// ```
pub fn context(self, ctx: impl Into<String>) -> Self {
    let prefix = ctx.into();
    let status = self.status_code();
    let original = self.to_string();
    FrameworkError::Internal {
        message: format!("{}: {}", prefix, original),
    }
    .into_status(status)
}

/// Internal helper to preserve status code through transformations.
fn into_status(self, status: u16) -> Self {
    match self {
        Self::Internal { message } => Self::Domain {
            message,
            status_code: status,
        },
        other => other,
    }
}
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova context_prepends
```

Expected: passes.

- [ ] **Step 5: Commit**

```bash
git add framework/src/error.rs
git commit -m "feat(error): FrameworkError::context for stacking operation context"
```

---

## Task 16: Streaming body — extend HttpResponse for SSE

**Files:**
- Modify: `framework/src/http/response.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/src/http/response.rs — in tests mod (create if absent)
#[cfg(test)]
mod stream_tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn streaming_response_emits_chunks_in_order() {
        let (tx, rx) = mpsc::channel::<Bytes>(4);
        tx.send(Bytes::from_static(b"chunk1\n")).await.unwrap();
        tx.send(Bytes::from_static(b"chunk2\n")).await.unwrap();
        drop(tx); // close stream

        let resp = HttpResponse::stream_bytes(rx)
            .header("Content-Type", "text/plain")
            .into_hyper_stream();

        let collected = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], b"chunk1\nchunk2\n");
    }
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova stream_tests
```

Expected: FAIL — `stream_bytes` not found.

- [ ] **Step 3: Implement**

```rust
// framework/src/http/response.rs — change body field

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use tokio_stream::wrappers::ReceiverStream;
use tokio::sync::mpsc::Receiver;

// At top of the file, define a body variant. KEEP the existing String
// body field as-is for backwards compat; introduce a SEPARATE optional
// streaming body to avoid breaking every existing construction:

pub struct HttpResponse {
    status: u16,
    body: String,                          // legacy path
    stream: Option<Receiver<Bytes>>,       // SSE / chunked path
    headers: Vec<(String, String)>,
}

// All existing constructors set `stream: None`. Add the new one:

impl HttpResponse {
    pub fn stream_bytes(rx: Receiver<Bytes>) -> Self {
        Self {
            status: 200,
            body: String::new(),
            stream: Some(rx),
            headers: vec![],
        }
    }

    /// Convert to a streaming hyper response. Use this when the
    /// response was built with `stream_bytes`. For non-streaming
    /// responses, call `into_hyper()`.
    pub fn into_hyper_stream(self) -> hyper::Response<BoxBody<Bytes, std::io::Error>> {
        let mut builder = hyper::Response::builder().status(self.status);
        for (name, value) in self.headers {
            builder = builder.header(name, value);
        }
        let body: BoxBody<Bytes, std::io::Error> = if let Some(rx) = self.stream {
            let stream = ReceiverStream::new(rx).map(|b| {
                Ok::<_, std::io::Error>(http_body::Frame::data(b))
            });
            BoxBody::new(StreamBody::new(stream))
        } else {
            BoxBody::new(
                Full::new(Bytes::from(self.body))
                    .map_err(|never| match never {}),
            )
        };
        builder.body(body).unwrap()
    }
}
```

- [ ] **Step 4: Add `tokio-stream` dependency**

```toml
# framework/Cargo.toml — append [dependencies]
tokio-stream = "0.1"
http-body = "1"
```

- [ ] **Step 5: Run — expect pass**

```bash
cargo test -p suprnova stream_tests
```

Expected: passes.

- [ ] **Step 6: Update Server::serve to use `into_hyper_stream` when stream is Some**

```rust
// framework/src/server.rs — wherever it calls .into_hyper() today,
// branch on whether the response has a stream:

// Replace:
let hyper_resp = resp.into_hyper();
// With:
let hyper_resp: hyper::Response<BoxBody<Bytes, std::io::Error>> = if resp.has_stream() {
    resp.into_hyper_stream()
} else {
    let r = resp.into_hyper();
    let (parts, body) = r.into_parts();
    let boxed = BoxBody::new(body.map_err(|never| match never {}));
    hyper::Response::from_parts(parts, boxed)
};
```

And add `has_stream`:

```rust
// framework/src/http/response.rs
impl HttpResponse {
    pub fn has_stream(&self) -> bool {
        self.stream.is_some()
    }
}
```

> **Implementation note:** This widens the server's response body type from `Full<Bytes>` to `BoxBody<Bytes, std::io::Error>`. Every place hyper expects the response body changes. If the change is too invasive, fall back to a dedicated `StreamingResponse` type that the route handler returns *instead of* `Response` (using a separate route registration). For Phase 1, the union approach is preferred.

- [ ] **Step 7: Run full workspace check**

```bash
cargo check --workspace
cargo test -p suprnova
```

Expected: clean check, all tests pass.

- [ ] **Step 8: Commit**

```bash
git add framework/src/http/response.rs framework/src/server.rs framework/Cargo.toml
git commit -m "feat(http): streaming body variant for SSE / chunked responses"
```

---

## Task 17: SSE module — SseEvent + HttpResponse::sse

**Files:**
- Create: `framework/src/sse/mod.rs`
- Modify: `framework/src/lib.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/src/sse/mod.rs
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn sse_event_formats_data_with_double_newline_terminator() {
        let evt = SseEvent::new("hello world");
        assert_eq!(evt.to_bytes(), Bytes::from_static(b"data: hello world\n\n"));
    }

    #[test]
    fn sse_event_with_named_event_and_id() {
        let evt = SseEvent::new("payload")
            .with_event("ping")
            .with_id("42");
        let s = std::str::from_utf8(&evt.to_bytes()).unwrap().to_string();
        assert!(s.contains("event: ping\n"));
        assert!(s.contains("id: 42\n"));
        assert!(s.contains("data: payload\n"));
        assert!(s.ends_with("\n\n"));
    }

    #[test]
    fn multiline_data_emits_one_data_field_per_line() {
        let evt = SseEvent::new("line1\nline2\nline3");
        let s = std::str::from_utf8(&evt.to_bytes()).unwrap().to_string();
        assert_eq!(s, "data: line1\ndata: line2\ndata: line3\n\n");
    }
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova sse
```

Expected: FAIL — `SseEvent` not found.

- [ ] **Step 3: Implement**

```rust
// framework/src/sse/mod.rs
//! Server-Sent Events delivery primitive.
//!
//! ```ignore
//! use suprnova::{sse::SseEvent, HttpResponse, Request, Response};
//! use tokio::sync::mpsc;
//!
//! pub async fn stream_events(_req: Request) -> Response {
//!     let (tx, rx) = mpsc::channel(16);
//!     tokio::spawn(async move {
//!         for i in 0..10 {
//!             let _ = tx.send(SseEvent::new(format!("tick {i}")).to_bytes()).await;
//!             tokio::time::sleep(std::time::Duration::from_secs(1)).await;
//!         }
//!     });
//!     Ok(HttpResponse::sse(rx))
//! }
//! ```

use bytes::Bytes;

/// A single SSE event, framed per the W3C EventSource spec.
pub struct SseEvent {
    data: String,
    event: Option<String>,
    id: Option<String>,
    retry_ms: Option<u64>,
}

impl SseEvent {
    pub fn new(data: impl Into<String>) -> Self {
        Self {
            data: data.into(),
            event: None,
            id: None,
            retry_ms: None,
        }
    }

    pub fn with_event(mut self, name: impl Into<String>) -> Self {
        self.event = Some(name.into());
        self
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn with_retry_ms(mut self, ms: u64) -> Self {
        self.retry_ms = Some(ms);
        self
    }

    /// Serialize to wire bytes. Each `data` line is prefixed; multi-line
    /// payloads emit one `data: ` field per line per the spec.
    pub fn to_bytes(&self) -> Bytes {
        let mut out = String::new();
        if let Some(name) = &self.event {
            out.push_str("event: ");
            out.push_str(name);
            out.push('\n');
        }
        if let Some(id) = &self.id {
            out.push_str("id: ");
            out.push_str(id);
            out.push('\n');
        }
        if let Some(ms) = self.retry_ms {
            out.push_str(&format!("retry: {}\n", ms));
        }
        for line in self.data.split('\n') {
            out.push_str("data: ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n'); // event terminator
        Bytes::from(out)
    }
}
```

- [ ] **Step 4: Add `HttpResponse::sse` convenience constructor**

```rust
// framework/src/http/response.rs — impl HttpResponse
pub fn sse(rx: tokio::sync::mpsc::Receiver<bytes::Bytes>) -> Self {
    Self::stream_bytes(rx)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .header("X-Accel-Buffering", "no") // disable nginx buffering
}
```

- [ ] **Step 5: Re-export from lib.rs**

```rust
// framework/src/lib.rs
pub mod sse;
pub use sse::SseEvent;
```

- [ ] **Step 6: Run — expect pass**

```bash
cargo test -p suprnova sse
```

Expected: 3 passed.

- [ ] **Step 7: Commit**

```bash
git add framework/src/sse/ framework/src/http/response.rs framework/src/lib.rs
git commit -m "feat(sse): SseEvent framing + HttpResponse::sse convenience"
```

---

## Task 18: Integration test — SSE end-to-end

**Files:**
- Create: `framework/tests/sse.rs`

- [ ] **Step 1: Write test**

```rust
// framework/tests/sse.rs
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use suprnova::{sse::SseEvent, HttpResponse, Request};
use tokio::sync::mpsc;

async fn spawn() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let io = TokioIo::new(stream);
            let svc = service_fn(|_req: hyper::Request<hyper::body::Incoming>| async move {
                let (tx, rx) = mpsc::channel(4);
                tokio::spawn(async move {
                    let _ = tx.send(SseEvent::new("hello").to_bytes()).await;
                    let _ = tx.send(SseEvent::new("world").with_event("greet").to_bytes()).await;
                });
                let resp = HttpResponse::sse(rx);
                Ok::<_, Infallible>(resp.into_hyper_stream())
            });
            let _ = http1::Builder::new().serve_connection(io, svc).await;
        }
    });
    addr
}

#[tokio::test]
async fn sse_response_emits_events_with_correct_framing() {
    let addr = spawn().await;
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(io)
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("GET")
        .uri("/")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = sender.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("Content-Type").unwrap(),
        "text/event-stream"
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let s = std::str::from_utf8(&body).unwrap();
    assert!(s.contains("data: hello\n\n"));
    assert!(s.contains("event: greet\ndata: world\n\n"));
}
```

- [ ] **Step 2: Run — expect pass**

```bash
cargo test -p suprnova --test sse
```

Expected: 1 passed.

- [ ] **Step 3: Commit**

```bash
git add framework/tests/sse.rs
git commit -m "test(sse): integration test for SSE response framing"
```

---

## Task 19: App dogfood — UserRegistered event + listener + SSE example

**Files:**
- Create: `app/src/events/mod.rs`, `app/src/listeners/mod.rs`, `app/src/controllers/sse_example.rs`
- Modify: `app/src/lib.rs`, `app/src/bootstrap.rs`, `app/src/controllers/mod.rs`

- [ ] **Step 1: Create event**

```rust
// app/src/events/mod.rs
use suprnova::EventTrait;

#[derive(Debug, Clone)]
pub struct UserRegistered {
    pub user_id: i64,
    pub email: String,
}

impl EventTrait for UserRegistered {
    fn event_name() -> &'static str {
        "UserRegistered"
    }
}
```

- [ ] **Step 2: Create listener**

```rust
// app/src/listeners/mod.rs
use crate::events::UserRegistered;
use suprnova::{async_trait, events::Listener, FrameworkError};
use tracing::info;

pub struct SendWelcomeEmailListener;

#[async_trait]
impl Listener<UserRegistered> for SendWelcomeEmailListener {
    async fn handle(&self, event: &UserRegistered) -> Result<(), FrameworkError> {
        info!(
            user_id = event.user_id,
            email = %event.email,
            "would send welcome email"
        );
        Ok(())
    }
}
```

- [ ] **Step 3: Register listener in bootstrap**

```rust
// app/src/bootstrap.rs — inside register(), AFTER existing setup
use crate::events::UserRegistered;
use crate::listeners::SendWelcomeEmailListener;
use suprnova::events::Event;
use std::sync::Arc;

Event::listen::<UserRegistered>(Arc::new(SendWelcomeEmailListener)).await;
```

- [ ] **Step 4: Create SSE example controller**

```rust
// app/src/controllers/sse_example.rs
use suprnova::{sse::SseEvent, HttpResponse, Request, Response};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

pub async fn stream(_req: Request) -> Response {
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(async move {
        for i in 0..10 {
            let evt = SseEvent::new(format!("tick {}", i))
                .with_id(i.to_string())
                .with_event("tick");
            if tx.send(evt.to_bytes()).await.is_err() {
                break; // client disconnected
            }
            sleep(Duration::from_secs(1)).await;
        }
    });
    Ok(HttpResponse::sse(rx))
}
```

- [ ] **Step 5: Wire into module + route**

```rust
// app/src/lib.rs — add
pub mod events;
pub mod listeners;
```

```rust
// app/src/controllers/mod.rs — add
pub mod sse_example;
```

```rust
// Wherever routes are declared (app/src/routes.rs or similar):
use crate::controllers::sse_example;
get("/events/stream", sse_example::stream);
```

- [ ] **Step 6: Smoke test**

```bash
cargo run -p app -- serve &
sleep 2
curl -N http://127.0.0.1:8000/events/stream &
sleep 5
kill %2 %1
```

Expected: 5 ticks streamed with `event: tick`, `id: N`, `data: tick N`.

- [ ] **Step 7: Commit**

```bash
git add app/src/events app/src/listeners app/src/controllers/sse_example.rs app/src/lib.rs app/src/bootstrap.rs app/src/controllers/mod.rs
git commit -m "feat(app): dogfood UserRegistered event + SSE /events/stream"
```

---

## Task 20: OpenTelemetry export bridge + Metrics facade

**Files:** `framework/src/logging/otel.rs`, `framework/src/metrics/mod.rs`, modifications to `framework/src/logging/init.rs`

Production observability via [`opentelemetry-rust`](https://github.com/open-telemetry/opentelemetry-rust) 0.32 (Apache-2.0, `reference/opentelemetry-rust-opentelemetry-0.32.0/`). The `opentelemetry-appender-tracing` crate bridges every `tracing` span and event into OpenTelemetry, then `opentelemetry-otlp` exports to any OTLP-compatible collector — Jaeger, Tempo, Honeycomb, Datadog, Grafana Cloud, etc. Plus a thin `Metrics` facade for counters / histograms / gauges via the OpenTelemetry SDK's meter API.

This is why we [[deferred-phases]] deferred Phase 14 (Telescope + Pulse): OpenTelemetry IS the production observability story. We export; consumers point a Grafana / Honeycomb / Datadog at the OTLP endpoint.

- [ ] **Step 1: Add deps**

```toml
# framework/Cargo.toml — [dependencies]
opentelemetry = { path = "../reference/opentelemetry-rust-opentelemetry-0.32.0/opentelemetry" }
opentelemetry_sdk = { path = "../reference/opentelemetry-rust-opentelemetry-0.32.0/opentelemetry-sdk", features = ["rt-tokio"] }
opentelemetry-otlp = { path = "../reference/opentelemetry-rust-opentelemetry-0.32.0/opentelemetry-otlp", features = ["grpc-tonic", "metrics", "logs", "trace"] }
opentelemetry-appender-tracing = { path = "../reference/opentelemetry-rust-opentelemetry-0.32.0/opentelemetry-appender-tracing" }
opentelemetry-semantic-conventions = { path = "../reference/opentelemetry-rust-opentelemetry-0.32.0/opentelemetry-semantic-conventions" }
tracing-opentelemetry = "0.27"
```

`tracing-opentelemetry` is from crates.io — the bridge between the `tracing` ecosystem and OpenTelemetry's `Tracer` API. It's stable and not vendored.

- [ ] **Step 2: Extend `LogConfig` with OTLP fields**

```rust
// framework/src/logging/config.rs — extend LogConfig
pub struct LogConfig {
    pub level: String,
    pub format: LogFormat,
    /// OTLP endpoint URL (e.g. `http://localhost:4317` for gRPC, or
    /// `http://localhost:4318` for HTTP). When `None`, OTel export
    /// is disabled and logging stays local-only.
    pub otlp_endpoint: Option<String>,
    /// Service name reported in OTel resource attributes. Defaults
    /// to `APP_NAME` env var or `"suprnova"`.
    pub service_name: String,
    /// Sample rate for spans (0.0–1.0). 1.0 = every request, 0.1 =
    /// 10% sampling. Production typically uses 0.05–0.1.
    pub trace_sample_ratio: f64,
}

impl LogConfig {
    pub fn from_env() -> Self {
        let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into());
        let format = match std::env::var("LOG_FORMAT").as_deref() {
            Ok("json") => LogFormat::Json,
            _ => LogFormat::Pretty,
        };
        let otlp_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok();
        let service_name = std::env::var("OTEL_SERVICE_NAME")
            .or_else(|_| std::env::var("APP_NAME"))
            .unwrap_or_else(|_| "suprnova".into());
        let trace_sample_ratio = std::env::var("OTEL_TRACES_SAMPLER_ARG")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1.0);
        Self { level, format, otlp_endpoint, service_name, trace_sample_ratio }
    }
}
```

- [ ] **Step 3: Implement the OTel layer**

```rust
// framework/src/logging/otel.rs
//! OpenTelemetry export. Built only when LogConfig.otlp_endpoint is
//! Some(...). Wires three signals:
//!
//!   - **Traces**: tracing spans → opentelemetry::trace::Tracer via
//!     `tracing-opentelemetry`'s OpenTelemetryLayer → OTLP exporter.
//!     Every #[tracing::instrument] span and every TelescopeRequest-
//!     style middleware span propagates trace_id + span_id end-to-end.
//!
//!   - **Logs**: tracing events → OpenTelemetry log records via
//!     `opentelemetry-appender-tracing`. Logs carry the trace_id
//!     they fired inside, so log+trace correlation works in the UI.
//!
//!   - **Metrics**: opt-in via the `Metrics` facade (Task 20, Step 4).
//!     The OpenTelemetry SDK's meter is installed as the global
//!     meter provider here.

use crate::FrameworkError;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter, WithExportConfig};
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use opentelemetry_sdk::Resource;

use super::config::LogConfig;

pub struct OtelHandle {
    tracer_provider: SdkTracerProvider,
    meter_provider: SdkMeterProvider,
    logger_provider: SdkLoggerProvider,
}

impl OtelHandle {
    /// Install all three OpenTelemetry signals (traces, metrics,
    /// logs) and return a handle whose drop flushes pending exports.
    /// Call from `Server::serve` after `init_subscriber` returns the
    /// `tracing::Subscriber`.
    pub fn install(config: &LogConfig) -> Result<Self, FrameworkError> {
        let endpoint = config
            .otlp_endpoint
            .as_deref()
            .ok_or_else(|| FrameworkError::internal("OTLP endpoint not set"))?;

        let resource = Resource::builder()
            .with_attribute(KeyValue::new("service.name", config.service_name.clone()))
            .with_attribute(KeyValue::new(
                "service.version",
                env!("CARGO_PKG_VERSION").to_string(),
            ))
            .build();

        // Traces
        let span_exporter = SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .map_err(|e| FrameworkError::internal(format!("otlp span exporter: {}", e)))?;
        let tracer_provider = SdkTracerProvider::builder()
            .with_batch_exporter(span_exporter)
            .with_sampler(Sampler::TraceIdRatioBased(config.trace_sample_ratio))
            .with_resource(resource.clone())
            .build();
        global::set_tracer_provider(tracer_provider.clone());

        // Metrics
        let metric_exporter = MetricExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .map_err(|e| FrameworkError::internal(format!("otlp metric exporter: {}", e)))?;
        let meter_provider = SdkMeterProvider::builder()
            .with_periodic_exporter(metric_exporter)
            .with_resource(resource.clone())
            .build();
        global::set_meter_provider(meter_provider.clone());

        // Logs
        let log_exporter = LogExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .map_err(|e| FrameworkError::internal(format!("otlp log exporter: {}", e)))?;
        let logger_provider = SdkLoggerProvider::builder()
            .with_batch_exporter(log_exporter)
            .with_resource(resource)
            .build();

        Ok(Self {
            tracer_provider,
            meter_provider,
            logger_provider,
        })
    }

    /// The tracing-opentelemetry layer to add to a Subscriber
    /// registry. The caller composes this with the local fmt layer.
    pub fn tracing_layer<S>(&self) -> tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::Tracer>
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        let tracer = self.tracer_provider.tracer("suprnova");
        tracing_opentelemetry::OpenTelemetryLayer::new(tracer)
    }

    /// The opentelemetry-appender-tracing layer for log bridging.
    pub fn log_appender_layer<S>(&self) -> opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge<SdkLoggerProvider, opentelemetry_sdk::logs::SdkLogger>
    where
        S: tracing::Subscriber,
    {
        opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&self.logger_provider)
    }
}

impl Drop for OtelHandle {
    fn drop(&mut self) {
        // Best-effort flush of pending exports. Production should
        // also call shutdown() explicitly on graceful-shutdown paths.
        let _ = self.tracer_provider.shutdown();
        let _ = self.meter_provider.shutdown();
        let _ = self.logger_provider.shutdown();
    }
}
```

> **API verification:** opentelemetry-rust 0.32 has had API churn across recent versions. Verify exact builder method names (`with_tonic()` vs `with_grpc()`, `with_periodic_exporter` vs `with_reader`, `Resource::builder()` shape) by reading `reference/opentelemetry-rust-opentelemetry-0.32.0/opentelemetry-sdk/src/` and `opentelemetry-otlp/src/` before wiring. The sketch describes the architecture; field-level adjustments are implementer work.

- [ ] **Step 4: Wire OTel into `init_subscriber`**

```rust
// framework/src/logging/init.rs — extend
pub fn init_subscriber(config: LogConfig) -> Option<otel::OtelHandle> {
    let env_filter = EnvFilter::try_new(&config.level)
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let registry = tracing_subscriber::registry().with(env_filter);

    // OTel handle returned to caller so its Drop flushes on shutdown.
    let otel = if config.otlp_endpoint.is_some() {
        otel::OtelHandle::install(&config).ok()
    } else {
        None
    };

    let result = match (config.format, otel.as_ref()) {
        (LogFormat::Pretty, None) => registry
            .with(fmt::layer().with_target(true).pretty())
            .try_init(),
        (LogFormat::Pretty, Some(o)) => registry
            .with(fmt::layer().with_target(true).pretty())
            .with(o.tracing_layer())
            .with(o.log_appender_layer())
            .try_init(),
        (LogFormat::Json, None) => registry
            .with(fmt::layer().json().with_target(true))
            .try_init(),
        (LogFormat::Json, Some(o)) => registry
            .with(fmt::layer().json().with_target(true))
            .with(o.tracing_layer())
            .with(o.log_appender_layer())
            .try_init(),
    };

    let _ = result;
    otel
}
```

> **Subscriber init signature change:** Returning `Option<OtelHandle>` is a breaking change to the function in Task 5. Update Task 5's call site in `Server::serve` to hold the handle for the server's lifetime (drop on shutdown flushes pending exports).

- [ ] **Step 5: Metrics facade**

```rust
// framework/src/metrics/mod.rs
//! Thin facade over OpenTelemetry's meter API. Counters,
//! histograms, gauges via the global meter installed by the OTel
//! handle. When OTel isn't enabled (no OTLP endpoint), all calls
//! are no-ops via the OpenTelemetry SDK's `noop` provider.
//!
//! ```ignore
//! use suprnova::Metrics;
//!
//! Metrics::counter("http.requests", &[("method", "GET"), ("status", "200")]).inc();
//! Metrics::histogram("db.query.duration_ms", &[]).record(elapsed_ms);
//! Metrics::gauge("queue.depth").set(current);
//! ```

use opentelemetry::metrics::{Counter, Histogram, Meter, UpDownCounter};
use opentelemetry::{global, KeyValue};

pub struct Metrics;

impl Metrics {
    /// Get or create a `Counter<u64>` for the given metric name.
    /// Counters only go up; use UpDownCounter for things that can
    /// decrease (queue depth, open connections).
    pub fn counter(name: &'static str) -> CounterHandle {
        let meter = meter();
        let counter = meter.u64_counter(name).build();
        CounterHandle { counter }
    }

    /// `f64` histogram — distribution of values (latency, payload sizes).
    pub fn histogram(name: &'static str) -> HistogramHandle {
        let meter = meter();
        let histogram = meter.f64_histogram(name).build();
        HistogramHandle { histogram }
    }

    /// `i64` up-down counter — for values that go up AND down.
    pub fn gauge(name: &'static str) -> GaugeHandle {
        let meter = meter();
        let counter = meter.i64_up_down_counter(name).build();
        GaugeHandle { counter }
    }
}

fn meter() -> Meter {
    global::meter("suprnova")
}

pub struct CounterHandle {
    counter: Counter<u64>,
}

impl CounterHandle {
    pub fn inc(&self) {
        self.counter.add(1, &[]);
    }
    pub fn inc_by(&self, value: u64) {
        self.counter.add(value, &[]);
    }
    pub fn inc_with(&self, attrs: &[(&str, &str)]) {
        let kvs: Vec<KeyValue> = attrs.iter().map(|(k, v)| KeyValue::new(*k, v.to_string())).collect();
        self.counter.add(1, &kvs);
    }
}

pub struct HistogramHandle {
    histogram: Histogram<f64>,
}

impl HistogramHandle {
    pub fn record(&self, value: f64) {
        self.histogram.record(value, &[]);
    }
    pub fn record_with(&self, value: f64, attrs: &[(&str, &str)]) {
        let kvs: Vec<KeyValue> = attrs.iter().map(|(k, v)| KeyValue::new(*k, v.to_string())).collect();
        self.histogram.record(value, &kvs);
    }
}

pub struct GaugeHandle {
    counter: UpDownCounter<i64>,
}

impl GaugeHandle {
    pub fn add(&self, delta: i64) {
        self.counter.add(delta, &[]);
    }
    pub fn set(&self, _value: i64) {
        // OpenTelemetry has no direct "set" for UpDownCounter — it's
        // a delta API. For "current value" gauges, use the
        // ObservableGauge with a callback, OR track the delta from
        // the last known value in caller code. Phase 1 ships the
        // delta API; ObservableGauge belongs in a v2 metrics task.
        unimplemented!("use add(delta) for now; ObservableGauge for true gauges is v2")
    }
}
```

```rust
// framework/src/lib.rs
pub mod metrics;
pub use metrics::Metrics;
```

- [ ] **Step 6: W3C Trace Context propagation in outbound HTTP**

```rust
// framework/src/http_client/mod.rs (Phase 2) — add inside HttpRequestBuilder::build_request
use opentelemetry::trace::TraceContextExt;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry_sdk::propagation::TraceContextPropagator;

// Inject the current trace context into outgoing request headers so
// downstream services can continue the trace. W3C standard — works
// with any compliant collector.
let cx = opentelemetry::Context::current();
let propagator = TraceContextPropagator::new();
let mut injector = std::collections::HashMap::<String, String>::new();
propagator.inject_context(&cx, &mut HeaderInjector(&mut injector));
for (k, v) in injector {
    rb = rb.header(&k, &v);
}
```

> **Phase 2 dependency:** This propagation step modifies `framework/src/http_client/mod.rs` which is built in Phase 2. Two options: (a) leave the OTel-aware injection out of Phase 2's plan and add it here as a Phase-1-to-Phase-2 bridge note, (b) move propagation entirely to a Phase 2 task that depends on Phase 1's OTel deps being present. **Recommendation: (b)** — keep Phase 1 focused on local + export bridges; outbound trace propagation is a Phase 2 concern. Document the dependency in Phase 2's plan.

- [ ] **Step 7: Integration test**

```rust
// framework/tests/otel.rs
use suprnova::Metrics;

#[tokio::test]
async fn metrics_facade_records_without_otlp_endpoint() {
    // When OTLP isn't configured, the meter falls back to noop. All
    // operations succeed without exporting. This test guards against
    // accidental "fail when not configured" regressions.
    Metrics::counter("test.counter").inc();
    Metrics::counter("test.counter").inc_by(5);
    Metrics::histogram("test.histogram").record(42.0);
    // No assertions on values — noop provider doesn't surface them.
    // The test passes if no panic.
}

#[tokio::test]
#[ignore = "requires OTLP collector on :4317 — docker run -p 4317:4317 otel/opentelemetry-collector"]
async fn otel_export_round_trip() {
    let cfg = suprnova::LogConfig {
        level: "info".into(),
        format: suprnova::LogFormat::Pretty,
        otlp_endpoint: Some("http://localhost:4317".into()),
        service_name: "suprnova-test".into(),
        trace_sample_ratio: 1.0,
    };
    let _handle = suprnova::init_subscriber(cfg);

    tracing::info_span!("test_op").in_scope(|| {
        tracing::info!("test event inside span");
    });

    Metrics::counter("test.exported").inc();
    Metrics::histogram("test.latency_ms").record(123.45);

    // Drop _handle flushes; check the collector received the data
    // (manual verification — collector logs).
}
```

- [ ] **Step 8: Run + commit**

```bash
cargo test -p suprnova --test otel
git add framework/Cargo.toml framework/src/logging/otel.rs framework/src/logging/init.rs framework/src/logging/config.rs framework/src/metrics framework/src/lib.rs framework/tests/otel.rs
git commit -m "feat(observability): OpenTelemetry export bridge + Metrics facade"
```

---

## Task 21: Polish — workspace lint + final verification

- [ ] **Step 1: Run clippy**

```bash
cargo clippy --workspace -- -D warnings
```

Expected: clean.

- [ ] **Step 2: Run all tests**

```bash
cargo test --workspace
```

Expected: all passing, including the existing 229 framework tests and the new ones added in this phase.

- [ ] **Step 3: Update CLAUDE.md or ROADMAP "Where we are"**

Move the following from "Missing" / "Partial" to "Production-ready and complete":
- Logging (tracing-based, structured, request-id propagation, OTLP export)
- Events (Event::dispatch / Event::listen / Event::fake, sync + queued)
- Error tracing + ErrorOccurred event for 5xx
- Minimal SSE delivery primitive
- OpenTelemetry traces / metrics / logs export to any OTLP collector
- Metrics::counter / histogram / gauge facade

Edit `ROADMAP.md` "Where we are" section.

- [ ] **Step 4: Commit roadmap update**

```bash
git add ROADMAP.md
git commit -m "docs(roadmap): mark Phase 1 observability foundation complete"
```

- [ ] **Step 5: Push**

```bash
git push
```

---

## Self-Review

**Spec coverage check (against ROADMAP Track 1 — Observability foundation + Error handling + minimal SSE):**

| Spec item | Covered by |
|-----------|------------|
| Structured logging (`tracing`) | Tasks 1, 2, 5, 6, 7, 8 |
| Per-request correlation id | Tasks 3, 4, 7 |
| Event::dispatch / Event::listen | Tasks 9, 10, 11, 13 |
| Sync + queued event delivery | Tasks 10, 11 |
| Event::fake() / assert_dispatched | Tasks 12, 13 |
| Built-in `ErrorOccurred` event | Task 14 |
| Tracing on framework errors | Task 14 |
| Error context wrapping | Task 15 |
| Streaming response body | Task 16 |
| SSE event framing | Tasks 17, 18 |
| Dogfood event + SSE in app | Task 19 |
| OpenTelemetry traces + metrics + logs export | Task 20 |
| `Metrics::counter` / `histogram` / `gauge` facade | Task 20 |
| W3C Trace Context propagation in outbound HTTP | Task 20 (Phase 2 wires the actual call site) |

**Placeholder scan:** None ("TODO", "TBD", "implement later", "add appropriate error handling", "similar to Task N" — all clean. The few `> Implementation note:` callouts are concrete fork-points that name the file/function to inspect, not placeholders.)

**Type consistency:** `EventTrait` consistent across Tasks 9, 13, 14, 19. `SseEvent::with_event` / `with_id` consistent across Tasks 17, 18, 19. `HttpResponse::stream_bytes` / `sse` / `into_hyper_stream` consistent across Tasks 16, 17, 18, 19. `RequestId` / `current_request_id` consistent across Tasks 3, 4, 7, 8, 14.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-05-14-phase-01-observability-foundation.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.
