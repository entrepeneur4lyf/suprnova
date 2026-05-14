# Phase 14: Dev Observability (Telescope + Pulse) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Two complementary dashboards mounted at `/telescope` and `/pulse`. Telescope is the **debugging dashboard** — recent requests, DB queries, dispatched events, sent mail, queued jobs, exceptions — each captured with full payload for inspection. Pulse is the **performance dashboard** — slow queries, hot endpoints, queue depth, system health, error rate over time. Both reuse Phase 1's event system as their data source.

**Architecture:** A new `framework/src/observability/` module ships two subsystems. **Telescope** writes recordings to a `telescope_entries` table; a controller serves the dashboard Inertia pages (Vue/React/Svelte starters all get a `Telescope/` page directory). **Pulse** aggregates events into time-window buckets (per-minute, per-hour) stored either in a SQL `pulse_aggregates` table or Redis (faster). Both have an `enabled` config flag — in production, Pulse stays on, Telescope can be on or off depending on infrastructure access policies. RBAC: both dashboards require the `view-observability` gate (Phase 3 Authorization).

**Tech Stack:** No new crates. Reuses Phase 1 events, Phase 3 authorization, Phase 4 SeaORM, Phase 9 i18n optional, the SPA starters from existing scaffolder. Optional `redis` for Pulse (already a dep).

---

## File Structure

**New files:**
- `framework/src/observability/mod.rs` — module entry, `enable_telescope` / `enable_pulse`
- `framework/src/observability/telescope/mod.rs` — `Telescope` facade
- `framework/src/observability/telescope/recorder.rs` — listeners that record events to DB
- `framework/src/observability/telescope/entries.rs` — entry types (Request, Query, Mail, Job, Event, Exception)
- `framework/src/observability/telescope/controller.rs` — `/telescope` routes
- `framework/src/observability/pulse/mod.rs` — `Pulse` facade
- `framework/src/observability/pulse/aggregator.rs` — time-window aggregation
- `framework/src/observability/pulse/recorders/{requests,queries,jobs,errors}.rs`
- `framework/src/observability/pulse/controller.rs` — `/pulse` routes
- `framework/src/observability/migrations/m_create_telescope_entries.rs`
- `framework/src/observability/migrations/m_create_pulse_aggregates.rs`
- Frontend pages: `Telescope/Index.tsx`, `Telescope/Request.tsx`, `Pulse/Index.tsx` (mirror in Vue + Svelte starters)
- `framework/tests/telescope.rs`, `pulse.rs`

---

## Task 1: Migrations

**Files:** migrations, `framework/Cargo.toml`

- [ ] **Step 1: Schema sketches**

```rust
// framework/src/observability/migrations/m_create_telescope_entries.rs
// CREATE TABLE telescope_entries (
//   id BIGINT PRIMARY KEY AUTO_INCREMENT,
//   sequence BIGINT NOT NULL,
//   batch_id VARCHAR(36),       -- groups entries from the same request
//   family_hash VARCHAR(64),
//   kind VARCHAR(64) NOT NULL,  -- "request" | "query" | "mail" | "job" | "exception" | "event"
//   content JSONB NOT NULL,
//   created_at DATETIME NOT NULL,
//   INDEX (kind, created_at DESC),
//   INDEX (batch_id)
// );
```

```rust
// framework/src/observability/migrations/m_create_pulse_aggregates.rs
// CREATE TABLE pulse_aggregates (
//   id BIGINT PRIMARY KEY AUTO_INCREMENT,
//   bucket DATETIME NOT NULL,       -- start of the time window
//   period VARCHAR(8) NOT NULL,     -- "minute" | "hour" | "day"
//   kind VARCHAR(64) NOT NULL,      -- "request" | "slow_query" | "job_throughput" | "error"
//   key VARCHAR(255) NOT NULL,      -- the endpoint / query template / job name
//   count BIGINT NOT NULL,
//   total_ms BIGINT NOT NULL,
//   UNIQUE INDEX (bucket, period, kind, key)
// );
```

- [ ] **Step 2: Commit**

```bash
git add framework/src/observability/migrations
git commit -m "feat(observability): migrations for telescope_entries + pulse_aggregates"
```

