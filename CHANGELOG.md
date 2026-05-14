# Changelog

All notable changes to Suprnova will be documented here.
Until a 1.0 release, breaking changes are landed as hard cuts.

## [unreleased] — Phase 2

### Breaking changes

- Session cookies are now AES-256-GCM encrypted. Existing plaintext
  sessions become unreadable after deploy. Set `APP_KEY` (base64
  URL-safe, no padding, 32 bytes) before deploying. Pre-1.0 hard cut,
  no migration path.
- `Http::fake()` is now a closure-scoped async API:
  `Http::fake(|| async { ... }).await` instead of `let _g = Http::fake();`.
  Backing state moved from a process-wide `Mutex` to
  `tokio::task_local!`, so tests no longer need an `HTTP_LOCK` and
  can run in parallel. The dropped type `HttpFakeGuard` is removed
  from public re-exports.
- `ClientResponse::into_inner()` now returns
  `Result<reqwest::Response, FrameworkError>` instead of panicking on
  fake responses. Real responses return `Ok(resp)`; fake responses
  return `Err(FrameworkError::internal("into_inner is not available
  on fake responses"))`. Callers must add a `?` or `.expect(...)`.

### Added

- `Crypt` static facade + `EncryptionKey` (`crypto::*`). 32-byte key
  loaded from `APP_KEY` or generated; AES-256-GCM with 12-byte random
  nonce; `encrypt_string` / `decrypt_string` / `encrypt<T>` /
  `decrypt<T>`. `Crypt::init` runs at `Server::from_config` boot from
  the environment.
- `suprnova key:generate` CLI command — mints a fresh 32-byte AES-256
  key encoded URL-safe base64 (no padding). `--show` prints only the
  key for use in `APP_KEY=$(suprnova key:generate --show)`; the
  default form prints the key plus shell hints.
- `Http` facade (`http_client::*`) — `get` / `post` / `put` / `patch` /
  `delete` return a `RequestBuilder`; `.send().await` produces a
  `ClientResponse` newtype around `reqwest::Response`. rustls TLS, 30s
  default timeout, `suprnova/<version>` user-agent. `RequestBuilder`
  supports `json` / `form` / `body` / `header` / `bearer_token` /
  `basic_auth` / `timeout`.
- `Http::fake()` test guard with `fake_response(method, url_substring,
  status, body)` + `assert_sent` / `assert_not_sent`.
- `RequestBuilder::retry(max_attempts, base_backoff)` — exponential
  backoff retries for transient failures and 5xx responses. 4xx
  short-circuits to no retry; connect/timeout errors during a single
  attempt retry. For `503` responses, the wait is the larger of the
  computed backoff and the `Retry-After` header (delta-seconds).
- `LengthAwarePaginator` + `CursorPaginator` + `Pagination::length_aware`
  / `Pagination::cursor` over SeaORM `Select<E>`. Cursors are encrypted
  via `Crypt` (plain-base64 fallback when `Crypt` is uninitialized) and
  carry a typed `sea_orm::Value` boundary plus a `CursorDirection`
  (`next`/`prev`) — so Postgres, MySQL, and SQLite all receive the
  natively-typed bind without string coercion. `prev_cursor` is wired:
  passing it back walks the previous page (DESC scan, reversed to ASC).
  Supported boundary variants: every scalar `sea_orm::Value` —
  integers, floats, bool, string, char, bytes, uuid, chrono date /
  time / datetime variants, `Decimal`, `BigDecimal`.
- `DbConnection::from_raw(sea_orm::DatabaseConnection)` — wrap an
  existing SeaORM connection (e.g. an in-memory SQLite handle for
  tests) as a `DbConnection` so it can be registered on the container
  and resolve through `DB::connection()`.
- `IntoInertiaScroll` trait; `Inertia::paginate(key, paginator)` facade
  and `InertiaResponse::paginate(key, paginator)` builder method that
  wire either paginator into an Inertia scroll prop.
- `/api/users` dogfood route in `app/` — cursor-paginated 100-user
  fixture. Default path serves an Inertia response via
  `Inertia::paginate("Users/Index", "users", paginator)`; pass
  `?format=json` to receive the raw paginator JSON.
- `suprnova::handle_request(router, middleware_registry, req)` —
  public adapter that serves a single inbound `hyper::Request`
  against a router + middleware chain. Same code path `Server::run`
  uses internally; promoted to `pub` so embedders and tests can wire
  the framework into their own hyper service loop.

### Internal

- Unified reqwest TLS backend: framework's `reqwest` and
  `opentelemetry-otlp` both pull rustls (`rustls-tls` /
  `reqwest-rustls`), eliminating duplicate TLS stacks when the `otel`
  feature is on.
