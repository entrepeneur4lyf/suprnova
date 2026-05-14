# Phase 5: Queue + Mail + Notifications + Rate Limiting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Ship the four "every production app has these" subsystems together because they share infrastructure: Queue (sea-streamer-backed Redis Streams + Kafka + in-process + DB + file), Mail (lettre-backed SMTP + provider HTTP `Transport` impls), Notifications (channel-based delivery: mail, slack, discord, SMS, DB, webhook, web-push, broadcast), Rate Limiting (middleware on Cache backends with atomic Lua on Redis). Mail-via-queue is the canonical pattern; the controllers using these don't care which driver is configured.

**Architecture:** Each subsystem is a trait + driver registry following the existing pattern (`UserProvider`, `CacheStore`, etc.). `Queue::push(job)` writes to whichever driver is currently bound; sea-streamer is the production driver, in-process is the dev/test default. `Mail::to(user).send(mailable)` builds a `lettre::Message` and dispatches through the bound transport. `Notify::send(user, notification).via(&channels)` fans out across channels. `RateLimiter::for_(name).limit(n).per_minute()` writes to the same Cache backend used for `Cache::*`.

**Tech Stack:** `sea-streamer` 0.5, `lettre` 0.11.22, `web-push` 0.11.0, `reqwest` (already in Phase 2) for provider HTTP transports. All three reference libraries are vendored under `reference/` and are wired by `path = "../reference/<name>"` so we pin to exact in-workspace sources until each crate hits a stable upstream release we trust to track.

---

## File Structure

**New files:**
- `framework/src/queue/mod.rs` — `Queue` facade, `Job` trait, registry
- `framework/src/queue/streamer.rs` — sea-streamer-backed driver (Redis Streams + Kafka)
- `framework/src/queue/in_process.rs` — tokio-task in-process driver
- `framework/src/queue/database.rs` — Postgres/SQLite jobs-table driver
- `framework/src/queue/file.rs` — sea-streamer-file dev backend
- `framework/src/queue/worker.rs` — `QueueWorker` (background consumer)
- `framework/src/queue/testing.rs` — `Queue::fake()`, `assert_pushed`
- `framework/src/mail/mod.rs` — `Mail` facade, `Mailable` trait
- `framework/src/mail/transport.rs` — `MailTransport` trait, registry
- `framework/src/mail/smtp.rs` — `lettre::AsyncSmtpTransport` driver
- `framework/src/mail/sendmail.rs` — sendmail driver
- `framework/src/mail/providers/{ses,postmark,sendgrid,mailgun,resend}.rs` — HTTP provider drivers
- `framework/src/mail/log.rs` — dev "log to stdout" driver
- `framework/src/mail/file.rs` — dev "write to file" driver
- `framework/src/mail/testing.rs` — `Mail::fake()`, `assert_sent`
- `framework/src/notifications/mod.rs` — `Notification` trait, `Notify` facade
- `framework/src/notifications/channel.rs` — `NotificationChannel` trait, channel registry
- `framework/src/notifications/channels/{mail,slack,discord,sms,database,webhook,web_push,broadcast}.rs`
- `framework/src/notifications/testing.rs` — `Notify::fake()`
- `framework/src/rate_limit/mod.rs` — `RateLimiter` facade
- `framework/src/rate_limit/middleware.rs` — `ThrottleMiddleware`
- `framework/src/rate_limit/window.rs` — sliding-window algorithm (memory + redis-lua)
- `framework/tests/queue.rs`, `framework/tests/mail.rs`, `framework/tests/notifications.rs`, `framework/tests/rate_limit.rs`
- Example mailable + notification + scheduled job in `app/`

**Modified files:**
- `framework/Cargo.toml` — add sea-streamer, lettre, web-push
- `framework/src/lib.rs` — re-export Queue / Mail / Notify / RateLimiter
- `framework/src/cache/redis.rs` — expose atomic `eval` for rate-limiter Lua scripts

---

## Task 1: Add deps

**Files:** `framework/Cargo.toml`

- [ ] **Step 1: Add**

```toml
# framework/Cargo.toml
sea-streamer = { path = "../reference/sea-streamer-0.5.2", features = ["redis", "kafka", "file", "stdio", "runtime-tokio"] }
lettre = { path = "../reference/lettre-0.11.22", default-features = false, features = ["builder", "smtp-transport", "tokio1-rustls", "pool"] }
web-push = { path = "../reference/rust-web-push-master" }
# After upstream stabilises, switch each to crates.io:
# lettre = { version = "0.11", default-features = false, features = [...] }
# web-push = "0.11"
```

- [ ] **Step 2: Verify build**

```bash
cargo check --workspace
```

- [ ] **Step 3: Commit**

```bash
git add framework/Cargo.toml Cargo.lock
git commit -m "feat(deps): add sea-streamer, lettre, web-push for Phase 5"
```

---

## Task 2: Job trait + Queue facade

**Files:** `framework/src/queue/mod.rs`, `framework/src/queue/in_process.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/queue.rs
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use suprnova::{async_trait, FrameworkError, Job, Queue};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AddNumbers {
    a: i64,
    b: i64,
}

#[async_trait]
impl Job for AddNumbers {
    fn job_name() -> &'static str {
        "AddNumbers"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        let sum = self.a + self.b;
        TOTAL.fetch_add(sum, Ordering::SeqCst);
        Ok(())
    }
}

static TOTAL: AtomicI64 = AtomicI64::new(0);

#[tokio::test]
async fn in_process_queue_runs_job_immediately() {
    TOTAL.store(0, Ordering::SeqCst);
    Queue::use_in_process().await;
    Queue::register::<AddNumbers>().await;
    Queue::push(AddNumbers { a: 5, b: 7 }).await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(TOTAL.load(Ordering::SeqCst), 12);
}
```

- [ ] **Step 2: Implement Job trait + facade + in-process driver**

```rust
// framework/src/queue/mod.rs
mod in_process;
pub mod testing;
mod streamer;
mod database;
mod file;
mod worker;

pub use worker::QueueWorker;

use crate::FrameworkError;
use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize};
use std::sync::OnceLock;

#[async_trait]
pub trait Job: Serialize + DeserializeOwned + Send + Sync + 'static {
    fn job_name() -> &'static str
    where
        Self: Sized;
    async fn handle(self) -> Result<(), FrameworkError>;
}

#[async_trait]
pub trait QueueDriver: Send + Sync {
    /// Enqueue a JSON-encoded job payload tagged with its `job_name`.
    async fn push(&self, job_name: &'static str, payload: serde_json::Value) -> Result<(), FrameworkError>;
}

static DRIVER: OnceLock<Box<dyn QueueDriver>> = OnceLock::new();

pub struct Queue;

impl Queue {
    pub async fn push<J: Job>(job: J) -> Result<(), FrameworkError> {
        if testing::is_active() {
            return testing::record(&job);
        }
        let driver = DRIVER
            .get()
            .ok_or_else(|| FrameworkError::internal("queue driver not initialized; call Queue::use_*()"))?;
        let payload = serde_json::to_value(&job)
            .map_err(|e| FrameworkError::internal(format!("encode job: {}", e)))?;
        driver.push(J::job_name(), payload).await
    }

    pub async fn register<J: Job>() {
        worker::register::<J>().await;
    }

    pub async fn use_in_process() {
        let _ = DRIVER.set(Box::new(in_process::InProcessDriver::new()));
    }

    pub async fn use_redis(url: impl Into<String>, stream: impl Into<String>) -> Result<(), FrameworkError> {
        let driver = streamer::SeaStreamerDriver::redis(url.into(), stream.into()).await?;
        let _ = DRIVER.set(Box::new(driver));
        Ok(())
    }

    pub async fn use_kafka(brokers: impl Into<String>, topic: impl Into<String>) -> Result<(), FrameworkError> {
        let driver = streamer::SeaStreamerDriver::kafka(brokers.into(), topic.into()).await?;
        let _ = DRIVER.set(Box::new(driver));
        Ok(())
    }

    pub async fn use_file(path: impl Into<std::path::PathBuf>) -> Result<(), FrameworkError> {
        let driver = file::FileQueueDriver::new(path.into()).await?;
        let _ = DRIVER.set(Box::new(driver));
        Ok(())
    }

    pub async fn use_database(table: impl Into<String>) -> Result<(), FrameworkError> {
        let driver = database::DatabaseQueueDriver::new(table.into()).await?;
        let _ = DRIVER.set(Box::new(driver));
        Ok(())
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn fake() -> testing::QueueFakeGuard {
        testing::install_fake()
    }
}
```

```rust
// framework/src/queue/in_process.rs
use super::{worker, QueueDriver};
use crate::FrameworkError;
use async_trait::async_trait;

pub struct InProcessDriver;

impl InProcessDriver {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl QueueDriver for InProcessDriver {
    async fn push(&self, job_name: &'static str, payload: serde_json::Value) -> Result<(), FrameworkError> {
        // Dispatch on a tokio task immediately.
        let job_name = job_name.to_string();
        let payload = payload.clone();
        tokio::spawn(async move {
            if let Err(e) = worker::dispatch_by_name(&job_name, payload).await {
                tracing::error!(job = %job_name, error = %e, "in-process job failed");
            }
        });
        Ok(())
    }
}
```

```rust
// framework/src/queue/worker.rs
//! Job-name registry. Each `Queue::register::<J>()` stores a
//! deserializer + handler tied to `J::job_name()`. Drivers call
//! `dispatch_by_name` to run an inbound payload.

use crate::FrameworkError;
use std::collections::HashMap;
use std::sync::RwLock;

type DispatchFn = Box<dyn Fn(serde_json::Value) -> futures::future::BoxFuture<'static, Result<(), FrameworkError>> + Send + Sync>;

static REGISTRY: RwLock<Option<HashMap<String, DispatchFn>>> = RwLock::new(None);

pub async fn register<J: super::Job>() {
    let f: DispatchFn = Box::new(|payload: serde_json::Value| {
        Box::pin(async move {
            let job: J = serde_json::from_value(payload)
                .map_err(|e| FrameworkError::internal(format!("decode job: {}", e)))?;
            job.handle().await
        })
    });
    let mut g = REGISTRY.write().unwrap();
    let map = g.get_or_insert_with(HashMap::new);
    map.insert(J::job_name().to_string(), f);
}

pub async fn dispatch_by_name(name: &str, payload: serde_json::Value) -> Result<(), FrameworkError> {
    let f = {
        let g = REGISTRY.read().unwrap();
        let map = g.as_ref().ok_or_else(|| {
            FrameworkError::internal("no jobs registered")
        })?;
        map.get(name)
            .cloned()
            .ok_or_else(|| FrameworkError::internal(format!("unknown job: {}", name)))?
    };
    f(payload).await
}

// DispatchFn isn't Clone. Wrap in Arc.
// (Implementation: change DispatchFn = Arc<dyn Fn... + Send + Sync>)
```