---

## Task 2: Telescope entry types + recorder

**Files:** `framework/src/observability/telescope/entries.rs`, `recorder.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/observability/telescope/entries.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntryKind {
    Request,
    Query,
    Mail,
    Job,
    Event,
    Exception,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub kind: EntryKind,
    pub content: serde_json::Value,
    pub batch_id: Option<String>,
}
```

```rust
// framework/src/observability/telescope/recorder.rs
//! Listeners that observe Phase 1 events and write them to the
//! telescope_entries table.

use super::entries::{Entry, EntryKind};
use crate::events::{ErrorOccurred, Listener};
use crate::{async_trait, FrameworkError, DB};
use sea_orm::{ConnectionTrait, Statement};

pub struct ExceptionRecorder;

#[async_trait]
impl Listener<ErrorOccurred> for ExceptionRecorder {
    async fn handle(&self, event: &ErrorOccurred) -> Result<(), FrameworkError> {
        record(Entry {
            kind: EntryKind::Exception,
            content: serde_json::json!({
                "error": event.error_message,
                "status_code": event.status_code,
                "request_id": event.request_id,
            }),
            batch_id: event.request_id.clone(),
        })
        .await
    }
}

pub async fn record(entry: Entry) -> Result<(), FrameworkError> {
    if !super::is_enabled() {
        return Ok(());
    }
    let db = DB::get()?;
    db.execute(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO telescope_entries (sequence, batch_id, family_hash, kind, content, created_at) VALUES ((SELECT COALESCE(MAX(sequence), 0) + 1 FROM telescope_entries), ?, ?, ?, ?, NOW())",
        vec![
            entry.batch_id.into(),
            "".into(),
            format!("{:?}", entry.kind).into(),
            entry.content.to_string().into(),
        ],
    ))
    .await
    .map_err(|e| FrameworkError::internal(format!("telescope record: {}", e)))?;
    Ok(())
}
```

> **Volume control:** Recording every event into a SQL table is fine for dev / staging but will overwhelm production-scale apps. Add a sampling rate (`TELESCOPE_SAMPLE_RATE=0.01`) and a `prune` job that drops entries older than 24h. The `enable_telescope` config should expose these.

- [ ] **Step 2: Commit**

```bash
git add framework/src/observability/telescope
git commit -m "feat(telescope): entry types + ExceptionRecorder listener + record fn"
```

---

## Task 3: Additional Telescope recorders

**Files:** `framework/src/observability/telescope/recorder.rs` extensions

- [ ] **Step 1: RequestRecorder via middleware**

```rust
// framework/src/observability/telescope/request_middleware.rs
use crate::http::{Request, Response};
use crate::middleware::{Middleware, Next};
use async_trait::async_trait;
use std::time::Instant;

pub struct TelescopeRequestMiddleware;

#[async_trait]
impl Middleware for TelescopeRequestMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        if !super::super::is_enabled() {
            return next(request).await;
        }
        let started = Instant::now();
        let request_id = crate::current_request_id().map(|id| id.as_str().to_string());
        let method = request.method().to_string();
        let path = request.path().to_string();

        let response = next(request).await;
        let elapsed_ms = started.elapsed().as_millis();
        let status = match &response {
            Ok(r) => r.status_code(),
            Err(r) => r.status_code(),
        };

        let _ = super::record(super::entries::Entry {
            kind: super::entries::EntryKind::Request,
            content: serde_json::json!({
                "method": method,
                "path": path,
                "status": status,
                "duration_ms": elapsed_ms,
            }),
            batch_id: request_id,
        })
        .await;

        response
    }
}
```

- [ ] **Step 2: QueryRecorder** — hook SeaORM's tracing layer (it emits `sqlx_query` spans) into a recorder that writes to telescope.

> **SeaORM query interception:** SeaORM emits queries through `tracing`. Add a custom `tracing` Layer that filters for `sqlx::query` spans and persists them via `record()`. Implementer: implement `tracing_subscriber::Layer` and install it from `enable_telescope()`.

- [ ] **Step 3: MailRecorder + JobRecorder + EventRecorder**

