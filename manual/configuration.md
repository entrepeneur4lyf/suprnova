# Configuration

Suprnova reads configuration from environment variables (loaded from
`.env` in development, the process environment in production) and
exposes them to your code in two shapes:

1. **Direct env access** — `env::env`, `env_required`, `env_optional`
   for one-off lookups
2. **Typed config structs** — `Config::register` / `Config::get` for
   anything you read more than once, with strong typing

The framework reads a handful of env vars itself (`APP_KEY`,
`APP_ENV`, `DATABASE_URL`, etc.); the rest are yours.

## The `.env` file

`suprnova new` writes a starter `.env` with the values your app needs
to boot:

```env
APP_NAME="my-app"
APP_ENV=local                # local, development, staging, production, testing, …
APP_DEBUG=true               # detailed error pages + verbose logs
APP_URL=http://localhost:8080

# 32-byte AES-256 key (URL-safe base64, no padding). Encrypts session
# cookies, pagination cursors, and anything via `suprnova::Crypt`.
# Generated at scaffold time. Rotate with `suprnova key:generate`.
APP_KEY=<32-byte base64>

SERVER_HOST=127.0.0.1
SERVER_PORT=8080
VITE_PORT=5173

# Database — SQLite by default; swap to postgres://user:pass@host/db
DATABASE_URL=sqlite://./database.db
DB_MAX_CONNECTIONS=10
DB_MIN_CONNECTIONS=1
DB_CONNECT_TIMEOUT=30
DB_LOGGING=false

# Session
SESSION_LIFETIME=120         # minutes
SESSION_COOKIE=suprnova_session
SESSION_SECURE=false         # set true in production (HTTPS only)
SESSION_PATH=/
SESSION_SAME_SITE=Lax

# Mail
MAIL_DRIVER=smtp             # smtp, ses, mailgun, postmark, sendgrid, resend, log, memory
MAIL_SMTP_HOST=127.0.0.1
MAIL_SMTP_PORT=587
MAIL_SMTP_USER=
MAIL_SMTP_PASS=
```

A sibling `.env.example` ships the same keys with placeholder values —
commit it; do not commit `.env`. The default `.gitignore` excludes
`.env` already.

## How `.env` loading works

At boot, the framework:

1. Detects the environment from `APP_ENV` (case-insensitive,
   `prod`/`dev`/`stage`/`stg`/`test` are also recognised).
2. Loads `.env` from the project root.
3. If a per-environment file exists (`.env.staging`, `.env.production`),
   loads it on top — its values override `.env`.
4. Real process environment variables override both (this is what
   container orchestration relies on).

The order in one line: **process env > `.env.<environment>` > `.env`**.

```rust
use suprnova::Config;

let env = Config::environment();           // Environment::Local
let is_prod = Config::is_production();     // false
```

In a CI run with `APP_ENV=testing`, the framework loads `.env.testing`
on top of `.env` so you can override DB URLs and disable mail drivers
without touching the dev `.env`.

## Direct env access

For one-off reads of strings, numbers, bools — anything implementing
`std::str::FromStr` — use the `env::*` family:

```rust
use suprnova::config::{env, env_required, env_optional};

let port: u16 = env("SERVER_PORT", 8080);                    // with default
let url: String = env_required("APP_URL");                   // panics if missing — boot-only
let smtp_host: Option<String> = env_optional("MAIL_HOST");   // None if missing
```

- `env(key, default)` — type-coerced read with fallback
- `env_required(key)` — panics if the key is missing or fails to
  parse. Only use this at boot time (in `bootstrap()` or `config::register()`)
  where a missing required value should crash the process immediately
- `env_optional(key)` — returns `Option<T>`; `None` for missing or
  unparseable values

Each unique key is also logged once on first read, so you can audit
exactly which env vars your app touches.

## Typed config structs

For anything your app reads more than once, define a typed struct
and register it. The pattern is:

```rust
// src/config/database.rs
use suprnova::Config;
use suprnova::config::{env, env_required, env_optional};

#[derive(Clone, Debug)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout_secs: u32,
    pub logging: bool,
}

pub fn register() {
    Config::register(DatabaseConfig {
        url: env_required("DATABASE_URL"),
        max_connections: env("DB_MAX_CONNECTIONS", 10),
        min_connections: env("DB_MIN_CONNECTIONS", 1),
        connect_timeout_secs: env("DB_CONNECT_TIMEOUT", 30),
        logging: env("DB_LOGGING", false),
    });
}
```

Then read it anywhere with one line:

```rust
let db = Config::get::<DatabaseConfig>().expect("DB config registered at boot");
println!("Pool size: {}", db.max_connections);
```

The registry is keyed by `TypeId`, so each struct is stored once.
Calling `Config::register` again with the same type replaces the
previous entry — convenient for tests.

### Wiring registration into your app

The scaffold's `cmd/main.rs` includes a `.config(…)` step in the
fluent boot pipeline:

```rust
use suprnova::Application;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    Application::new()
        .config(my_app::config::register)   // ← this calls your registration
        .bootstrap(my_app::bootstrap::bootstrap)
        .routes(my_app::routes::register)
        .migrations::<my_app::migrations::Migrator>()
        .run()
        .await
}
```

`my_app::config::register` typically delegates to each section module:

```rust
// src/config/mod.rs
pub mod database;
pub mod mail;

pub fn register() {
    database::register();
    mail::register();
}
```

### Deserialising whole structs from env

For larger configs, you can deserialise directly from env vars via
`serde`. Suprnova exposes two helpers:

```rust
use suprnova::Config;

#[derive(Clone, Debug, serde::Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

// Reads SERVER_HOST / SERVER_PORT from the environment
let cfg = Config::resolve_prefixed::<ServerConfig>("SERVER_")?;
```

- `Config::resolve::<T>()` — deserialise from all process env vars
- `Config::resolve_prefixed::<T>("PREFIX_")` — deserialise only
  vars with the given prefix (the prefix is stripped before
  deserialisation)

Both return `Result<T, FrameworkError>` so a missing required field
surfaces as a `FrameworkError::Internal` carrying the envy diagnostic
instead of a panic.

## Environment-specific config

The `Environment` enum covers the standard set:

| Variant | Recognised `APP_ENV` values |
|---|---|
| `Local` | `local` |
| `Development` | `development`, `dev` |
| `Staging` | `staging`, `stage`, `stg` |
| `Production` | `production`, `prod` |
| `Testing` | `testing`, `test` |
| `Custom(String)` | anything else (preserves your casing, used for `.env.<custom>` lookup) |

Common branches:

```rust
use suprnova::{Config, Environment};

if Config::is_production() {
    // strict cookies, real mail driver, etc.
}

if Config::is_debug() {
    // verbose error pages, query logging
}

match Config::environment() {
    Environment::Production => { /* … */ },
    Environment::Staging    => { /* … */ },
    _ => { /* dev/test path */ },
}
```

`is_debug()` returns `true` when `APP_DEBUG=true` is set explicitly,
or — when `APP_DEBUG` is unset — when the detected environment is
`Local`, `Development`, or `Testing`. Production, staging, and any
unrecognised custom environment default to `false`. Keep it off in
production; it controls error-page detail and a few internal defaults.

### `APP_KEY` is required in non-development

In production (any `APP_ENV` other than `local`/`development`/
`testing`), Suprnova requires `APP_KEY` to be set to a valid 32-byte
URL-safe base64 string. Booting without it fails closed with a
descriptive error message — there is no silent fallback.

If you don't have an `APP_KEY` yet:

```bash
suprnova key:generate          # prints the key with a hint reminding you to add it to .env
suprnova key:generate --show   # prints only the key, suitable for `APP_KEY=$(suprnova key:generate --show)`
```

Neither form edits `.env` for you — copy the printed key into your
`.env` (or your secrets manager) yourself.

For key rotation (where old encrypted data must still decrypt during
the migration window), see [Encryption](encryption.md#key-rotation).

## Configuration in tests

In tests, register config in the test setup rather than relying on
`.env`:

```rust
use suprnova::suprnova_test;

#[suprnova_test]
async fn test_with_custom_db() {
    suprnova::Config::register(DatabaseConfig {
        url: "sqlite::memory:".to_string(),
        max_connections: 1,
        min_connections: 1,
        connect_timeout_secs: 5,
        logging: false,
    });

    // … your test
}
```

The `#[suprnova_test]` attribute also sets up isolated container
state so concurrent tests don't see each other's bindings — see
[Testing](testing.md).

## Common env vars Suprnova reads

A non-exhaustive list — these are vars the framework itself looks at.
Your app reads more on top.

| Var | Default | What it does |
|---|---|---|
| `APP_NAME` | `"app"` | Logged at boot, used in some default error messages |
| `APP_ENV` | `local` | Drives `Environment::detect` and `.env.<suffix>` lookup |
| `APP_DEBUG` | env-aware (`false` in production) | Verbose error pages + extra logging |
| `APP_URL` | `http://localhost:8080` | Base URL for absolute URL generation, signed URLs |
| `APP_KEY` | none (required in prod) | AES-256 key for `Crypt`, sessions, cursors |
| `APP_KEY_PREVIOUS` | none | Comma-separated previous keys for rotation (max 8) |
| `SERVER_HOST` | `127.0.0.1` | Bind address |
| `SERVER_PORT` | `8080` | Bind port |
| `DATABASE_URL` | none | Required if your app uses the database |
| `DB_MAX_CONNECTIONS` | `10` | sqlx pool max |
| `DB_MIN_CONNECTIONS` | `1` | sqlx pool min |
| `DB_CONNECT_TIMEOUT` | `30` (seconds) | sqlx pool connect timeout |
| `SESSION_LIFETIME` | `120` (minutes) | Session expiry |
| `SESSION_COOKIE` | `suprnova_session` | Cookie name |
| `SESSION_SECURE` | `true` | Set `Secure` cookie flag. Override to `false` for local-HTTP development. |
| `SESSION_SAME_SITE` | `Lax` | `Strict`, `Lax`, or `None` |
| `MAIL_DRIVER` | `log` | One of `smtp`, `ses`, `mailgun`, `postmark`, `sendgrid`, `resend`, `log`, `memory` |
| `CACHE_DRIVER` | `memory` | One of `memory`, `redis`, `database` |
| `QUEUE_DRIVER` | `memory` | One of `memory`, `redis`, `database` (unknown values warn and fall back to `memory`) |
| `RATE_LIMIT_DRIVER` | `memory` | One of `memory`, `redis` |
| `LOG_FORMAT` | env-aware (`pretty` in dev/local, `json` in production) | `pretty` or `json` |
| `LOG_LEVEL` | `info` | One of `error`, `warn`, `info`, `debug`, `trace` |

The full audited list lives in [Environment Variables](env-vars.md).

## Next

- [Application Bootstrap](bootstrap.md) — where typed config registration
  is called from
- [Service Container](container.md) — how registered config is read
  alongside bound services
- [Environment Variables](env-vars.md) — the full reference list
- [Deployment](deployment.md) — production env setup