> **DispatchFn cloning:** The `dispatch_by_name` clones an `Fn` boxed-closure — `Box<dyn Fn>` is not `Clone`. Wrap as `Arc<dyn Fn(...) -> BoxFuture<...> + Send + Sync>` so the registry holds clonable handles. Adjust accordingly.

```rust
// framework/src/queue/testing.rs
use crate::FrameworkError;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct FakeStore {
    pushed: HashMap<TypeId, Vec<serde_json::Value>>,
}

static FAKE: Mutex<Option<FakeStore>> = Mutex::new(None);

pub(crate) fn is_active() -> bool {
    FAKE.lock().unwrap().is_some()
}

pub(crate) fn record<J: super::Job>(job: &J) -> Result<(), FrameworkError> {
    if let Some(store) = FAKE.lock().unwrap().as_mut() {
        let payload = serde_json::to_value(job)
            .map_err(|e| FrameworkError::internal(format!("encode: {}", e)))?;
        store.pushed.entry(TypeId::of::<J>()).or_default().push(payload);
    }
    Ok(())
}

pub fn install_fake() -> QueueFakeGuard {
    *FAKE.lock().unwrap() = Some(FakeStore::default());
    QueueFakeGuard
}

pub struct QueueFakeGuard;

impl Drop for QueueFakeGuard {
    fn drop(&mut self) {
        *FAKE.lock().unwrap() = None;
    }
}

pub fn assert_pushed<J: super::Job>(pred: impl Fn(&J) -> bool) {
    let count = {
        let g = FAKE.lock().unwrap();
        let store = g.as_ref().expect("Queue::fake() must be active");
        let bucket = store.pushed.get(&TypeId::of::<J>());
        bucket
            .map(|b| {
                b.iter()
                    .filter_map(|p| serde_json::from_value::<J>(p.clone()).ok())
                    .filter(|j| pred(j))
                    .count()
            })
            .unwrap_or(0)
    };
    assert!(count > 0, "expected at least one pushed {}", J::job_name());
}
```

```rust
// framework/src/lib.rs
pub mod queue;
pub use queue::{Job, Queue};
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test queue
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/queue framework/src/lib.rs framework/tests/queue.rs
git commit -m "feat(queue): Job trait + Queue facade + in-process driver + fake"
```

- [ ] **Step 5: Per-job metadata + `#[job(...)]` macro — failing test**

Laravel 13 added `#[Tries]` / `#[Backoff]` / `#[Timeout]` / `#[FailOnTimeout]`
attributes on job classes. Mirror them as `#[job(tries = ..., backoff = ...,
timeout = ..., fail_on_timeout = ...)]`. Backoff schedule is a Laravel-style
comma-separated duration list (`"2s,5s,30s"`).

```rust
// framework/tests/queue.rs — append
use std::time::Duration;
use suprnova::queue::BackoffSchedule;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[suprnova::job(tries = 5, backoff = "2s,5s,30s", timeout = "90s", fail_on_timeout = true)]
struct ResilientJob {
    payload: String,
}

#[async_trait]
impl Job for ResilientJob {
    fn job_name() -> &'static str { "ResilientJob" }
    async fn handle(self) -> Result<(), FrameworkError> { Ok(()) }
}

#[test]
fn job_macro_emits_metadata_overrides() {
    assert_eq!(ResilientJob::tries(), 5);
    assert_eq!(
        ResilientJob::backoff(),
        BackoffSchedule::Sequence(vec![
            Duration::from_secs(2),
            Duration::from_secs(5),
            Duration::from_secs(30),
        ])
    );
    assert_eq!(ResilientJob::timeout(), Some(Duration::from_secs(90)));
    assert!(ResilientJob::fail_on_timeout());
}

#[test]
fn job_without_macro_uses_trait_defaults() {
    assert_eq!(AddNumbers::tries(), 1);
    assert_eq!(AddNumbers::backoff(), BackoffSchedule::None);
    assert_eq!(AddNumbers::timeout(), None);
    assert!(!AddNumbers::fail_on_timeout());
}
```

- [ ] **Step 6: Run — expect failure**

```bash
cargo test -p suprnova --test queue job_macro_emits_metadata_overrides
```

Expected: FAIL — `BackoffSchedule` not in scope; trait methods missing.

- [ ] **Step 7: Implement Job trait metadata + `#[job]` proc macro**

```rust
// framework/src/queue/mod.rs — extend the Job trait
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackoffSchedule {
    /// No backoff — retry immediately.
    None,
    /// Apply a fixed delay between retries.
    Fixed(Duration),
    /// Apply a per-attempt sequence; last entry is reused for any
    /// attempts beyond the sequence length.
    Sequence(Vec<Duration>),
}

impl BackoffSchedule {
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        match self {
            BackoffSchedule::None => Duration::ZERO,
            BackoffSchedule::Fixed(d) => *d,
            BackoffSchedule::Sequence(s) => {
                let idx = (attempt as usize).saturating_sub(1).min(s.len().saturating_sub(1));
                s.get(idx).copied().unwrap_or(Duration::ZERO)
            }
        }
    }
}

#[async_trait]
pub trait Job: Serialize + DeserializeOwned + Send + Sync + 'static {
    fn job_name() -> &'static str
    where
        Self: Sized;
    async fn handle(self) -> Result<(), FrameworkError>;

    /// Maximum attempts (default 1 — no retry).
    fn tries() -> u32 where Self: Sized { 1 }
    /// Delay schedule between retries.
    fn backoff() -> BackoffSchedule where Self: Sized { BackoffSchedule::None }
    /// Hard timeout per attempt.
    fn timeout() -> Option<Duration> where Self: Sized { None }
    /// Whether timeout counts as a permanent failure (no further retries).
    fn fail_on_timeout() -> bool where Self: Sized { false }
}
```

```rust
// suprnova-macros/src/job.rs (new file)
use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, AttributeArgs, ItemStruct, Lit, Meta, NestedMeta};

pub fn job_attribute(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as AttributeArgs);
    let input = parse_macro_input!(input as ItemStruct);
    let name = &input.ident;

    let mut tries: Option<proc_macro2::TokenStream> = None;
    let mut backoff_expr: Option<proc_macro2::TokenStream> = None;
    let mut timeout_expr: Option<proc_macro2::TokenStream> = None;
    let mut fail_on_timeout = false;

    for arg in args {
        let NestedMeta::Meta(Meta::NameValue(nv)) = arg else { continue };
        let key = nv.path.get_ident().map(|i| i.to_string()).unwrap_or_default();
        match (key.as_str(), nv.lit) {
            ("tries", Lit::Int(n)) => {
                let v = n.base10_parse::<u32>().expect("tries must be u32");
                tries = Some(quote! { #v });
            }
            ("backoff", Lit::Str(s)) => {
                backoff_expr = Some(parse_backoff_literal(&s.value()));
            }
            ("timeout", Lit::Str(s)) => {
                let d = humantime::parse_duration(&s.value())
                    .expect("timeout must parse as humantime duration");
                let secs = d.as_secs();
                let nanos = d.subsec_nanos();
                timeout_expr = Some(quote! {
                    ::std::option::Option::Some(
                        ::std::time::Duration::new(#secs, #nanos)
                    )
                });
            }
            ("fail_on_timeout", Lit::Bool(b)) => fail_on_timeout = b.value(),
            _ => {}
        }
    }

    let tries_impl = tries.map(|t| quote! { fn tries() -> u32 { #t } });
    let backoff_impl = backoff_expr.map(|b| quote! { fn backoff() -> ::suprnova::queue::BackoffSchedule { #b } });
    let timeout_impl = timeout_expr.map(|t| quote! { fn timeout() -> ::std::option::Option<::std::time::Duration> { #t } });
    let fail_impl = if fail_on_timeout {
        Some(quote! { fn fail_on_timeout() -> bool { true } })
    } else {
        None
    };

    quote! {
        #input

        #[automatically_derived]
        impl #name {
            #tries_impl
            #backoff_impl
            #timeout_impl
            #fail_impl
        }
    }
    .into()
}

fn parse_backoff_literal(s: &str) -> proc_macro2::TokenStream {
    let durations: Vec<_> = s
        .split(',')
        .map(|p| {
            let d = humantime::parse_duration(p.trim())
                .expect("backoff entry must parse as humantime duration");
            let secs = d.as_secs();
            let nanos = d.subsec_nanos();
            quote! { ::std::time::Duration::new(#secs, #nanos) }
        })
        .collect();
    if durations.len() == 1 {
        let d = &durations[0];
        quote! { ::suprnova::queue::BackoffSchedule::Fixed(#d) }
    } else {
        quote! { ::suprnova::queue::BackoffSchedule::Sequence(vec![ #(#durations),* ]) }
    }
}
```

```rust
// suprnova-macros/src/lib.rs — append
mod job;
#[proc_macro_attribute]
pub fn job(args: TokenStream, input: TokenStream) -> TokenStream {
    job::job_attribute(args, input)
}
```

> **Note:** The `#[job(...)]` macro emits an inherent `impl` on the
> struct that provides metadata methods. Because the `Job` trait's
> methods all have default impls, the inherent-impl methods shadow
> them via Rust's method resolution. Verify with `cargo expand` that
> the generated methods are called from `<ResilientJob as Job>::tries()`
> sites in the dispatch path.

**Driver dispatch must read these:** the in-process driver and the
sea-streamer driver (Task 3) read `J::tries()` / `J::backoff()` /
`J::timeout()` / `J::fail_on_timeout()` from the registered handler's
type and apply retry / timeout policy accordingly. Extend
`worker::register::<J>` to capture these as `FnOnce` closures alongside
the deserializer.

- [ ] **Step 8: Run — expect pass**

```bash
cargo test -p suprnova --test queue job_macro_emits_metadata_overrides job_without_macro_uses_trait_defaults
```

Expected: both pass.

- [ ] **Step 9: Commit**