```rust
// Each listens to its respective event from Phase 5:
//  - Mail::send → emit MailDispatched event → record
//  - Queue::push → record (no event needed; tap inside Queue::push)
//  - Event::dispatch → record (instrument inside EventDispatcher)
```

These require small touches in `framework/src/mail/mod.rs`, `framework/src/queue/mod.rs`, and `framework/src/events/dispatcher.rs` — each, after delegating to the driver/listener, calls `telescope::record(...)` if Telescope is enabled.

- [ ] **Step 4: Commit each recorder as a separate commit**

```bash
git commit -m "feat(telescope): RequestRecorder middleware"
git commit -m "feat(telescope): QueryRecorder via tracing Layer"
git commit -m "feat(telescope): Mail/Job/Event recorders"
```

---

## Task 4: Telescope `/telescope` routes + Inertia pages

**Files:** `framework/src/observability/telescope/controller.rs`, frontend pages

- [ ] **Step 1: Backend**

```rust
// framework/src/observability/telescope/controller.rs
use crate::{Gate, Inertia, Request, Response, DB};
use sea_orm::{ConnectionTrait, Statement};

pub async fn index(req: Request) -> Response {
    // Gate::authorize requires (user, resource) — Telescope uses a
    // marker resource type:
    struct TelescopeResource;
    let user = crate::Auth::user_as::<crate::AuthUser>().await?.ok_or(crate::FrameworkError::Unauthorized)?;
    Gate::authorize("view-observability", &user, &TelescopeResource)?;

    let kind = req.query("kind").unwrap_or_else(|| "request".into());
    let db = DB::get()?;
    let rows = db
        .query_all(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT id, kind, content, created_at FROM telescope_entries WHERE kind = ? ORDER BY id DESC LIMIT 100",
            vec![kind.clone().into()],
        ))
        .await
        .map_err(|e| crate::FrameworkError::internal(format!("query: {}", e)))?;
    let entries: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.try_get::<i64>("", "id").unwrap_or(0),
                "kind": r.try_get::<String>("", "kind").unwrap_or_default(),
                "content": r.try_get::<String>("", "content").unwrap_or_default(),
                "created_at": r.try_get::<String>("", "created_at").unwrap_or_default(),
            })
        })
        .collect();

    Ok(Inertia::render("Telescope/Index", serde_json::json!({
        "kind": kind,
        "entries": entries,
    }))?)
}

pub async fn show(req: Request) -> Response {
    let id: i64 = req.param("id")?.parse().map_err(|_| crate::FrameworkError::param("id"))?;
    let db = DB::get()?;
    let row = db
        .query_one(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT id, kind, content, created_at, batch_id FROM telescope_entries WHERE id = ?",
            vec![id.into()],
        ))
        .await
        .map_err(|e| crate::FrameworkError::internal(format!("query: {}", e)))?
        .ok_or_else(|| crate::FrameworkError::model_not_found("TelescopeEntry"))?;

    let entry = serde_json::json!({
        "id": row.try_get::<i64>("", "id").unwrap_or(0),
        "kind": row.try_get::<String>("", "kind").unwrap_or_default(),
        "content": row.try_get::<String>("", "content").unwrap_or_default(),
        "batch_id": row.try_get::<String>("", "batch_id").unwrap_or_default(),
    });

    Ok(Inertia::render("Telescope/Entry", entry)?)
}
```

- [ ] **Step 2: Frontend pages**

Mirror under `suprnova-cli/src/templates/files/observability/<framework>/`:
- `Telescope/Index.tsx` — table of recent entries filtered by kind
- `Telescope/Entry.tsx` — single-entry inspector (pretty-prints JSON content)
- `Telescope/Layout.tsx` — left-nav with kinds (Requests, Queries, Mail, Jobs, Events, Exceptions)

- [ ] **Step 3: Wire routes**

```rust
// framework/src/observability/mod.rs — install_routes called from server boot
pub fn install_routes(router: &mut crate::routing::Router) {
    router.add_route("GET", "/telescope", telescope::controller::index);
    router.add_route("GET", "/telescope/:id", telescope::controller::show);
    router.add_route("GET", "/pulse", pulse::controller::index);
}
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/observability suprnova-cli/src/templates/files/observability
git commit -m "feat(telescope): /telescope index + show routes + Inertia pages"
```

