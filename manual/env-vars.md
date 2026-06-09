# Environment Variables

This is the audited list of every environment variable the Suprnova
framework reads at runtime, grouped by the subsystem that consults it.
Every entry has been validated against framework source — defaults,
types, and behaviour reflect what the code actually does, not what the
starter `.env` happens to ship.

The list also covers the variables the `suprnova` CLI binary reads
(dev server, SSR worker) since those appear in the starter `.env` and
readers will look for them here.

See [Configuration](configuration.md) for the loading rules
(`.env` → `.env.<environment>` → process env), the `env*` helpers
(`env`, `env_required`, `env_optional`), and the typed `Config::*`
registration pattern.

## Conventions

- **Default** — the value the framework uses when the variable is
  unset. `none` means there is no default; the framework either
  errors at boot, falls back to a feature default (e.g. `Memory`
  driver), or treats the value as `None`.
- **Type** — the Rust type the variable is parsed into. `bool` values
  accept `true`/`false`/`1`/`0`/`yes`/`no`/`on`/`off`
  (case-insensitive). Out-of-range or unparseable values for typed
  framework knobs are clamped (workflow), `warn!`-logged then
  defaulted (lenient `env()` / `env_optional()`), or fail boot
  (strict `try_from_env`).
- **Required** — `boot` means the framework refuses to start without
  it in the listed environments. `driver` means it's required only
  when the parent driver is selected (e.g. `MAIL_SES_REGION` is
  irrelevant unless `MAIL_DRIVER=ses`). Everything else is optional.

Where a starter `.env` ships a key the framework never reads
(`MAIL_FROM_ADDRESS`, `MAIL_FROM_NAME`, `FILESYSTEM_DISK`), it's
called out at the end of this chapter.

## Application

The `APP_*` family is the framework's identity and crypto root. These
are the variables every Suprnova app sets; the rest of the file
becomes relevant as you opt into subsystems.