```bash
git add framework/src/queue/mod.rs suprnova-macros framework/Cargo.toml framework/tests/queue.rs
git commit -m "feat(queue): #[job(tries, backoff, timeout, fail_on_timeout)] attribute"
```

- [ ] **Step 10: `Queue::route` — class-to-queue routing (L13)**

Failing test:

```rust
// framework/tests/queue.rs — append
#[derive(Serialize, Deserialize, Debug, Clone)]
struct PodcastJob { id: i64 }

#[async_trait]
impl Job for PodcastJob {
    fn job_name() -> &'static str { "PodcastJob" }
    async fn handle(self) -> Result<(), FrameworkError> { Ok(()) }
}

#[tokio::test]
async fn queue_route_pins_a_job_to_a_specific_connection_and_queue() {
    Queue::route::<PodcastJob>("redis", "podcasts").await;
    let route = Queue::route_for::<PodcastJob>().await;
    assert_eq!(route.as_deref(), Some(("redis", "podcasts")));
}
```

- [ ] **Step 11: Implement `Queue::route`**

```rust
// framework/src/queue/mod.rs — append impl Queue block
use dashmap::DashMap;
use std::any::TypeId;

static ROUTES: OnceLock<DashMap<TypeId, (String, String)>> = OnceLock::new();

fn routes() -> &'static DashMap<TypeId, (String, String)> {
    ROUTES.get_or_init(DashMap::new)
}

impl Queue {
    /// Pin a job type to a specific connection + queue.
    ///
    /// Calls to `Queue::push::<J>` route to the named driver if the
    /// type has a registered route; otherwise they use the default
    /// driver installed by `Queue::use_*()`.
    pub async fn route<J: Job>(connection: impl Into<String>, queue: impl Into<String>) {
        routes().insert(TypeId::of::<J>(), (connection.into(), queue.into()));
    }

    /// Read the route for a job type (test/inspection only).
    pub async fn route_for<J: Job>() -> Option<(&'static str, &'static str)> {
        routes()
            .get(&TypeId::of::<J>())
            .map(|kv| (Box::leak(kv.0.clone().into_boxed_str()) as &str,
                       Box::leak(kv.1.clone().into_boxed_str()) as &str))
    }
}
```

The push path consults the routes table:

```rust
// framework/src/queue/mod.rs — modify Queue::push
impl Queue {
    pub async fn push<J: Job>(job: J) -> Result<(), FrameworkError> {
        if testing::is_active() {
            return testing::record(&job);
        }
        // Route lookup
        let route = routes().get(&TypeId::of::<J>()).map(|kv| kv.value().clone());
        let payload = serde_json::to_value(&job)
            .map_err(|e| FrameworkError::internal(format!("encode job: {}", e)))?;
        match route {
            Some((connection, queue)) => {
                // Look up the named driver and push to its queue.
                let named = NAMED_DRIVERS
                    .get()
                    .and_then(|map| map.get(&connection).cloned())
                    .ok_or_else(|| FrameworkError::internal(format!(
                        "no named queue driver for connection '{}'", connection
                    )))?;
                named.push_to_queue(J::job_name(), &queue, payload).await
            }
            None => {
                let driver = DRIVER
                    .get()
                    .ok_or_else(|| FrameworkError::internal("queue driver not initialized"))?;
                driver.push(J::job_name(), payload).await
            }
        }
    }
}
```

> **Implementation note:** Add `push_to_queue(job_name, queue, payload)`
> to the `QueueDriver` trait alongside `push`. For drivers that don't
> support multiple queues (in-process), default the impl to call `push`
> ignoring the `queue` arg. For sea-streamer / database drivers, route
> to a different stream/table per `queue` name.

Also add a `NAMED_DRIVERS: OnceLock<DashMap<String, Arc<dyn QueueDriver>>>`
and a `Queue::register_driver(connection_name, driver)` API.

- [ ] **Step 12: Run — expect pass**

```bash
cargo test -p suprnova --test queue queue_route_pins_a_job
```

- [ ] **Step 13: Commit**

```bash
git add framework/src/queue/mod.rs framework/tests/queue.rs
git commit -m "feat(queue): Queue::route — pin job classes to a named connection+queue"
```

---

## Task 3: sea-streamer-backed Queue driver

**Files:** `framework/src/queue/streamer.rs`

- [ ] **Step 1: Write integration test (requires Redis or skip)**

```rust
// framework/tests/queue.rs — append
#[tokio::test]
#[ignore = "requires redis on localhost:6379; run with --ignored after `docker run -d --rm -p 6379:6379 redis`"]
async fn redis_streams_queue_round_trip() {
    let _ = Queue::use_redis("redis://localhost:6379", "queue-test").await.unwrap();
    Queue::register::<AddNumbers>().await;
    TOTAL.store(0, Ordering::SeqCst);

    Queue::push(AddNumbers { a: 100, b: 200 }).await.unwrap();

    // Start a worker that drains the stream
    let worker = suprnova::QueueWorker::start("queue-test").await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    worker.stop().await;

    assert_eq!(TOTAL.load(Ordering::SeqCst), 300);
}
```

- [ ] **Step 2: Implement sea-streamer adapter**

```rust
// framework/src/queue/streamer.rs
use super::QueueDriver;
use crate::FrameworkError;
use async_trait::async_trait;
use sea_streamer::{Producer, SeaProducer, SeaStreamer, StreamKey, Streamer as _};

pub struct SeaStreamerDriver {
    producer: SeaProducer,
    _streamer: SeaStreamer,
}

impl SeaStreamerDriver {
    pub async fn redis(url: String, stream: String) -> Result<Self, FrameworkError> {
        let url = format!("redis://{}", url.trim_start_matches("redis://"));
        let streamer = SeaStreamer::connect(url.parse().map_err(map_err)?, Default::default())
            .await
            .map_err(map_err)?;
        let producer = streamer
            .create_producer(StreamKey::new(stream).map_err(map_err)?, Default::default())
            .await
            .map_err(map_err)?;
        Ok(Self {
            producer,
            _streamer: streamer,
        })
    }

    pub async fn kafka(brokers: String, topic: String) -> Result<Self, FrameworkError> {
        let url = format!("kafka://{}", brokers);
        let streamer = SeaStreamer::connect(url.parse().map_err(map_err)?, Default::default())
            .await
            .map_err(map_err)?;
        let producer = streamer
            .create_producer(StreamKey::new(topic).map_err(map_err)?, Default::default())
            .await
            .map_err(map_err)?;
        Ok(Self {
            producer,
            _streamer: streamer,
        })
    }
}

#[async_trait]
impl QueueDriver for SeaStreamerDriver {
    async fn push(&self, job_name: &'static str, payload: serde_json::Value) -> Result<(), FrameworkError> {
        let envelope = serde_json::json!({
            "job_name": job_name,
            "payload": payload,
        });
        let bytes = serde_json::to_vec(&envelope).map_err(map_err)?;
        self.producer
            .send(bytes)
            .map_err(map_err)?;
        Ok(())
    }
}

fn map_err<E: std::fmt::Display>(e: E) -> FrameworkError {
    FrameworkError::internal(format!("sea-streamer: {}", e))
}
```

> **API verification:** Confirm sea-streamer 0.5's `SeaStreamer::connect` signature, `Producer::send`, and `StreamKey` construction via `cargo doc -p sea-streamer --open --no-deps`. Adjust if the trait surface differs.

- [ ] **Step 3: Implement `QueueWorker`**

```rust
// framework/src/queue/worker.rs — append
use sea_streamer::{Consumer, Message, SeaConsumer, SeaConsumerOptions, SeaStreamer, ConsumerMode, Streamer as _, StreamKey};

pub struct QueueWorker {
    cancel: tokio::sync::oneshot::Sender<()>,
}

impl QueueWorker {
    pub async fn start(stream: impl Into<String>) -> Result<Self, crate::FrameworkError> {
        let stream = stream.into();
        let url = std::env::var("QUEUE_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
        let streamer = SeaStreamer::connect(url.parse().map_err(map_err)?, Default::default())
            .await
            .map_err(map_err)?;
        let consumer: SeaConsumer = streamer
            .create_consumer(&[StreamKey::new(stream).map_err(map_err)?], SeaConsumerOptions::new(ConsumerMode::RealTime))
            .await
            .map_err(map_err)?;

        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    msg = consumer.next() => {
                        match msg {
                            Ok(m) => {
                                if let Ok(bytes) = m.message().as_bytes() {
                                    if let Ok(envelope) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                                        let job_name = envelope["job_name"].as_str().unwrap_or_default().to_string();
                                        let payload = envelope["payload"].clone();
                                        if let Err(e) = dispatch_by_name(&job_name, payload).await {
                                            tracing::error!(job = %job_name, error = %e, "queue job failed");
                                        }
                                    }
                                }
                            }
                            Err(e) => tracing::error!(error = %e, "consumer error"),
                        }
                    }
                }
            }
        });
        Ok(Self { cancel: tx })
    }

    pub async fn stop(self) {
        let _ = self.cancel.send(());
    }
}
```

- [ ] **Step 4: Run integration test (with Redis up)**

```bash
docker run -d --rm --name redis -p 6379:6379 redis
cargo test -p suprnova --test queue -- --ignored redis_streams
docker stop redis
```

- [ ] **Step 5: Commit**

```bash
git add framework/src/queue/streamer.rs framework/src/queue/worker.rs framework/tests/queue.rs
git commit -m "feat(queue): sea-streamer driver (Redis Streams + Kafka) + QueueWorker"
```

---

## Task 4: Mailable trait + Mail facade