---

## Task 5: Pulse — time-window aggregation

**Files:** `framework/src/observability/pulse/aggregator.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/observability/pulse/aggregator.rs
//! Tick a per-minute aggregation of incoming events. Each event-kind
//! gets a count + total_ms (where applicable). At the start of every
//! minute we flush in-memory buckets to `pulse_aggregates`.

use crate::FrameworkError;
use chrono::{DateTime, Duration, DurationRound, Utc};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default, Clone)]
struct Bucket {
    count: u64,
    total_ms: u64,
}

#[derive(Default)]
struct Buckets {
    current_minute: DateTime<Utc>,
    by_key: HashMap<(String, String), Bucket>, // (kind, key) → bucket
}

static BUCKETS: Mutex<Option<Buckets>> = Mutex::new(None);

pub fn record(kind: &str, key: &str, duration_ms: u64) {
    let mut g = BUCKETS.lock().unwrap();
    let buckets = g.get_or_insert_with(Buckets::default);
    let now = Utc::now().duration_round(Duration::minutes(1)).unwrap();
    if now != buckets.current_minute {
        // Flush previous minute (sketch — schedule via background task in real impl)
        let _ = flush_blocking(&buckets);
        buckets.by_key.clear();
        buckets.current_minute = now;
    }
    let bucket = buckets.by_key.entry((kind.to_string(), key.to_string())).or_default();
    bucket.count += 1;
    bucket.total_ms += duration_ms;
}

fn flush_blocking(buckets: &Buckets) -> Result<(), FrameworkError> {
    // Spawn a tokio task to write each (kind, key) → (count, total_ms)
    // row to pulse_aggregates. The DB call is async, so the actual
    // implementation uses tokio::spawn:
    //
    //   tokio::spawn(async move {
    //       for ((kind, key), bucket) in buckets.by_key.clone() {
    //           let _ = DB::get()?.execute(stmt).await;
    //       }
    //   });
    let _ = buckets;
    Ok(())
}
```

- [ ] **Step 2: Recorders that feed the aggregator**

```rust
// framework/src/observability/pulse/recorders/requests.rs
// Hook into the same TelescopeRequestMiddleware path (or a separate
// PulseRequestMiddleware) and call pulse::aggregator::record("request",
// &format!("{} {}", method, path), elapsed_ms).
```

```rust
// framework/src/observability/pulse/recorders/queries.rs
// Hook into the same QueryRecorder tracing layer; record by SQL
// template (normalized — strip literal values) so "slow query" rows
// group sensibly.
```

```rust
// framework/src/observability/pulse/recorders/errors.rs
// Hook ErrorOccurred event; record("error", &format!("{}", status_code), 0).
```

- [ ] **Step 3: Commit**

```bash
git add framework/src/observability/pulse
git commit -m "feat(pulse): time-window aggregator + per-kind recorders"
```

---

## Task 6: Pulse `/pulse` dashboard

**Files:** `framework/src/observability/pulse/controller.rs`, frontend pages

- [ ] **Step 1: Backend**

```rust
// framework/src/observability/pulse/controller.rs
use crate::{Gate, Inertia, Request, Response, DB};
use sea_orm::{ConnectionTrait, Statement};

pub async fn index(_req: Request) -> Response {
    struct PulseResource;
    let user = crate::Auth::user_as::<crate::AuthUser>().await?.ok_or(crate::FrameworkError::Unauthorized)?;
    Gate::authorize("view-observability", &user, &PulseResource)?;

    let db = DB::get()?;
    // Top 10 slowest endpoints over the last hour:
    let slow_endpoints = db
        .query_all(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT key, SUM(count) AS hits, SUM(total_ms)::float / SUM(count) AS avg_ms \
             FROM pulse_aggregates \
             WHERE kind = 'request' AND bucket > NOW() - INTERVAL '1 hour' \
             GROUP BY key \
             ORDER BY avg_ms DESC \
             LIMIT 10",
            vec![],
        ))
        .await
        .map_err(|e| crate::FrameworkError::internal(format!("pulse query: {}", e)))?;

    // Similar for: slow_queries, error_rate_by_minute, queue_depth_by_minute.

    Ok(Inertia::render("Pulse/Index", serde_json::json!({
        "slow_endpoints": slow_endpoints.iter().map(|r| serde_json::json!({
            "endpoint": r.try_get::<String>("", "key").unwrap_or_default(),
            "hits": r.try_get::<i64>("", "hits").unwrap_or(0),
            "avg_ms": r.try_get::<f64>("", "avg_ms").unwrap_or(0.0),
        })).collect::<Vec<_>>(),
        // ... other panels ...
    }))?)
}
```