| Var | Default | Type | Purpose |
|---|---|---|---|
| `APP_NAME` | `"Suprnova Application"` | `String` | Application name. Used as the TOTP issuer (2FA), the HTTP Basic `WWW-Authenticate` realm, mail subject branding, and structured-log fields. |
| `APP_ENV` | `local` | `String` | Drives `Environment::detect()` and `.env.<suffix>` lookup. Recognised aliases (case-insensitive): `local`, `development`/`dev`, `staging`/`stage`/`stg`, `production`/`prod`, `testing`/`test`. Any other value is preserved as `Environment::Custom(...)` with original casing. |
| `APP_DEBUG` | env-aware (see Required) | `bool` | Verbose error pages + extra logs. Default is `true` in `local`/`development`/`testing` and `false` everywhere else (including `staging`, `production`, and any unrecognised custom environment). An explicit value always wins; an unparseable value falls back to the env-aware default with a `warn!`. The strict `try_from_env` variant aborts boot on a parse failure. |
| `APP_URL` | `"http://localhost:8765"` (AppConfig) / `"http://localhost"` (URL fallback) | `String` | Base URL for absolute URL generation, signed URLs, and Inertia redirects. Trailing slashes are trimmed on read. |
| `APP_KEY` | none — required in non-dev | `String` (base64-url-no-pad, 32 bytes) | AES-256-GCM key for `Crypt`, encrypted sessions, pagination cursors, signed URLs, and any other encrypt-at-rest path. Boot **fails closed** when missing or malformed outside `local`/`development`/`testing`. Generate with `suprnova key:generate`. |
| `APP_KEY_PREVIOUS` | none | `String` (comma-separated base64 keys, max 8) | Comma-separated previous keys used during rotation. `Crypt::decrypt` tries the current `APP_KEY` first, then each entry in order. Hard cap of 8 entries — `crypto::MAX_PREVIOUS_KEYS`. A half-rotated entry that fails to decode aborts boot. See [Encryption](encryption.md#key-rotation). |
| `APP_PREVIOUS_KEYS` | none | `String` (alias of `APP_KEY_PREVIOUS`) | Laravel-compat alias accepted so a Laravel `.env` dropped into a Suprnova deploy still graceful-decrypts legacy data. When both are set with different values, `APP_KEY_PREVIOUS` wins with a `warn!` to surface the duplicate; identical values are accepted silently. |
| `APP_BASE_PATH` | current working directory | `Path` | Root directory the path resolver uses for `config/`, `database/`, `public/`, `storage/`, `resources/`, `lang/`. Useful when running the binary from a different CWD than the project root (e.g. systemd unit, `WorkingDirectory=` not pointing at the project). Falls back to CWD, then `.` if CWD is unavailable. |
| `AUTH_GUARD` | `"web"` | `String` | Name of the default guard read by `Auth::*`. Mirrors Laravel — only the default is env-selectable; named guards live in code via `AuthConfig::guard(name, …)`. |

### App-key required matrix

| Environment | `APP_KEY` required at boot |
|---|---|
| `local` | no (generates an ephemeral key if missing) |
| `development` | no |
| `testing` | no |
| `staging` | yes — boot exits non-zero with a remediation message |
| `production` | yes |
| `Custom(...)` | yes — anything not in the safe-list is treated as production for this check |

## Server

The HTTP listener and request body limits.

| Var | Default | Type | Purpose |
|---|---|---|---|
| `SERVER_HOST` | `"127.0.0.1"` | `String` | Bind address. Set to `0.0.0.0` to expose outside the loopback interface (e.g. in containers). |
| `SERVER_PORT` | `8765` | `u16` | Bind port. Lenient parse warns and defaults; strict `try_from_env` aborts boot on a typo. |
| `SERVER_MAX_BODY_SIZE` | `8388608` (8 MiB) | `usize` (bytes) | Process-global maximum request body size. Per-`FormRequest::max_body_bytes` overrides still apply on individual endpoints. The configured value is wired into the global cap during `Server::from_config`. |

## Database

Connection URL and sqlx pool tuning. `DATABASE_URL` is required for
any subcommand that touches the database (`migrate*`, `db:sync`,
`db:seed`, `queue:work` with `QUEUE_DRIVER=database`, `workflow:work`,
the session DB store) and for `serve` when the app has migrations
registered.

| Var | Default | Type | Purpose |
|---|---|---|---|
| `DATABASE_URL` | none — required when migrations exist | `String` | Connection URL. Scheme selects the driver: `sqlite://path`, `postgres://...` / `postgresql://...`, `mysql://...`, `mariadb://...`. The framework auto-creates the parent directory for SQLite paths. `serve` skips the database connection entirely when the configured `Migrator` has no migrations. |
| `DB_MAX_CONNECTIONS` | `10` | `u32` | sqlx pool ceiling. |
| `DB_MIN_CONNECTIONS` | `1` | `u32` | sqlx pool floor (kept warm). |
| `DB_CONNECT_TIMEOUT` | `30` (seconds) | `u32` | How long sqlx will wait for an initial connection before erroring. |
| `DB_LOGGING` | `false` | `bool` | When true, sqlx logs every statement (use sparingly in production — chatty). |
| `SUPRNOVA_AUTO_MIGRATE_BEST_EFFORT` | `false` | `bool` | When true, a failing auto-migration during `serve` boot is logged but does not abort. Default is fail-closed: boot exits non-zero rather than start against a partially-migrated schema. Pass `--no-migrate` to skip auto-migration entirely. |

## Session

Cookie attributes and lifetime for the session subsystem. Note that
`SESSION_SECURE` defaults to **`true`** — production-safe by default;
flip it off only for local HTTP development.

| Var | Default | Type | Purpose |
|---|---|---|---|
| `SESSION_LIFETIME` | `120` (minutes) | `u64` | Session lifetime in minutes. Parsed via `env_optional`; falls back silently if unparseable. |
| `SESSION_COOKIE` | `"suprnova_session"` | `String` | Session cookie name. |
| `SESSION_PATH` | `"/"` | `String` | Cookie `Path=` attribute. |
| `SESSION_DOMAIN` | unset | `String` | Cookie `Domain=` attribute. Leave unset for host-only cookies (the safer default for most apps). |
| `SESSION_SECURE` | `true` | `bool` | Cookie `Secure` attribute. Defaults to `true`; set to `false` only in local HTTP development. `cookie_http_only` is always `true` and is not env-configurable. |
| `SESSION_SAME_SITE` | `"Lax"` | `String` | `SameSite` attribute. Accepts `Strict`, `Lax`, `None` (case-insensitive). |
| `SESSION_PARTITIONED` | `false` | `bool` | Emit the `Partitioned` / CHIPS cookie attribute for third-party-isolated cookies. |
| `SESSION_EXPIRE_ON_CLOSE` | `false` | `bool` | When true, drop `Max-Age` so the browser deletes the cookie on close (session-cookie semantics). |
| `SESSION_CONNECTION` | unset | `String` | Named DB connection for the session store. Unset means the default connection. |
| `REMEMBER_LIFETIME` | `43200` (30 days, in minutes) | `u64` | "Remember me" cookie/token lifetime in minutes. |

## Cache

| Var | Default | Type | Purpose |
|---|---|---|---|
| `CACHE_DRIVER` | `memory` | `String` (`memory`/`in-memory`/`inmemory`, `redis`) | Selects the bootstrap target. Memory keeps everything in-process; Redis requires `REDIS_URL` and fails boot if unreachable. Unknown values fail boot with a clear error. |
| `REDIS_URL` | `"redis://127.0.0.1:6379"` | `String` | Redis connection URL (consulted only when `CACHE_DRIVER=redis`). |
| `REDIS_PREFIX` | `"suprnova_cache:"` | `String` | Key prefix for cache entries (collision-avoidance for shared Redis). |
| `CACHE_DEFAULT_TTL` | `3600` (seconds) | `u64` | Default TTL in seconds. `0` means "no expiration". Applied to `Cache::put(None)` / `Cache::tags_put(None)`; `Cache::forever` and `Cache::remember_forever` always bypass. |

## Queue

| Var | Default | Type | Purpose |
|---|---|---|---|
| `QUEUE_DRIVER` | `memory` | `String` (`memory`, `redis`, `database`) | Active queue backend. Unknown values log a `warn!` and fall back to memory. |
| `QUEUE_REDIS_URL` | `"redis://127.0.0.1:6379"` | `String` | Redis URL (required-by-driver when `QUEUE_DRIVER=redis`). |
| `QUEUE_REDIS_STREAM` | `"suprnova-queue"` | `String` | Redis Stream key used for fan-out. |
| `QUEUE_REDIS_GROUP` | `"default"` | `String` | Consumer-group name. |
| `QUEUE_REDIS_CONSUMER` | `"consumer-1"` | `String` | Consumer name within the group. Set per-worker for parallel workers. |
| `QUEUE_VISIBILITY_TIMEOUT_SECS` | `60` | `u64` | How long a claimed job stays invisible before another consumer can reclaim it. Match this to your slowest job. |
| `QUEUE_DB_TABLE` | `"jobs"` | `String` | Table name for the database driver. Validated as a SQL identifier — a malformed value fails at boot, not at SQL composition time. Required-by-driver when `QUEUE_DRIVER=database`; the driver also requires `DB::init()` to have run first. |

## Workflow

The `#[workflow]` long-running stateful worker. All values are clamped
to safe minimums rather than honoured blindly — a `WORKFLOW_CONCURRENCY=0`
would park the worker semaphore forever, so the framework warns and
clamps instead of accepting an obviously-broken config.

| Var | Default | Type | Purpose |
|---|---|---|---|
| `WORKFLOW_CONCURRENCY` | `4` | `usize` | Maximum concurrent workflow executions per worker process. Clamped to `>= 1`. |
| `WORKFLOW_POLL_INTERVAL_MS` | `1000` (ms) | `u64` | How often the worker polls for newly-due workflows. |
| `WORKFLOW_LOCK_TIMEOUT_SECS` | `30` (seconds) | `u64` | Reclaim timeout for a claimed workflow row whose worker has died. |
| `WORKFLOW_MAX_ATTEMPTS` | `3` | `i32` | Max attempts per workflow run before it is marked failed. Clamped to `>= 1`. |
| `WORKFLOW_RETRY_BACKOFF_SECS` | `5` | `i64` | Linear backoff per attempt. Clamped to `>= 0` — negative backoff would schedule retries in the past and produce a tight-loop reclaim. |

## Mail

`MAIL_DRIVER` defaults to **`log`** — outgoing mail prints to the
configured tracing subscriber rather than reaching the network. Flip
to `memory` in tests and `smtp`/`ses`/etc. in production. The
provider-specific keys/tokens are required only when that driver is
selected; an unknown driver value logs a `warn!` and falls back to
`log`.

| Var | Default | Type | Purpose |
|---|---|---|---|
| `MAIL_DRIVER` | `"log"` | `String` (`log`, `memory`, `smtp`, `ses`, `sendgrid`, `mailgun`, `postmark`, `resend`) | Selects the bootstrap target. |
| `MAIL_FROM` | none — required by auth-flow facades | `String` | Default from-address for auth-flow facades (`EmailVerification`, `PasswordReset`, `TwoFactor`). Required for those paths; absent it errors at the call site rather than silently falling back to a placeholder that would break DMARC/SPF. |

### SMTP (`MAIL_DRIVER=smtp`)

| Var | Default | Type | Purpose |
|---|---|---|---|
| `MAIL_SMTP_HOST` | `"127.0.0.1"` | `String` | SMTP host. |
| `MAIL_SMTP_PORT` | `587` | `u16` | SMTP port. |
| `MAIL_SMTP_USER` | unset | `String` | SMTP username. Both `MAIL_SMTP_USER` **and** `MAIL_SMTP_PASS` must be set to enable STARTTLS auth; partial credentials fall through to unencrypted local-dev mode intentionally. |
| `MAIL_SMTP_PASS` | unset | `String` | SMTP password. See `MAIL_SMTP_USER` for the partial-credentials behaviour. |

### Postmark (`MAIL_DRIVER=postmark`)

| Var | Default | Type | Purpose |
|---|---|---|---|
| `MAIL_POSTMARK_TOKEN` | required-by-driver | `String` | Postmark server token. |
| `MAIL_POSTMARK_ENDPOINT` | Postmark default | `String` | Override the API endpoint (regional or mock server). |

### Amazon SES (`MAIL_DRIVER=ses`)

| Var | Default | Type | Purpose |
|---|---|---|---|
| `MAIL_SES_ACCESS_KEY` | required-by-driver | `String` | AWS access key. |
| `MAIL_SES_SECRET_KEY` | required-by-driver | `String` | AWS secret key. |
| `MAIL_SES_REGION` | `"us-east-1"` | `String` | AWS region. |
| `MAIL_SES_ENDPOINT` | AWS default for the region | `String` | Override the SES endpoint (regional or mock server). |

### SendGrid (`MAIL_DRIVER=sendgrid`)

| Var | Default | Type | Purpose |
|---|---|---|---|
| `MAIL_SENDGRID_API_KEY` | required-by-driver | `String` | SendGrid API key. |
| `MAIL_SENDGRID_ENDPOINT` | SendGrid default | `String` | Override the API endpoint. |

### Mailgun (`MAIL_DRIVER=mailgun`)

| Var | Default | Type | Purpose |
|---|---|---|---|
| `MAIL_MAILGUN_API_KEY` | required-by-driver | `String` | Mailgun API key. |
| `MAIL_MAILGUN_DOMAIN` | required-by-driver | `String` | Mailgun sending domain. |
| `MAIL_MAILGUN_ENDPOINT` | Mailgun default | `String` | Override the API endpoint (e.g. EU vs US). |

### Resend (`MAIL_DRIVER=resend`)

| Var | Default | Type | Purpose |
|---|---|---|---|
| `MAIL_RESEND_API_KEY` | required-by-driver | `String` | Resend API key. |
| `MAIL_RESEND_ENDPOINT` | Resend default | `String` | Override the API endpoint. |

## Rate Limiting

| Var | Default | Type | Purpose |
|---|---|---|---|
| `RATE_LIMIT_DRIVER` | `memory` | `String` (`memory`, `redis`) | Selects the rate-limiter backend. Unknown values log a `warn!` and fall back to memory. |
| `RATE_LIMIT_REDIS_URL` | `"redis://127.0.0.1:6379"` | `String` | Redis URL (required-by-driver when `RATE_LIMIT_DRIVER=redis`). |
| `RATE_LIMIT_PREFIX` | `"suprnova:"` | `String` | Key prefix in Redis. |

## Hashing

Password-hashing driver and per-algorithm parameters. Invalid values
return a `FrameworkError::param` at first hash, surfacing
misconfiguration immediately instead of silently defaulting.

| Var | Default | Type | Purpose |
|---|---|---|---|
| `HASH_DRIVER` | `bcrypt` | `String` (`bcrypt`, `argon`/`argon2i`, `argon2id`) | Active hashing algorithm. Case-insensitive. |
| `HASH_ROUNDS` | `12` | `u32` | Bcrypt cost (range `4..=31`). Out-of-range values fail with a clear error. |
| `HASH_MEMORY` | `65536` (64 MiB, KiB units) | `u32` | Argon2 memory in KiB. Minimum `8`. Argon-only. |
| `HASH_TIME` | `4` | `u32` | Argon2 time / iterations. Minimum `1`. Argon-only. |
| `HASH_THREADS` | `1` | `u32` | Argon2 parallelism (matches OWASP / libsodium). Minimum `1`. Argon-only. |
| `HASH_VERIFY` | `false` | `bool` | When true, `verify()` rejects hashes from a different algorithm than `HASH_DRIVER` (returns `Ok(false)`). Default `false` so legacy bcrypt hashes still verify after a driver flip until they're rotated. |

## Auth Flows

Two-factor authentication uses `APP_NAME` (covered under Application)
as the TOTP issuer string — there is no dedicated `2FA_ISSUER` env
var. The issuer falls back to `"Suprnova"` when `APP_NAME` is unset.

## Inertia / Frontend

| Var | Default | Type | Purpose |
|---|---|---|---|
| `SUPRNOVA_FRONTEND` | `svelte` | `String` (`svelte`, `react`, `vue`) | Active frontend. Case-insensitive. Drives `Frontend::detect_from_env()`, the default Vite entry point, and the page-component extension search order at compile time. Unknown or unset values fall back to `svelte`. |

## Maintenance Mode

| Var | Default | Type | Purpose |
|---|---|---|---|
| `MAINTENANCE_DRIVER` | `file` | `String` (`file`, `cache`) | Selects how `down`/`up` state is stored. `file` writes to the framework storage path; `cache` rides on the configured cache driver (useful when many app instances must coordinate maintenance state). Any other value falls back to `file`. |

## Events

| Var | Default | Type | Purpose |
|---|---|---|---|
| `EVENT_MAX_CONCURRENCY` | `256` | `usize` | Ceiling on concurrent queued listener tasks. Values `<= 0` or unparseable fall back to the default. Applies to `Event::queue` / queued listeners; sync listeners are not subject to this limit. |

## Logging

`LOG_FORMAT` is **environment-aware**: in production (`APP_ENV=production`)
the default is `json` for log-aggregator friendliness; everywhere else
the default is `pretty` for human-readable local/dev output. An
explicit value always wins.

| Var | Default | Type | Purpose |
|---|---|---|---|
| `LOG_LEVEL` | `"info"` | `String` (`error`, `warn`, `info`, `debug`, `trace` — case-insensitive) | Tracing-subscriber filter level. |
| `LOG_FORMAT` | env-aware (`json` in production, `pretty` elsewhere) | `String` (`json`, `pretty`) | Tracing-subscriber output format. |

## Observability (OpenTelemetry)

| Var | Default | Type | Purpose |
|---|---|---|---|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | unset (telemetry disabled) | `String` | OTLP collector endpoint. When unset (or whitespace), exporters are not installed and the framework keeps using the standard `tracing` subscriber. |
| `OTEL_SERVICE_NAME` | `"suprnova"` | `String` | `service.name` resource attribute on every span / metric / log record. |
| `OTEL_SERVICE_VERSION` | `CARGO_PKG_VERSION` at build time | `String` | `service.version` resource attribute. |
| `OTEL_SDK_DISABLED` | `false` | `bool` | Standard OTel kill switch. When true, exporters are not installed regardless of `OTEL_EXPORTER_OTLP_ENDPOINT`. |

## CLI / dev server

These are read by the `suprnova` CLI binary (dev server, SSR worker)
rather than the runtime framework — they appear in the starter `.env`
or are honoured by `suprnova serve` / `suprnova ssr:*`.

| Var | Default | Type | Purpose |
|---|---|---|---|
| `VITE_PORT` | `5765` | `u16` | Port Vite binds to in `suprnova serve`. CLI `--frontend-port` overrides. |
| `SUPRNOVA_SSR_RUNTIME` | `"node"` | `String` | Runtime to launch the SSR worker under (`suprnova ssr:start`). CLI `--runtime` overrides. |
| `SUPRNOVA_SSR_BUNDLE` | `frontend/bootstrap/ssr/ssr.js` | `Path` | Path to the built SSR bundle. CLI `--bundle` overrides. |
| `SUPRNOVA_SSR_URL` | `"http://127.0.0.1:13714"` | `String` | SSR worker URL for `suprnova ssr:check`. CLI `--url` overrides. |

## Subsystems with no env vars

A few subsystems are configured entirely in Rust code via the
container or service registration — they have **zero** env vars the
framework reads:

- **Filesystem / Storage.** Disks are registered with
  `FilesystemRegistry::add_disk(name, driver)` in `bootstrap()`. There
  is no `FILESYSTEM_DISK` env var (the name appears in some starter
  `.env` files but is not consulted by the framework — see "Variables
  the framework does not read" below).
- **Broadcasting & WebSockets.** Channels are registered with the
  `ws!()` macro and `BroadcastHub` configuration in code. The driver
  itself rides on whatever the configured `CACHE_DRIVER` selects.
- **CORS, CSRF, Idempotency, Timeout.** Configured via builder structs
  passed to the middleware constructors in `bootstrap()`. The defaults
  are conservative enough that a typical app never touches them.
- **OAuth (torii integration).** Provider client IDs and secrets
  (`GITHUB_CLIENT_ID`, `GOOGLE_CLIENT_ID`, etc.) are *user*
  configuration — your `bootstrap()` reads them via
  `std::env::var(...)` and passes them to `torii::Plugin::new(...)`.
  The framework itself doesn't read them.
- **Vector search, Notifications, Payments, Feature Flags.** Each
  registers concrete drivers via `App::bind` in `bootstrap()`. Pick
  your driver in Rust; pass any URLs/keys it needs as your own env
  vars.

## Variables the framework does not read

The scaffolded starter `.env` lists a few keys for human-author
convenience that the framework never consults. They're documented
here so a reader searching for them isn't left wondering:

- `MAIL_FROM_ADDRESS` and `MAIL_FROM_NAME` — Laravel-style placeholders.
  The actual from-address the auth-flow facades use is `MAIL_FROM`
  (covered under Mail). Your own `Mailable` types can read these vars
  via `env_optional` if you want to keep the Laravel names, but
  nothing in `suprnova::*` does.
- `FILESYSTEM_DISK` — placeholder for the default disk name. Set the
  default in code via `FilesystemRegistry::set_default(name)` instead.

## How values are parsed

A short reference for the three env-helper variants — see
[Configuration](configuration.md#direct-env-access) for the full
treatment:

| Helper | Behaviour on missing | Behaviour on unparseable |
|---|---|---|
| `env(key, default)` | returns `default` | `warn!` + returns `default` |
| `env_required(key)` | **panics** | **panics** |
| `env_optional(key)` | returns `None` | `warn!` + returns `None` |
| `env_strict(key)` (internal, used by `try_from_env`) | returns `Ok(None)` | returns `Err(FrameworkError)` — boot aborts |

Strict variants (`AppConfig::try_from_env`, `ServerConfig::try_from_env`)
are what `Config::init` calls, so a typo in `APP_DEBUG=tru` or
`SERVER_PORT=80a0` aborts boot with a structured error instead of
silently reverting to the default. Lenient variants exist for the
broader call-site population (including `impl Default`) where a parse
failure must not panic.

## Per-environment overrides

The loader reads files in this order, each overriding the previous:

1. `.env`
2. `.env.<environment>` (e.g. `.env.production`, `.env.staging`,
   `.env.testing`, `.env.<custom>` for `APP_ENV=<custom>`)
3. Process environment

That means a containerised production deploy can ship a minimal
`.env.production` overriding only the keys that differ from `.env`
(driver names, URLs, key material), and the real container env
overrides both for secrets that should never land in a committed
file.

See [Configuration](configuration.md#how-env-loading-works) for the
exact loader behaviour and the `LOADED_KEYS` tracking that prevents
stale `.env` values from promoting into the "real system env" tier
across reloads.

## Next

- [Configuration](configuration.md) — typed `Config::*` registration,
  the `env*` helpers, environment detection
- [Deployment](deployment.md) — what to set in production
- [Encryption](encryption.md) — `APP_KEY` rotation via
  `APP_KEY_PREVIOUS`
- [Application Bootstrap](bootstrap.md) — where env-driven boot order
  is established