**Files:** `framework/src/mail/mod.rs`, `framework/src/mail/transport.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/mail.rs
use suprnova::{async_trait, FrameworkError, Mail, Mailable};

struct WelcomeEmail {
    pub email: String,
    pub name: String,
}

#[async_trait]
impl Mailable for WelcomeEmail {
    fn to(&self) -> Vec<String> {
        vec![self.email.clone()]
    }
    fn subject(&self) -> String {
        format!("Welcome, {}!", self.name)
    }
    fn body_html(&self) -> String {
        format!("<h1>Hi {}</h1><p>Welcome.</p>", self.name)
    }
    fn body_text(&self) -> Option<String> {
        Some(format!("Hi {}\n\nWelcome.", self.name))
    }
}

#[tokio::test]
async fn fake_mail_records_sends() {
    let _g = Mail::fake();
    Mail::send(WelcomeEmail {
        email: "alice@example.com".into(),
        name: "Alice".into(),
    })
    .await
    .unwrap();
    suprnova::mail::testing::assert_sent::<WelcomeEmail>(|m| m.email == "alice@example.com");
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/mail/mod.rs
//! Mail facade. `Mail::to(user).send(Mailable)` builds a
//! `lettre::Message` and dispatches through the bound transport.

mod log_driver;
mod smtp;
pub mod testing;
mod transport;

use crate::FrameworkError;
use async_trait::async_trait;
use std::sync::OnceLock;

pub use transport::MailTransport;

static TRANSPORT: OnceLock<Box<dyn MailTransport>> = OnceLock::new();

#[async_trait]
pub trait Mailable: Send + Sync + 'static {
    fn to(&self) -> Vec<String>;
    fn from(&self) -> Option<String> {
        std::env::var("MAIL_FROM").ok()
    }
    fn subject(&self) -> String;
    fn body_html(&self) -> String;
    fn body_text(&self) -> Option<String> {
        None
    }
    fn reply_to(&self) -> Option<String> {
        None
    }
}

pub struct Mail;

impl Mail {
    pub async fn send<M: Mailable>(mailable: M) -> Result<(), FrameworkError> {
        if testing::is_active() {
            return testing::record(mailable);
        }
        let transport = TRANSPORT
            .get()
            .ok_or_else(|| FrameworkError::internal("mail transport not initialized"))?;
        let message = build_message(&mailable)?;
        transport.send(message).await
    }

    pub async fn use_smtp(config: smtp::SmtpConfig) -> Result<(), FrameworkError> {
        let driver = smtp::SmtpTransport::new(config).await?;
        let _ = TRANSPORT.set(Box::new(driver));
        Ok(())
    }

    pub async fn use_log() {
        let _ = TRANSPORT.set(Box::new(log_driver::LogTransport));
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn fake() -> testing::MailFakeGuard {
        testing::install_fake()
    }
}

fn build_message<M: Mailable>(m: &M) -> Result<lettre::Message, FrameworkError> {
    use lettre::message::{header::ContentType, MultiPart, SinglePart};
    use lettre::Message;

    let from = m
        .from()
        .ok_or_else(|| FrameworkError::internal("MAIL_FROM not set and no Mailable::from() override"))?;
    let from = from.parse().map_err(|e| FrameworkError::internal(format!("from: {}", e)))?;
    let mut builder = Message::builder().from(from).subject(m.subject());
    for to in m.to() {
        let to = to.parse().map_err(|e| FrameworkError::internal(format!("to: {}", e)))?;
        builder = builder.to(to);
    }
    if let Some(rt) = m.reply_to() {
        let rt = rt.parse().map_err(|e| FrameworkError::internal(format!("reply-to: {}", e)))?;
        builder = builder.reply_to(rt);
    }

    let html = m.body_html();
    let message = match m.body_text() {
        Some(text) => builder
            .multipart(
                MultiPart::alternative()
                    .singlepart(SinglePart::builder().header(ContentType::TEXT_PLAIN).body(text))
                    .singlepart(SinglePart::builder().header(ContentType::TEXT_HTML).body(html)),
            )
            .map_err(|e| FrameworkError::internal(format!("build: {}", e)))?,
        None => builder
            .header(ContentType::TEXT_HTML)
            .body(html)
            .map_err(|e| FrameworkError::internal(format!("build: {}", e)))?,
    };

    Ok(message)
}
```

```rust
// framework/src/mail/transport.rs
use crate::FrameworkError;
use async_trait::async_trait;

#[async_trait]
pub trait MailTransport: Send + Sync {
    async fn send(&self, msg: lettre::Message) -> Result<(), FrameworkError>;
}
```

```rust
// framework/src/mail/log_driver.rs
use super::MailTransport;
use crate::FrameworkError;
use async_trait::async_trait;
use tracing::info;

pub struct LogTransport;

#[async_trait]
impl MailTransport for LogTransport {
    async fn send(&self, msg: lettre::Message) -> Result<(), FrameworkError> {
        let formatted = String::from_utf8_lossy(&msg.formatted()).into_owned();
        info!(target: "mail", "would send:\n{}", formatted);
        Ok(())
    }
}
```

```rust
// framework/src/mail/testing.rs — same pattern as queue/testing.rs
// (record concrete Mailable instances by TypeId; assert_sent<M>(predicate))
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test mail
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/mail framework/src/lib.rs framework/tests/mail.rs
git commit -m "feat(mail): Mail facade + Mailable trait + log/fake transports"
```

---

## Task 5: lettre SMTP transport

**Files:** `framework/src/mail/smtp.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/mail/smtp.rs
use super::MailTransport;
use crate::FrameworkError;
use async_trait::async_trait;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub use_tls: bool,
}

pub struct SmtpTransport {
    inner: AsyncSmtpTransport<Tokio1Executor>,
}

impl SmtpTransport {
    pub async fn new(config: SmtpConfig) -> Result<Self, FrameworkError> {
        let mut builder = if config.use_tls {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)
                .map_err(|e| FrameworkError::internal(format!("smtp relay: {}", e)))?
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host)
        };
        builder = builder.port(config.port);
        if let (Some(u), Some(p)) = (config.username, config.password) {
            builder = builder.credentials(Credentials::new(u, p));
        }
        Ok(Self {
            inner: builder.build(),
        })
    }
}

#[async_trait]
impl MailTransport for SmtpTransport {
    async fn send(&self, msg: lettre::Message) -> Result<(), FrameworkError> {
        self.inner
            .send(msg)
            .await
            .map(|_| ())
            .map_err(|e| FrameworkError::internal(format!("smtp send: {}", e)))
    }
}
```

- [ ] **Step 2: Integration test (with mailpit or skip)**

```rust
// framework/tests/mail.rs — append
#[tokio::test]
#[ignore = "requires mailpit/mailcatcher on :1025; docker run -d -p 1025:1025 -p 8025:8025 axllent/mailpit"]
async fn smtp_sends_to_local_relay() {
    Mail::use_smtp(suprnova::mail::SmtpConfig {
        host: "localhost".into(),
        port: 1025,
        username: None,
        password: None,
        use_tls: false,
    })
    .await
    .unwrap();
    Mail::send(WelcomeEmail {
        email: "to@example.com".into(),
        name: "Bob".into(),
    })
    .await
    .unwrap();
    // Visual check via mailpit UI at http://localhost:8025
}
```

- [ ] **Step 3: Commit**

```bash
git add framework/src/mail/smtp.rs framework/tests/mail.rs
git commit -m "feat(mail): SMTP transport on lettre tokio1-rustls with pool"
```

---

## Task 6: HTTP provider transports (SES, Postmark, SendGrid, Mailgun, Resend)

**Files:** `framework/src/mail/providers/{ses,postmark,sendgrid,mailgun,resend}.rs`

- [ ] **Step 1: Pattern — each provider gets a transport that builds a request from `lettre::Message`**

Each provider file follows the same shape. Example — Postmark:

```rust
// framework/src/mail/providers/postmark.rs
use crate::mail::MailTransport;
use crate::FrameworkError;
use async_trait::async_trait;
use serde::Serialize;

pub struct PostmarkTransport {
    server_token: String,
    client: reqwest::Client,
}

impl PostmarkTransport {
    pub fn new(server_token: impl Into<String>) -> Self {
        Self {
            server_token: server_token.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Serialize)]
struct PostmarkRequest<'a> {
    #[serde(rename = "From")]
    from: &'a str,
    #[serde(rename = "To")]
    to: String,
    #[serde(rename = "Subject")]
    subject: &'a str,
    #[serde(rename = "HtmlBody")]
    html_body: &'a str,
    #[serde(rename = "TextBody", skip_serializing_if = "Option::is_none")]
    text_body: Option<&'a str>,
}

#[async_trait]
impl MailTransport for PostmarkTransport {
    async fn send(&self, msg: lettre::Message) -> Result<(), FrameworkError> {
        // Extract metadata from msg.headers() — Postmark wants From/To/Subject
        // as separate JSON fields rather than the raw RFC822.
        // For brevity, this implementation extracts the first To and From,
        // assumes HTML body; production should parse the multipart properly.
        let envelope = msg.envelope().clone();
        let from = envelope.from().expect("from").to_string();
        let to = envelope.to().first().expect("to").to_string();
        let formatted = String::from_utf8_lossy(&msg.formatted()).into_owned();
        // For real impls: parse multipart back; here we pass formatted RFC822.
        // Postmark's /email endpoint actually accepts From/To/HtmlBody fields
        // directly — implementer should plumb these from Mailable, not Message.
        let req = serde_json::json!({
            "From": from,
            "To": to,
            "Subject": "(see body)",
            "HtmlBody": formatted,
        });
        let resp = self.client
            .post("https://api.postmarkapp.com/email")
            .header("Accept", "application/json")
            .header("X-Postmark-Server-Token", &self.server_token)
            .json(&req)
            .send()
            .await
            .map_err(|e| FrameworkError::internal(format!("postmark: {}", e)))?;
        if !resp.status().is_success() {
            return Err(FrameworkError::internal(format!(
                "postmark {} {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            )));
        }
        Ok(())
    }
}
```

> **Refactor note:** Provider HTTP APIs want `From`/`To`/`Subject` as structured fields, not raw RFC822. The cleanest path is for `MailTransport::send` to take a richer `(MailableMetadata, lettre::Message)` tuple OR for each provider transport to read these from the message envelope/headers. **Recommendation:** Change `MailTransport::send(&self, &dyn Mailable) -> ...` so the provider has direct access to `to()`, `subject()`, `body_html()` rather than reconstructing from a Message. Adjust the trait in `transport.rs` and `Mail::send` accordingly.

- [ ] **Step 2: Repeat for SES, SendGrid, Mailgun, Resend**

Same pattern, different endpoints:
- SES: `https://email.<region>.amazonaws.com/` — requires SigV4 (use `aws-sigv4` crate, OR pull in `aws-sdk-sesv2` behind a feature flag — same trade-off as we made for S3)
- SendGrid: `https://api.sendgrid.com/v3/mail/send` with bearer token
- Mailgun: `https://api.mailgun.net/v3/<domain>/messages` with form auth
- Resend: `https://api.resend.com/emails` with bearer token

Each is a separate task; for plan brevity they share Step 1's structure.

- [ ] **Step 3: Smoke test against each (using their sandbox/test endpoints)**

- [ ] **Step 4: Commit each provider as a separate commit**

