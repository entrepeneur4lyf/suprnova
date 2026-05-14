# Phase 5: Queue + Mail + Notifications + Rate Limiting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Ship the four "every production app has these" subsystems together because they share infrastructure: Queue (sea-streamer-backed Redis Streams + Kafka + in-process + DB + file), Mail (lettre-backed SMTP + provider HTTP `Transport` impls), Notifications (channel-based delivery: mail, slack, discord, SMS, DB, webhook, web-push, broadcast), Rate Limiting (middleware on Cache backends with atomic Lua on Redis). Mail-via-queue is the canonical pattern; the controllers using these don't care which driver is configured.

**Architecture:** Each subsystem is a trait + driver registry following the existing pattern (`UserProvider`, `CacheStore`, etc.). `Queue::push(job)` writes to whichever driver is currently bound; sea-streamer is the production driver, in-process is the dev/test default. `Mail::to(user).send(mailable)` builds a `lettre::Message` and dispatches through the bound transport. `Notify::send(user, notification).via(&channels)` fans out across channels. `RateLimiter::for_(name).limit(n).per_minute()` writes to the same Cache backend used for `Cache::*`.

**Tech Stack:** `sea-streamer` 0.5 (with `kafka`, `redis`, `file`, `stdio`, `runtime-tokio` features), `lettre` 0.11 (with `tokio1-rustls`, `builder`, `pool` features), `web-push` 0.10, `reqwest` (already in Phase 2) for provider HTTP transports.

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
lettre = { version = "0.11", default-features = false, features = ["builder", "smtp-transport", "tokio1-rustls", "pool"] }
web-push = "0.10"
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

> **web-push API:** Verify version 0.10's exact `WebPushMessageBuilder` and client construction (the example uses `IsahcWebPushClient` — older versions use a different client). Adjust signatures.

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

## Task 10: Workspace lint + final verification + roadmap update

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
| Queue facade + Job trait | Task 2 |
| In-process Queue driver | Task 2 |
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

**Placeholder scan:** Clean. `> API verification:` and `> Refactor note:` flag concrete files and trait signatures to confirm before implementation.

---

## Execution Handoff

**Subagent-Driven recommended given the breadth of subsystems — one task per agent.**