- [ ] **Step 2: Frontend page** — `Pulse/Index.tsx` with cards: "Slow endpoints", "Slow queries", "Error rate (last 15min)", "Queue depth".

- [ ] **Step 3: Commit**

```bash
git add framework/src/observability/pulse/controller.rs suprnova-cli/src/templates/files/observability/<frameworks>/src/pages/Pulse
git commit -m "feat(pulse): /pulse dashboard with slow endpoints, queries, errors, queue depth"
```

---

## Task 7: Boot wiring + auth gate

**Files:** `framework/src/observability/mod.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/observability/mod.rs
use crate::FrameworkError;
use std::sync::atomic::{AtomicBool, Ordering};

pub mod telescope;
pub mod pulse;

static TELESCOPE_ENABLED: AtomicBool = AtomicBool::new(false);
static PULSE_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn enable_telescope() {
    TELESCOPE_ENABLED.store(true, Ordering::SeqCst);
}

pub fn enable_pulse() {
    PULSE_ENABLED.store(true, Ordering::SeqCst);
}

pub fn is_enabled() -> bool {
    TELESCOPE_ENABLED.load(Ordering::SeqCst)
}

pub fn pulse_enabled() -> bool {
    PULSE_ENABLED.load(Ordering::SeqCst)
}

pub async fn boot(router: &mut crate::routing::Router) -> Result<(), FrameworkError> {
    // Always register the gate; opt-in flags control whether the
    // recorders actually run.
    crate::Gate::define::<crate::AuthUser, ()>(
        "view-observability",
        |user, _| user.is_admin,
    );

    if is_enabled() {
        telescope::controller::install_routes(router);
        crate::Event::listen::<crate::events::ErrorOccurred>(
            std::sync::Arc::new(telescope::recorder::ExceptionRecorder),
        )
        .await;
        // Install TelescopeRequestMiddleware globally:
        crate::middleware::register_global_middleware(telescope::request_middleware::TelescopeRequestMiddleware);
    }
    if pulse_enabled() {
        pulse::controller::install_routes(router);
    }
    Ok(())
}
```

- [ ] **Step 2: Commit**

```bash
git add framework/src/observability/mod.rs
git commit -m "feat(observability): boot wiring + view-observability gate + opt-in flags"
```

---

## Task 8: App dogfood

```rust
// app/src/bootstrap.rs
suprnova::observability::enable_telescope();
suprnova::observability::enable_pulse();
```

- [ ] **Smoke test:**

```bash
cargo run -p app -- serve &
sleep 2
# Send some traffic
for i in {1..10}; do curl -s http://127.0.0.1:8000/ > /dev/null; done
# Visit /telescope (logged in as admin)
curl http://127.0.0.1:8000/telescope -b "session=..."
kill %1
```

- [ ] **Commit**

```bash
git add app/src/bootstrap.rs
git commit -m "feat(app): enable telescope + pulse in dev"
```

---

## Task 9: Workspace lint + roadmap

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

ROADMAP "Where we are" — move Telescope + Pulse to Production-ready. Commit + push.

---

## Self-Review

| Spec item | Covered by |
|-----------|------------|
| Telescope entry types + recorder | Task 2 |
| Request / Query / Mail / Job / Event / Exception recorders | Task 3 |
| /telescope routes + UI | Task 4 |
| Pulse time-window aggregator | Task 5 |
| Pulse recorders | Task 5 |
| /pulse dashboard | Task 6 |
| Boot wiring + gate | Task 7 |
| Dogfood | Task 8 |

---

## Execution Handoff

**Subagent-Driven recommended — Telescope recorders can run in parallel.**