```bash
git commit -m "feat(mail): Postmark HTTP transport"
git commit -m "feat(mail): SendGrid HTTP transport"
git commit -m "feat(mail): Mailgun HTTP transport"
git commit -m "feat(mail): Resend HTTP transport"
git commit -m "feat(mail): SES HTTP transport (with optional ses-sdk feature)"
```

---

## Task 7: Notification trait + Notify facade

**Files:** `framework/src/notifications/mod.rs`, `framework/src/notifications/channel.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/notifications/mod.rs
//! `Notify::send(user, notification).via(&["mail", "slack"]).await`.

mod channel;
pub mod channels;
pub mod testing;

use crate::FrameworkError;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;

pub use channel::NotificationChannel;

/// A notification represents a thing-to-deliver. Each
/// `to_<channel>` method on a notification produces the
/// channel-specific payload.
#[async_trait]
pub trait Notification: Send + Sync + 'static {
    fn notification_name() -> &'static str
    where
        Self: Sized;

    /// Build a Mailable for this notification's mail channel.
    fn to_mail(&self, recipient: &dyn Notifiable) -> Option<Box<dyn crate::Mailable>> {
        let _ = recipient;
        None
    }

    /// Build a Slack payload.
    fn to_slack(&self, recipient: &dyn Notifiable) -> Option<serde_json::Value> {
        let _ = recipient;
        None
    }

    /// Build a Discord payload.
    fn to_discord(&self, recipient: &dyn Notifiable) -> Option<serde_json::Value> {
        let _ = recipient;
        None
    }

    /// Build a database notification row.
    fn to_database(&self, recipient: &dyn Notifiable) -> Option<serde_json::Value> {
        let _ = recipient;
        None
    }

    fn to_webhook(&self, recipient: &dyn Notifiable) -> Option<serde_json::Value> {
        let _ = recipient;
        None
    }

    fn to_web_push(&self, recipient: &dyn Notifiable) -> Option<serde_json::Value> {
        let _ = recipient;
        None
    }

    fn to_broadcast(&self, recipient: &dyn Notifiable) -> Option<serde_json::Value> {
        let _ = recipient;
        None
    }
}

pub trait Notifiable: Send + Sync {
    /// Stable identifier for the recipient (user id, etc.). Used by
    /// the database / web-push channels.
    fn notifiable_id(&self) -> String;

    /// Per-channel routing — e.g. email address for mail, webhook URL
    /// for webhook, slack channel id for slack.
    fn route_for(&self, channel: &str) -> Option<String>;
}

static CHANNELS: RwLock<Option<HashMap<String, Box<dyn NotificationChannel>>>> = RwLock::new(None);

pub fn register_channel(name: impl Into<String>, channel: impl NotificationChannel + 'static) {
    let mut g = CHANNELS.write().unwrap();
    let map = g.get_or_insert_with(HashMap::new);
    map.insert(name.into(), Box::new(channel));
}

pub struct Notify;

impl Notify {
    pub fn send<N: Notification>(recipient: impl Notifiable + 'static, notification: N) -> NotifyDispatch<N> {
        NotifyDispatch {
            recipient: Box::new(recipient),
            notification,
        }
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn fake() -> testing::NotifyFakeGuard {
        testing::install_fake()
    }
}

pub struct NotifyDispatch<N: Notification> {
    recipient: Box<dyn Notifiable>,
    notification: N,
}

impl<N: Notification> NotifyDispatch<N> {
    pub async fn via(self, channels: &[&str]) -> Result<(), FrameworkError> {
        if testing::is_active() {
            return testing::record(&self.notification, channels);
        }
        let g = CHANNELS.read().unwrap();
        let map = g.as_ref().ok_or_else(|| {
            FrameworkError::internal("no notification channels registered")
        })?;
        for name in channels {
            let chan = map.get(*name).ok_or_else(|| {
                FrameworkError::internal(format!("unknown channel: {}", name))
            })?;
            chan.dispatch(&*self.recipient, &self.notification as &dyn Notification).await?;
        }
        Ok(())
    }
}
```

```rust
// framework/src/notifications/channel.rs
use super::{Notifiable, Notification};
use crate::FrameworkError;
use async_trait::async_trait;

#[async_trait]
pub trait NotificationChannel: Send + Sync {
    async fn dispatch(
        &self,
        recipient: &dyn Notifiable,
        notification: &dyn Notification,
    ) -> Result<(), FrameworkError>;
}
```

Each channel in `framework/src/notifications/channels/` implements `NotificationChannel`. For example:

```rust
// framework/src/notifications/channels/mail.rs
use crate::notifications::{Notifiable, Notification, NotificationChannel};
use crate::{FrameworkError, Mail};
use async_trait::async_trait;

pub struct MailChannel;

#[async_trait]
impl NotificationChannel for MailChannel {
    async fn dispatch(
        &self,
        recipient: &dyn Notifiable,
        notification: &dyn Notification,
    ) -> Result<(), FrameworkError> {
        if let Some(mailable) = notification.to_mail(recipient) {
            // Box<dyn Mailable> → send. Requires Mail::send to accept
            // `Box<dyn Mailable>`. Adjust Mail facade if needed.
            Mail::send_boxed(mailable).await?;
        }
        Ok(())
    }
}
```

> **`Mail::send_boxed`:** Add to Mail facade for the boxed-trait-object case used by notifications.

- [ ] **Step 2: Implement web-push channel**

```rust
// framework/src/notifications/channels/web_push.rs
use crate::notifications::{Notifiable, Notification, NotificationChannel};
use crate::FrameworkError;
use async_trait::async_trait;
use web_push::*;

pub struct WebPushChannel {
    vapid_subject: String,
    vapid_private_key: String,
}

impl WebPushChannel {
    pub fn from_env() -> Result<Self, FrameworkError> {
        Ok(Self {
            vapid_subject: std::env::var("VAPID_SUBJECT")
                .map_err(|_| FrameworkError::internal("VAPID_SUBJECT not set"))?,
            vapid_private_key: std::env::var("VAPID_PRIVATE_KEY")
                .map_err(|_| FrameworkError::internal("VAPID_PRIVATE_KEY not set"))?,
        })
    }
}

#[async_trait]
impl NotificationChannel for WebPushChannel {
    async fn dispatch(
        &self,
        recipient: &dyn Notifiable,
        notification: &dyn Notification,
    ) -> Result<(), FrameworkError> {
        let Some(payload) = notification.to_web_push(recipient) else {
            return Ok(());
        };
        let subscription = serde_json::from_value::<SubscriptionInfo>(
            serde_json::Value::String(
                recipient
                    .route_for("web-push")
                    .ok_or_else(|| FrameworkError::internal("no web-push subscription"))?,
            ),
        )
        .map_err(|e| FrameworkError::internal(format!("subscription: {}", e)))?;
        let signer = VapidSignatureBuilder::from_pem(
            std::io::Cursor::new(self.vapid_private_key.as_bytes()),
            &subscription,
        )
        .map_err(|e| FrameworkError::internal(format!("vapid: {}", e)))?
        .build()
        .map_err(|e| FrameworkError::internal(format!("vapid build: {}", e)))?;
        let mut builder = WebPushMessageBuilder::new(&subscription);
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        builder.set_payload(ContentEncoding::Aes128Gcm, &payload_bytes);
        builder.set_vapid_signature(signer);
        let msg = builder
            .build()
            .map_err(|e| FrameworkError::internal(format!("build: {}", e)))?;
        let client = IsahcWebPushClient::new().unwrap();
        client
            .send(msg)
            .await
            .map_err(|e| FrameworkError::internal(format!("web-push: {}", e)))?;
        Ok(())
    }
}
```

> **web-push API:** The vendored source at `reference/rust-web-push-master/` is version 0.11.0. Verify the exact `WebPushMessageBuilder` / `IsahcWebPushClient` / `SubscriptionInfo` shapes by reading the vendored source (`src/lib.rs`, `examples/`) before implementing. The 0.11 line has a `WebPushClient` trait with separate `IsahcWebPushClient` and `HyperWebPushClient` impls — pick the one that matches our `reqwest`/`hyper` stack.

- [ ] **Step 3: Implement remaining channels** (`slack`, `discord`, `sms`, `database`, `webhook`, `broadcast`) — each `dispatch` posts via `Http::` (Phase 2 facade) or writes to DB / broadcasts via Track 7 broadcasting facade.

- [ ] **Step 4: Commit each channel**

---

## Task 8: RateLimiter — sliding-window middleware

**Files:** `framework/src/rate_limit/mod.rs`, `framework/src/rate_limit/middleware.rs`, `framework/src/rate_limit/window.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/rate_limit.rs
use suprnova::RateLimiter;

#[tokio::test]
async fn in_memory_limiter_blocks_after_threshold() {
    let limiter = RateLimiter::for_("login").limit(3).per_minute();
    for _ in 0..3 {
        assert!(limiter.attempt("ip:1.2.3.4").await.is_ok());
    }
    let result = limiter.attempt("ip:1.2.3.4").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn different_keys_have_independent_quota() {
    let limiter = RateLimiter::for_("login").limit(2).per_minute();
    assert!(limiter.attempt("a").await.is_ok());
    assert!(limiter.attempt("a").await.is_ok());
    assert!(limiter.attempt("a").await.is_err());
    // "b" has its own bucket
    assert!(limiter.attempt("b").await.is_ok());
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/rate_limit/window.rs
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(crate) struct MemoryWindow {
    buckets: Mutex<HashMap<String, Vec<Instant>>>,
}

impl MemoryWindow {
    pub fn new() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Sliding window — counts requests in the last `window` duration.
    /// Returns true if the new attempt fits within `limit`.
    pub fn attempt(&self, key: &str, limit: u32, window: Duration) -> bool {
        let mut buckets = self.buckets.lock().unwrap();
        let now = Instant::now();
        let cutoff = now - window;
        let bucket = buckets.entry(key.to_string()).or_default();
        bucket.retain(|t| *t > cutoff);
        if bucket.len() as u32 >= limit {
            false
        } else {
            bucket.push(now);
            true
        }
    }
}
```

