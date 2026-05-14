# Changelog

All notable changes to Suprnova will be documented here.
Until a 1.0 release, breaking changes are landed as hard cuts.

## [unreleased] — Phase 2

### Breaking changes

- Session cookies are now AES-256-GCM encrypted. Existing plaintext
  sessions become unreadable after deploy. Set `APP_KEY` (base64
  URL-safe, no padding, 32 bytes) before deploying. Pre-1.0 hard cut,
  no migration path.

### Added

- `Crypt` static facade + `EncryptionKey` (`crypto::*`). 32-byte key
  loaded from `APP_KEY` or generated; AES-256-GCM with 12-byte random
  nonce; `encrypt_string` / `decrypt_string` / `encrypt<T>` /
  `decrypt<T>`. `Crypt::init` runs at `Server::from_config` boot from
  the environment.
- `Http` facade (`http_client::*`) — `get` / `post` / `put` / `patch` /
  `delete` return a `RequestBuilder`; `.send().await` produces a
  `ClientResponse` newtype around `reqwest::Response`. rustls TLS, 30s
  default timeout, `suprnova/<version>` user-agent. `RequestBuilder`
  supports `json` / `form` / `body` / `header` / `bearer_token` /
  `basic_auth` / `timeout`.
- `Http::fake()` test guard with `fake_response(method, url_substring,
  status, body)` + `assert_sent` / `assert_not_sent`.
- `LengthAwarePaginator` + `CursorPaginator` + `Pagination::length_aware`
  / `Pagination::cursor` over SeaORM `Select<E>`. Cursors encrypted via
  `Crypt` (plain-base64 fallback when `Crypt` is uninitialized).
- `IntoInertiaScroll` trait; `Inertia::paginate(key, paginator)` facade
  and `InertiaResponse::paginate(key, paginator)` builder method that
  wire either paginator into an Inertia scroll prop.
- `/api/users` dogfood route in `app/` — cursor-paginated 100-user
  fixture.

### Internal

- Unified reqwest TLS backend: framework's `reqwest` and
  `opentelemetry-otlp` both pull rustls (`rustls-tls` /
  `reqwest-rustls`), eliminating duplicate TLS stacks when the `otel`
  feature is on.