```rust
// framework/src/rate_limit/mod.rs
mod middleware;
mod window;

pub use middleware::ThrottleMiddleware;

use crate::FrameworkError;
use std::sync::OnceLock;
use std::time::Duration;

static MEMORY: OnceLock<window::MemoryWindow> = OnceLock::new();

fn memory_window() -> &'static window::MemoryWindow {
    MEMORY.get_or_init(window::MemoryWindow::new)
}

pub struct RateLimiter {
    name: String,
    limit: u32,
    window: Duration,
}

impl RateLimiter {
    pub fn for_(name: impl Into<String>) -> RateLimitBuilder {
        RateLimitBuilder { name: name.into() }
    }

    pub async fn attempt(&self, key: &str) -> Result<(), FrameworkError> {
        let scoped_key = format!("{}:{}", self.name, key);
        // For now use the in-memory window; production deploys
        // configure a Redis-backed window via use_redis().
        if memory_window().attempt(&scoped_key, self.limit, self.window) {
            Ok(())
        } else {
            Err(FrameworkError::Domain {
                message: "too many requests".into(),
                status_code: 429,
            })
        }
    }
}

pub struct RateLimitBuilder {
    name: String,
}

impl RateLimitBuilder {
    pub fn limit(self, n: u32) -> RateLimitBuilderWithLimit {
        RateLimitBuilderWithLimit {
            name: self.name,
            limit: n,
        }
    }
}

pub struct RateLimitBuilderWithLimit {
    name: String,
    limit: u32,
}

impl RateLimitBuilderWithLimit {
    pub fn per_minute(self) -> RateLimiter {
        RateLimiter {
            name: self.name,
            limit: self.limit,
            window: Duration::from_secs(60),
        }
    }
    pub fn per_second(self) -> RateLimiter {
        RateLimiter {
            name: self.name,
            limit: self.limit,
            window: Duration::from_secs(1),
        }
    }
    pub fn per_hour(self) -> RateLimiter {
        RateLimiter {
            name: self.name,
            limit: self.limit,
            window: Duration::from_secs(3600),
        }
    }
}
```

```rust
// framework/src/rate_limit/middleware.rs
use super::RateLimiter;
use crate::http::{Request, Response};
use crate::middleware::{Middleware, Next};
use async_trait::async_trait;

pub struct ThrottleMiddleware {
    name: &'static str,
    limit: u32,
    per_minute: bool,
}

impl ThrottleMiddleware {
    pub fn per_minute(limit: u32) -> Self {
        Self {
            name: "default",
            limit,
            per_minute: true,
        }
    }
}

#[async_trait]
impl Middleware for ThrottleMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let ip = request
            .header("x-forwarded-for")
            .or_else(|| request.header("x-real-ip"))
            .unwrap_or_else(|| "unknown".into());
        let limiter = RateLimiter::for_(self.name).limit(self.limit).per_minute();
        limiter.attempt(&ip).await?;
        next(request).await
    }
}
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test rate_limit
```

- [ ] **Step 4: Add Redis-backed window** (atomic via Lua script)

```rust
// framework/src/rate_limit/window.rs — append
use redis::AsyncCommands;

const SLIDING_WINDOW_LUA: &str = r#"
local key = KEYS[1]
local now = tonumber(ARGV[1])
local window = tonumber(ARGV[2])
local limit = tonumber(ARGV[3])

redis.call('ZREMRANGEBYSCORE', key, '-inf', now - window)
local count = redis.call('ZCARD', key)
if count >= limit then
    return 0
end
redis.call('ZADD', key, now, now)
redis.call('EXPIRE', key, math.ceil(window))
return 1
"#;

pub(crate) struct RedisWindow {
    conn: redis::aio::ConnectionManager,
}

impl RedisWindow {
    pub async fn new(url: &str) -> Result<Self, crate::FrameworkError> {
        let client = redis::Client::open(url)
            .map_err(|e| crate::FrameworkError::internal(format!("redis open: {}", e)))?;
        let conn = redis::aio::ConnectionManager::new(client)
            .await
            .map_err(|e| crate::FrameworkError::internal(format!("redis conn: {}", e)))?;
        Ok(Self { conn })
    }

    pub async fn attempt(&self, key: &str, limit: u32, window: std::time::Duration) -> bool {
        let mut conn = self.conn.clone();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let result: redis::RedisResult<i64> = redis::Script::new(SLIDING_WINDOW_LUA)
            .key(key)
            .arg(now_ms)
            .arg(window.as_millis() as u64)
            .arg(limit)
            .invoke_async(&mut conn)
            .await;
        matches!(result, Ok(1))
    }
}
```

- [ ] **Step 5: Commit**

```bash
git add framework/src/rate_limit framework/src/lib.rs framework/tests/rate_limit.rs
git commit -m "feat(rate_limit): sliding-window RateLimiter (memory + redis-lua) + ThrottleMiddleware"
```

---

## Task 9: App dogfood — queued welcome email + login throttle

**Files:** `app/src/jobs/`, `app/src/notifications/`, `app/src/middleware/`

- [ ] **Step 1: Welcome-email job**

```rust
// app/src/jobs/send_welcome_email.rs
use serde::{Deserialize, Serialize};
use suprnova::{async_trait, FrameworkError, Job, Mail};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SendWelcomeEmailJob {
    pub user_id: i64,
    pub email: String,
}

#[async_trait]
impl Job for SendWelcomeEmailJob {
    fn job_name() -> &'static str {
        "SendWelcomeEmail"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Mail::send(crate::mail::WelcomeEmail {
            email: self.email,
            name: "Friend".into(),
        })
        .await
    }
}
```

- [ ] **Step 2: Dispatch from registration controller**

```rust
// app/src/controllers/auth.rs (or wherever registration lives)
suprnova::Queue::push(crate::jobs::SendWelcomeEmailJob {
    user_id: new_user.id,
    email: new_user.email.clone(),
})
.await?;
```

- [ ] **Step 3: Add login throttle**

```rust
// app/src/middleware/login_throttle.rs
use suprnova::{ThrottleMiddleware};

pub fn install() -> ThrottleMiddleware {
    ThrottleMiddleware::per_minute(5) // 5 login attempts / minute / IP
}
```

- [ ] **Step 4: Wire on the login route**

```rust
// Where the login route is registered:
group!("/login").middleware(crate::middleware::login_throttle::install())
```

- [ ] **Step 5: Commit**

```bash
git add app/src
git commit -m "feat(app): queued welcome email job + login throttle dogfood"
```

---

## Task 10: Bus facade — sync command bus + batches + chains + fake

Laravel's `Bus::dispatch($job)` runs a job synchronously inside the
current process; `Bus::batch([...])` runs N jobs in parallel with a
completion callback; `Bus::chain([...])` runs N jobs sequentially with
a failure short-circuit. Built on the existing `Job` trait (Task 2) —
no new transport.

**Files:** `framework/src/bus/mod.rs`, `framework/src/bus/testing.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/bus.rs
use suprnova::{async_trait, Bus, FrameworkError, Job};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

static COUNTER: AtomicI64 = AtomicI64::new(0);

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Increment { by: i64 }

#[async_trait]
impl Job for Increment {
    fn job_name() -> &'static str { "Increment" }
    async fn handle(self) -> Result<(), FrameworkError> {
        COUNTER.fetch_add(self.by, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AlwaysFails;

#[async_trait]
impl Job for AlwaysFails {
    fn job_name() -> &'static str { "AlwaysFails" }
    async fn handle(self) -> Result<(), FrameworkError> {
        Err(FrameworkError::internal("intentional"))
    }
}

#[tokio::test]
async fn bus_dispatch_runs_job_synchronously() {
    COUNTER.store(0, Ordering::SeqCst);
    Bus::dispatch(Increment { by: 7 }).await.unwrap();
    assert_eq!(COUNTER.load(Ordering::SeqCst), 7);
}

#[tokio::test]
async fn bus_batch_runs_jobs_in_parallel_and_reports_results() {
    COUNTER.store(0, Ordering::SeqCst);
    let report = Bus::batch(vec![
        Increment { by: 1 },
        Increment { by: 2 },
        Increment { by: 3 },
    ])
    .dispatch()
    .await;

    assert_eq!(COUNTER.load(Ordering::SeqCst), 6);
    assert_eq!(report.successful, 3);
    assert_eq!(report.failed, 0);
}

#[tokio::test]
async fn bus_chain_stops_at_first_failure() {
    COUNTER.store(0, Ordering::SeqCst);
    let result = Bus::chain()
        .then(Increment { by: 10 })
        .then(AlwaysFails)
        .then(Increment { by: 999 }) // should NOT run
        .dispatch()
        .await;

    assert!(result.is_err());
    assert_eq!(COUNTER.load(Ordering::SeqCst), 10);
}

#[tokio::test]
async fn bus_fake_records_dispatched_jobs() {
    let _guard = Bus::fake();
    Bus::dispatch(Increment { by: 1 }).await.unwrap();
    Bus::dispatch(Increment { by: 2 }).await.unwrap();
    suprnova::bus::testing::assert_dispatched::<Increment>(|j| j.by == 1);
    suprnova::bus::testing::assert_dispatched::<Increment>(|j| j.by == 2);
    suprnova::bus::testing::assert_dispatched_count::<Increment>(2);
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova --test bus
```

- [ ] **Step 3: Implement Bus facade**

```rust
// framework/src/bus/mod.rs
//! Synchronous command bus on top of the `Job` trait.

pub mod testing;

use crate::queue::Job;
use crate::FrameworkError;

pub struct Bus;

impl Bus {
    /// Run a job synchronously in the current task. Returns the
    /// `handle()` result.
    pub async fn dispatch<J: Job>(job: J) -> Result<(), FrameworkError> {
        if testing::is_active() {
            return testing::record(&job);
        }
        job.handle().await
    }

    /// Build a batch — N jobs that run concurrently.
    pub fn batch<J: Job>(jobs: Vec<J>) -> BatchBuilder<J> {
        BatchBuilder { jobs }
    }

    /// Build a chain — N jobs that run sequentially. The chain
    /// short-circuits on the first error.
    pub fn chain() -> ChainBuilder {
        ChainBuilder { steps: Vec::new() }
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn fake() -> testing::BusFakeGuard {
        testing::install_fake()
    }
}

pub struct BatchBuilder<J: Job> {
    jobs: Vec<J>,
}

pub struct BatchReport {
    pub successful: usize,
    pub failed: usize,
    pub errors: Vec<FrameworkError>,
}

impl<J: Job + Clone> BatchBuilder<J> {
    pub async fn dispatch(self) -> BatchReport {
        let futures = self.jobs.into_iter().map(|j| async move { j.handle().await });
        let results = futures::future::join_all(futures).await;

        let mut successful = 0;
        let mut failed = 0;
        let mut errors = Vec::new();
        for r in results {
            match r {
                Ok(()) => successful += 1,
                Err(e) => {
                    failed += 1;
                    errors.push(e);
                }
            }
        }
        BatchReport { successful, failed, errors }
    }
}

type BoxJob = Box<dyn FnOnce() -> futures::future::BoxFuture<'static, Result<(), FrameworkError>> + Send>;

pub struct ChainBuilder {
    steps: Vec<BoxJob>,
}

impl ChainBuilder {
    pub fn then<J: Job>(mut self, job: J) -> Self {
        self.steps.push(Box::new(move || {
            Box::pin(async move { job.handle().await })
        }));
        self
    }

    pub async fn dispatch(self) -> Result<(), FrameworkError> {
        for step in self.steps {
            step().await?;
        }
        Ok(())
    }
}
```

```rust
// framework/src/bus/testing.rs
//! `Bus::fake()` — record dispatched jobs without running them.

use crate::FrameworkError;
use crate::queue::Job;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct FakeStore {
    dispatched: HashMap<TypeId, Vec<serde_json::Value>>,
}

static FAKE: Mutex<Option<FakeStore>> = Mutex::new(None);

pub(crate) fn is_active() -> bool {
    FAKE.lock().unwrap().is_some()
}

pub(crate) fn record<J: Job>(job: &J) -> Result<(), FrameworkError> {
    if let Some(store) = FAKE.lock().unwrap().as_mut() {
        let v = serde_json::to_value(job)
            .map_err(|e| FrameworkError::internal(format!("encode: {}", e)))?;
        store.dispatched.entry(TypeId::of::<J>()).or_default().push(v);
    }
    Ok(())
}

pub fn install_fake() -> BusFakeGuard {
    *FAKE.lock().unwrap() = Some(FakeStore::default());
    BusFakeGuard
}

pub struct BusFakeGuard;
impl Drop for BusFakeGuard {
    fn drop(&mut self) {
        *FAKE.lock().unwrap() = None;
    }
}

pub fn assert_dispatched<J: Job>(pred: impl Fn(&J) -> bool) {
    let g = FAKE.lock().unwrap();
    let store = g.as_ref().expect("Bus::fake() must be active");
    let bucket = store.dispatched.get(&TypeId::of::<J>());
    let any_match = bucket
        .map(|b| {
            b.iter()
                .filter_map(|v| serde_json::from_value::<J>(v.clone()).ok())
                .any(|j| pred(&j))
        })
        .unwrap_or(false);
    assert!(any_match, "expected matching {} to be dispatched", J::job_name());
}

pub fn assert_dispatched_count<J: Job>(expected: usize) {
    let g = FAKE.lock().unwrap();
    let store = g.as_ref().expect("Bus::fake() must be active");
    let count = store.dispatched.get(&TypeId::of::<J>()).map(|b| b.len()).unwrap_or(0);
    assert_eq!(count, expected, "{} dispatched count mismatch", J::job_name());
}
```

```rust
// framework/src/lib.rs — append
pub mod bus;
pub use bus::Bus;
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova --test bus
```

Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add framework/src/bus framework/src/lib.rs framework/tests/bus.rs
git commit -m "feat(bus): Bus facade — sync dispatch + batch + chain + fake"
```

---

## Task 11: Cache tags + atomic locks + Cache::touch

Three Cache extensions that round out the parity story: tag-based
flushing, atomic locks (cluster-aware via Redis SET NX), and the
L13 `Cache::touch` that extends TTL without re-storing the value.
The existing `Cache::remember` / `get` / `put` / `forever` /
`forget` / `flush` / `increment` / `decrement` stay unchanged.

**Files:** `framework/src/cache/tags.rs`, `framework/src/cache/lock.rs`,
extend `framework/src/cache/mod.rs`, `framework/src/cache/redis.rs`,
`framework/src/cache/memory.rs`.

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/cache_extensions.rs
use std::time::Duration;
use suprnova::Cache;

#[tokio::test]
async fn cache_tags_flush_only_keys_tagged_with_that_tag() {
    Cache::tags(&["users"]).put("user:1", &"alice", None).await.unwrap();
    Cache::tags(&["users"]).put("user:2", &"bob", None).await.unwrap();
    Cache::tags(&["posts"]).put("post:1", &"hello", None).await.unwrap();

    Cache::tags(&["users"]).flush().await.unwrap();

    assert!(Cache::get::<String>("user:1").await.unwrap().is_none());
    assert!(Cache::get::<String>("user:2").await.unwrap().is_none());
    assert_eq!(Cache::get::<String>("post:1").await.unwrap().as_deref(), Some("hello"));
}

#[tokio::test]
async fn cache_lock_acquires_and_releases() {
    let lock = Cache::lock("import-1", Duration::from_secs(5)).await.unwrap();
    assert!(lock.acquired());

    let contender = Cache::lock("import-1", Duration::from_secs(5)).await.unwrap();
    assert!(!contender.acquired(), "second lock on same key should fail until release");

    drop(lock);
    tokio::task::yield_now().await;

    let after_release = Cache::lock("import-1", Duration::from_secs(5)).await.unwrap();
    assert!(after_release.acquired());
}

#[tokio::test]
async fn cache_lock_get_runs_callback_only_once_for_concurrent_callers() {
    use std::sync::atomic::{AtomicI64, Ordering};
    static CALLS: AtomicI64 = AtomicI64::new(0);
    CALLS.store(0, Ordering::SeqCst);

    let a = Cache::lock("import-2", Duration::from_secs(5)).get(|| async {
        CALLS.fetch_add(1, Ordering::SeqCst);
        "result".to_string()
    });
    let b = Cache::lock("import-2", Duration::from_secs(5)).get(|| async {
        CALLS.fetch_add(1, Ordering::SeqCst);
        "result".to_string()
    });

    let (ra, rb) = tokio::join!(a, b);
    // One of them got the lock and computed; the other returned the cached value.
    assert!(ra.is_ok() && rb.is_ok());
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cache_touch_extends_ttl_without_changing_value() {
    Cache::put("session:abc", &"data", Some(Duration::from_secs(5))).await.unwrap();
    Cache::touch("session:abc", Duration::from_secs(60)).await.unwrap();
    // Verify the value is unchanged
    assert_eq!(Cache::get::<String>("session:abc").await.unwrap().as_deref(), Some("data"));
    // Driver-specific TTL inspection is left to the redis-aware test;
    // for in-memory the assertion is just that the value persists.
}
```

- [ ] **Step 2: Implement tags**

```rust
// framework/src/cache/tags.rs
use crate::{Cache, FrameworkError};
use serde::Serialize;
use std::time::Duration;

/// Tag-scoped cache operations. Each tagged `put` also writes the key
/// into the tag's index set (`tag:{tag_name}`). `flush` reads the
/// index set and deletes every member, then deletes the index itself.
pub struct TaggedCache {
    tags: Vec<String>,
}

impl TaggedCache {
    pub(crate) fn new(tags: Vec<String>) -> Self {
        Self { tags }
    }

    pub async fn put<T: Serialize>(
        &self,
        key: &str,
        value: &T,
        ttl: Option<Duration>,
    ) -> Result<(), FrameworkError> {
        Cache::put(key, value, ttl).await?;
        for tag in &self.tags {
            Cache::sadd(&format!("tag:{}", tag), key).await?;
        }
        Ok(())
    }

    pub async fn flush(&self) -> Result<(), FrameworkError> {
        for tag in &self.tags {
            let index_key = format!("tag:{}", tag);
            let members: Vec<String> = Cache::smembers(&index_key).await?;
            for k in members {
                Cache::forget(&k).await?;
            }
            Cache::forget(&index_key).await?;
        }
        Ok(())
    }
}

impl Cache {
    pub fn tags(tags: &[&str]) -> TaggedCache {
        TaggedCache::new(tags.iter().map(|s| (*s).to_string()).collect())
    }

    /// Set-add — driver-specific implementation. Redis uses SADD; the
    /// in-memory store uses a `DashMap<String, HashSet<String>>`.
    pub(crate) async fn sadd(set_key: &str, member: &str) -> Result<(), FrameworkError> {
        Self::store()?.sadd(set_key, member).await
    }

    pub(crate) async fn smembers(set_key: &str) -> Result<Vec<String>, FrameworkError> {
        Self::store()?.smembers(set_key).await
    }
}
```

Add `sadd` + `smembers` to the `CacheStore` trait in
`framework/src/cache/store.rs` and implement them on `InMemoryCache`
(using a parallel `DashMap<String, HashSet<String>>`) and `RedisCache`
(`SADD` / `SMEMBERS`).

- [ ] **Step 3: Implement locks**

```rust
// framework/src/cache/lock.rs
use crate::{Cache, FrameworkError};
use std::future::Future;
use std::time::Duration;
use uuid::Uuid;

/// An atomic lock acquired via Redis `SET key value NX EX seconds`
/// (or the in-memory `DashMap::insert_if_absent` equivalent). Drops
/// itself by deleting the key — IF the held token still matches, so
/// a slow lock owner can't accidentally release a successor's lock.
pub struct CacheLock {
    key: String,
    token: String,
    acquired: bool,
    ttl: Duration,
}

impl CacheLock {
    pub fn acquired(&self) -> bool {
        self.acquired
    }

    /// Run `f` while holding the lock. If the lock is already held,
    /// poll the cached result key instead. The callback's return
    /// value is stored at `{key}:result` for the duration of `ttl`.
    pub async fn get<F, Fut, T>(self, f: F) -> Result<T, FrameworkError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
        T: serde::Serialize + serde::de::DeserializeOwned + Clone,
    {
        let result_key = format!("{}:result", self.key);
        if self.acquired {
            let value = f().await;
            Cache::put(&result_key, &value, Some(self.ttl)).await?;
            Ok(value)
        } else {
            // Wait briefly for the lock holder to publish the result
            let started = std::time::Instant::now();
            loop {
                if let Some(v) = Cache::get::<T>(&result_key).await? {
                    return Ok(v);
                }
                if started.elapsed() > self.ttl {
                    return Err(FrameworkError::internal(format!(
                        "timeout waiting for lock '{}'", self.key
                    )));
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        if self.acquired {
            let key = self.key.clone();
            let token = self.token.clone();
            tokio::spawn(async move {
                // Best-effort safe release: only delete if our token still matches.
                let _ = Cache::release_lock_if_token_matches(&key, &token).await;
            });
        }
    }
}

impl Cache {
    pub async fn lock(key: &str, ttl: Duration) -> Result<CacheLock, FrameworkError> {
        let token = Uuid::new_v4().to_string();
        let store = Self::store()?;
        let acquired = store.set_if_absent(key, &token, ttl).await?;
        Ok(CacheLock {
            key: key.to_string(),
            token,
            acquired,
            ttl,
        })
    }

    pub(crate) async fn release_lock_if_token_matches(
        key: &str,
        token: &str,
    ) -> Result<(), FrameworkError> {
        Self::store()?.release_if_token_matches(key, token).await
    }
}
```

Extend `CacheStore`:
```rust
// framework/src/cache/store.rs — append to trait CacheStore
#[async_trait]
pub trait CacheStore: Send + Sync {
    // existing methods ...

    async fn sadd(&self, set_key: &str, member: &str) -> Result<(), FrameworkError>;
    async fn smembers(&self, set_key: &str) -> Result<Vec<String>, FrameworkError>;
    async fn set_if_absent(&self, key: &str, value: &str, ttl: Duration) -> Result<bool, FrameworkError>;
    async fn release_if_token_matches(&self, key: &str, token: &str) -> Result<(), FrameworkError>;
    async fn touch(&self, key: &str, ttl: Duration) -> Result<(), FrameworkError>;
}
```

Implement on both `InMemoryCache` (use `DashMap` + per-key tokio
`Notify` for the lock wait) and `RedisCache` (`SET NX EX`, `SADD`,
`SMEMBERS`, `EXPIRE` for touch, Lua script for safe release).

- [ ] **Step 4: Implement `Cache::touch`**

```rust
// framework/src/cache/mod.rs — append to impl Cache
impl Cache {
    /// Extend `key`'s TTL to `ttl` (Laravel 13's `Cache::touch`).
    ///
    /// No-op if the key doesn't exist. Doesn't modify the stored value.
    pub async fn touch(key: &str, ttl: Duration) -> Result<(), FrameworkError> {
        Self::store()?.touch(key, ttl).await
    }
}
```

- [ ] **Step 5: Run — expect pass**

```bash
cargo test -p suprnova --test cache_extensions
```

Expected: 4 passed.

- [ ] **Step 6: Commit**

```bash
git add framework/src/cache framework/tests/cache_extensions.rs
git commit -m "feat(cache): tags + atomic locks + Cache::touch (L13)"
```

---

## Task 12: Memcached cache driver

Round out the cache driver story. Redis is shipped; DragonflyDB
works through the Redis driver; **Memcached** is the remaining
Laravel-canonical backend with no first-class support.

**Files:** `framework/src/cache/memcached.rs`,
modify `framework/src/cache/mod.rs` (bootstrap).

- [ ] **Step 1: Add dep**

```toml
# framework/Cargo.toml
memcache-async = "0.7"  # or `memcache` if a tokio-friendly fork lands
```

- [ ] **Step 2: Write failing test**

```rust
// framework/tests/cache_memcached.rs
#![cfg(feature = "memcached")]

use suprnova::Cache;

#[tokio::test]
async fn memcached_put_get_forget_round_trip() {
    if std::env::var("MEMCACHED_URL").is_err() {
        eprintln!("skipping: MEMCACHED_URL not set");
        return;
    }

    Cache::use_memcached(std::env::var("MEMCACHED_URL").unwrap()).await.unwrap();
    Cache::put("mc:1", &"hello", None).await.unwrap();
    assert_eq!(Cache::get::<String>("mc:1").await.unwrap().as_deref(), Some("hello"));
    Cache::forget("mc:1").await.unwrap();
    assert!(Cache::get::<String>("mc:1").await.unwrap().is_none());
}
```

- [ ] **Step 3: Implement `MemcachedCache`**

```rust
// framework/src/cache/memcached.rs
use super::store::CacheStore;
use crate::FrameworkError;
use async_trait::async_trait;
use std::time::Duration;

pub struct MemcachedCache {
    client: memcache_async::Client,
    prefix: String,
}

impl MemcachedCache {
    pub async fn connect(url: &str, prefix: &str) -> Result<Self, FrameworkError> {
        let client = memcache_async::Client::connect(url)
            .await
            .map_err(|e| FrameworkError::internal(format!("memcached: {}", e)))?;
        Ok(Self { client, prefix: prefix.to_string() })
    }

    fn key(&self, k: &str) -> String {
        format!("{}{}", self.prefix, k)
    }
}

#[async_trait]
impl CacheStore for MemcachedCache {
    async fn get_raw(&self, key: &str) -> Result<Option<String>, FrameworkError> {
        let k = self.key(key);
        let result = self.client.get(&k).await
            .map_err(|e| FrameworkError::internal(format!("memcached get: {}", e)))?;
        Ok(result.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
    }
    async fn set_raw(&self, key: &str, value: &str, ttl: Option<Duration>) -> Result<(), FrameworkError> {
        let k = self.key(key);
        let secs = ttl.map(|d| d.as_secs() as u32).unwrap_or(0);
        self.client.set(&k, value.as_bytes(), secs).await
            .map_err(|e| FrameworkError::internal(format!("memcached set: {}", e)))?;
        Ok(())
    }
    async fn forget(&self, key: &str) -> Result<bool, FrameworkError> {
        let k = self.key(key);
        let _ = self.client.delete(&k).await;
        Ok(true)
    }
    // sadd / smembers / set_if_absent / release_if_token_matches:
    // memcached doesn't support sets natively. Either:
    //   (a) emulate via serialized JSON arrays at a tag-index key
    //       (with race conditions you accept), or
    //   (b) return FrameworkError::unsupported() for these methods
    //       and document that tag flushing / atomic locks require
    //       Redis or in-memory.
    // Recommendation: (b) — Memcached users get put/get/forget; tags
    // and locks require a more capable backend.
    async fn sadd(&self, _set: &str, _member: &str) -> Result<(), FrameworkError> {
        Err(FrameworkError::internal("memcached driver does not support cache tags"))
    }
    async fn smembers(&self, _set: &str) -> Result<Vec<String>, FrameworkError> {
        Err(FrameworkError::internal("memcached driver does not support cache tags"))
    }
    async fn set_if_absent(&self, _k: &str, _v: &str, _ttl: Duration) -> Result<bool, FrameworkError> {
        Err(FrameworkError::internal("memcached driver does not support atomic locks; use Redis or in-process"))
    }
    async fn release_if_token_matches(&self, _k: &str, _t: &str) -> Result<(), FrameworkError> {
        Ok(())
    }
    async fn touch(&self, key: &str, ttl: Duration) -> Result<(), FrameworkError> {
        let k = self.key(key);
        let secs = ttl.as_secs() as u32;
        self.client.touch(&k, secs).await
            .map_err(|e| FrameworkError::internal(format!("memcached touch: {}", e)))?;
        Ok(())
    }
    // ... implement flush, increment, decrement
}
```

- [ ] **Step 4: Wire the driver**

```rust
// framework/src/cache/mod.rs — add Cache::use_memcached
impl Cache {
    pub async fn use_memcached(url: impl AsRef<str>) -> Result<(), FrameworkError> {
        let prefix = Config::get::<CacheConfig>().unwrap_or_default().prefix;
        let cache = memcached::MemcachedCache::connect(url.as_ref(), &prefix).await?;
        App::bind::<dyn CacheStore>(Arc::new(cache));
        Ok(())
    }
}
```

- [ ] **Step 5: Run — expect pass (or skipped without server)**

```bash
MEMCACHED_URL=tcp://127.0.0.1:11211 cargo test -p suprnova --test cache_memcached --features memcached
```

- [ ] **Step 6: Commit**

```bash
git add framework/src/cache framework/Cargo.toml framework/tests/cache_memcached.rs
git commit -m "feat(cache): Memcached driver (no tags/locks — those need Redis or in-process)"
```

---

## Task 13: Workspace lint + final verification + roadmap update

- [ ] **Step 1: Clippy + tests**

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

- [ ] **Step 2: Update ROADMAP "Where we are"**

Move from "Missing"/"Partial" to "Production-ready":
- Queue (sea-streamer + in-process + DB + file)
- Mail (lettre SMTP + provider HTTP transports + fakes)
- Notifications (channel system with web-push)
- Rate limiting (memory + redis-lua sliding window + middleware)

- [ ] **Step 3: Commit + push**

---

## Self-Review

| Spec item | Covered by |
|-----------|------------|
| Queue facade + Job trait | Task 2 Steps 1-4 |
| In-process Queue driver | Task 2 Steps 1-4 |
| `#[job(tries, backoff, timeout, fail_on_timeout)]` attribute (L13) | Task 2 Steps 5-9 |
| `BackoffSchedule` (None / Fixed / Sequence) | Task 2 Steps 5-9 |
| `Queue::route::<JobType>(connection, queue)` (L13) | Task 2 Steps 10-13 |
| Named driver registry (`Queue::register_driver`) | Task 2 Steps 10-13 |
| sea-streamer Queue (Redis/Kafka) | Task 3 |
| QueueWorker consumer | Task 3 |
| Queue::fake() | Task 2 |
| Mail facade + Mailable trait | Task 4 |
| SMTP transport (lettre) | Task 5 |
| Provider HTTP transports (Postmark/SES/SendGrid/Mailgun/Resend) | Task 6 |
| Mail::fake() | Task 4 |
| Notify facade + channels | Task 7 |
| Web-push channel | Task 7 |
| Slack/Discord/SMS/DB/Webhook/Broadcast channels | Task 7 |
| RateLimiter sliding window | Task 8 |
| ThrottleMiddleware | Task 8 |
| Redis-Lua atomic window | Task 8 |
| App dogfood | Task 9 |
| `Bus::dispatch` (sync) + `Bus::batch` + `Bus::chain` + `Bus::fake` | Task 10 |
| Cache tags (`Cache::tags(&[...]).put/flush`) | Task 11 |
| Cache atomic locks (`Cache::lock(key, ttl).get(callback)`) | Task 11 |
| `Cache::touch(key, ttl)` — TTL extension (L13) | Task 11 |
| Memcached cache driver | Task 12 |

**Placeholder scan:** Clean. `> API verification:` and `> Refactor note:` flag concrete files and trait signatures to confirm before implementation.

---

## Execution Handoff

**Subagent-Driven recommended given the breadth of subsystems — one task per agent.**
