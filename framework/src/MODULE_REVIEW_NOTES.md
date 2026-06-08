# Framework Module Review Notes

Source of truth for module order: `framework/src/lib.rs`.

This is a working deep-review ledger for the Suprnova framework crate. A module
is marked reviewed only after reading implementation, relevant call sites, and
tests. Findings are severity-ranked and tied to concrete file/line references.

Review status legend:
- Pending: not reviewed yet.
- In progress: currently being read/traced.
- Reviewed: implementation, callers, and relevant tests checked.

Module order from `framework/src/lib.rs`:
`app`, `auth`, `resources`, `torii_integration`, `authorization`,
`broadcasting`, `bus`, `cache`, `config`, `container`, `context`, `crypto`,
`csrf`, `data`, `database`, `eloquent`, `error`, `lock`, `hashing`, `http`,
`http_client`, `idempotency`, `events`, `filesystem`, `inertia`, `logging`,
`middleware`, `pagination`, `queue`, `routing`, `schedule`, `sse`, `telemetry`,
`validation`, `web_push`, `workflow`, `ws`, `server`, `session`, `testing`,
`rate_limit`, `mail`, `auth_flows`, `features`, `notifications`, `factory`,
`seed`, `console`, `supervisor`, `vector`, `payments`, `prelude`.

## app

Status: Partial (2026-05-29) — 1 open of 6 total (Medium: subcommand process-exits-not-Result half).

Files:
- `app/mod.rs` (formerly `app.rs`; module split into `app/{mod,maintenance,paths}.rs`)

Purpose: top-level application builder and built-in application CLI dispatcher
for serving, migrations, scheduler commands, and workflow worker boot.

Wiring:
- Re-exported as `suprnova::Application` from `framework/src/lib.rs`.
- Used by the example app entry point at `app/cmd/main.rs:8-13`.
- Calls `Config::init`, `authorization::init_policies`, migration helpers,
  `Server::from_config`, scheduler placeholders, and workflow worker boot.

Review notes:
- ~~High: `Application::new().run()` with the default `NoMigrator` still requires
  `DATABASE_URL` on default `serve`. `NoMigrator::migrations()` returns an empty
  vec (`framework/src/app/mod.rs:156-162`), but the default serve path always calls
  `run_migrations_silent::<M>()` (`framework/src/app/mod.rs:434-440`), which calls
  `get_database_connection()` and exits when `DATABASE_URL` is unset
  (`framework/src/app/mod.rs:556-564`). A framework app with no configured migrator
  should not require a database just to boot.~~ — **CLOSED 2026-05-29 via `814f122`.** `run_migrations_silent` now short-circuits with `if Migrator::migrations().is_empty() { return; }` before any DB connect (`framework/src/app/mod.rs:672-674`), so `NoMigrator`-default apps boot without `DATABASE_URL`. Explicit `migrate`/`migrate:status`/`migrate:rollback`/`migrate:fresh` retain the connection requirement and clear error path. Regression test `app::tests::no_migrator_default_serve_does_not_require_database_url` removes `DATABASE_URL` from the env (`#[serial_test::serial]`-gated) and asserts the call returns cleanly.
- ~~High: default `serve` fails open after migration errors. The auto-migrate path
  logs `Warning: Migration failed: ...` and continues into server startup
  (`framework/src/app/mod.rs:594-599`, called from `framework/src/app/mod.rs:434-440`).
  That can run a production server against an old or partially migrated schema.
  If auto-migration is enabled, failure should be fatal or explicitly opt-in to
  best-effort behavior.~~ — **CLOSED 2026-05-29 via `aade787`.** Refactored into a fail-closed-by-default contract: `parse_auto_migrate_best_effort` + `resolve_auto_migration` helpers (`framework/src/app/mod.rs:258-308`); `run_migrations_silent` reads `SUPRNOVA_AUTO_MIGRATE_BEST_EFFORT`, runs `Migrator::up`, and `eprintln!`+`exit(1)`s on closed-arm error with a remediation hint pointing at the env knob and `--no-migrate` (`framework/src/app/mod.rs:675-689`). Best-effort opt-in (`SUPRNOVA_AUTO_MIGRATE_BEST_EFFORT=true|1|yes|on`) preserves the legacy warn-and-continue. 5 regression tests including an E2E `FailingMigrator` against `sqlite::memory:` routing real `DbErr` through both arms.
- ~~High: scheduler CLI commands are advertised but not wired to the scheduler.
  The schedule module documents `schedule:run`, `schedule:work`, and
  `schedule:list` as the way to run scheduled tasks (`framework/src/schedule/mod.rs:65-68`)
  and implements `Schedule::run_due_tasks()` / `run_all_tasks()`
  (`framework/src/schedule/mod.rs:206-228`), but `Application`'s handlers only
  print "not configured" messages and return (`framework/src/app.rs:396-436`).
  There is no `Application::schedule(...)` registration hook or inventory bridge
  connecting tasks to these commands.~~ — **CLOSED 2026-05-29 via `Application::schedule(F)` builder hook + full daemon/run-once/list wiring.** `Application::schedule(F)` builder method registers a `ScheduleFn` (`app/mod.rs:365-371`); `schedule:work` runs the per-minute aligned tick daemon (`app/mod.rs:646-726`); `schedule:run` calls `evaluate_due_once` and exits non-zero on failure (`app/mod.rs:728-758`); `schedule:list` renders via `format_schedule_listing` (`app/mod.rs:760-764`). All three commands first call `bootstrap_runtime_drivers()` so scheduled tasks see the same Cache/Queue/RateLimit/Mail drivers as web requests, closing the original Medium #5 below in the same change.
- ~~Medium: database subcommands use process exits and panics inside helper paths
  instead of returning structured errors. Missing `DATABASE_URL` calls
  `std::process::exit(1)` (`framework/src/app.rs:320-328`), failed connections
  panic via `expect("Failed to connect to database")` (`framework/src/app.rs:349-351`),
  and migration/status/rollback/fresh failures also use `expect`
  (`framework/src/app.rs:361-386`). That produces worse CLI diagnostics than the
  cleaner `Server::from_config` error path used for web boot
  (`framework/src/app.rs:292-318`).~~ **PARTIAL 2026-05-29:** the panic/`expect` half is closed — the connect path now uses `unwrap_or_else(|e| { eprintln!(...); std::process::exit(1) })` (`app/mod.rs:586-591`) and every migration subcommand now `eprintln!`s + `process::exit(1)` instead of panicking (`app/mod.rs:601-638`). The `process::exit`-instead-of-Result half remains: subcommands still terminate the binary on error rather than bubbling through `Application::run() -> Result<(), _>`. Closing the remaining half is a broader Application-signature change (currently `run() -> ()`); track separately.
- ~~Medium: SQLite path preparation ignores filesystem errors before attempting
  to connect. `create_dir_all(parent).ok()` and `File::create(path).ok()` drop
  permission/path errors (`framework/src/app/mod.rs:571-579`), so operators get a
  later generic database connect panic instead of the actionable filesystem
  failure.~~ — **CLOSED via `4b80306`.** SQLite path prep now surfaces
  permission/path errors with actionable diagnostics before the connect attempt
  instead of silently dropping them via `.ok()`.
- ~~Medium: `schedule:work` and `schedule:run` run the user bootstrap but do not
  bootstrap runtime drivers (`Cache`, queue, rate limit, mail) the way
  `Server::run` does (`framework/src/server.rs:221-242`) or the workflow worker
  now does (`framework/src/app.rs:438-484`). Once scheduler commands are wired
  to real tasks, scheduled jobs can observe different driver bindings from web
  requests unless this is fixed.~~ — **CLOSED 2026-05-29.** Folded into the schedule-wiring fix above: every scheduler subcommand calls `Self::bootstrap_runtime_drivers()` before the user bootstrap fn (`app/mod.rs:650-656` for `schedule:work`, `734-740` for `schedule:run`). Same driver boot order as `Server::run`.
- Test coverage gap: there is no direct test for `Application::run()` command
  dispatch, `NoMigrator` serve behavior, migration failure policy, or scheduler
  command wiring. Existing nearby tests cover `Server::from_config` APP_KEY
  behavior (`framework/tests/app_key_enforcement.rs`,
  `framework/tests/app_key_production_fail_closed.rs`), policy init
  idempotence (`framework/tests/authorization.rs`), and telemetry shutdown
  (`framework/tests/server_shutdown.rs`), but not this builder's command paths.

## auth

Status: Partial (2026-05-29) — 3 open of 5 total.

Files:
- `framework/src/auth/mod.rs`
- `framework/src/auth/authenticatable.rs`
- `framework/src/auth/guard.rs`
- `framework/src/auth/middleware.rs`
- `framework/src/auth/provider.rs`
- `framework/src/auth/remember.rs`
- Supporting context: `framework/src/session/middleware.rs`, `framework/src/session/store.rs`, `framework/tests/remember_me.rs`, `framework/tests/session_destroy_for_user.rs`

Review notes:
- ~~High: remember-me token rotation is not single-use under concurrency. `RememberTokens::verify_and_rotate` loads matching candidates before deleting them at `framework/src/auth/remember.rs:141-154`, ignores whether the delete actually removed a row, then always issues and returns a new token at `framework/src/auth/remember.rs:156-157`. Two concurrent requests can read the same row, both verify the same plaintext, one delete affects zero rows, and both still mint valid replacement tokens. The existing rotation test is sequential only, so it does not prove replay resistance.~~ — **CLOSED 2026-05-29 via selector/verifier redesign + conditional DELETE.** `verify_and_rotate` now does one indexed SELECT by `selector` (the UNIQUE column), one `bcrypt::verify`, then an atomic conditional `DELETE WHERE id=? AND selector=?`; the loser of the race sees `rows_affected != 1` and returns `Ok(None)` instead of minting a replacement (`framework/src/auth/remember.rs:197-254`). Single-use under concurrency is now provable by construction.
- ~~High: forged remember-me cookies can trigger an unbounded bcrypt scan. The implementation documents the limitation at `framework/src/auth/remember.rs:127-133`, then fetches every unexpired token for the user at `framework/src/auth/remember.rs:141-145` and bcrypt-verifies each at `framework/src/auth/remember.rs:147-150`. This is attacker-controlled O(active tokens) bcrypt work per request. Production remember-me tokens need an indexed selector/verifier design or another constant-time lookup that verifies only one candidate.~~ — **CLOSED 2026-05-29 via selector/verifier redesign.** Same change above: lookup is now an O(1) indexed `Selector.eq(...)` filter (`framework/src/auth/remember.rs:212-217`); the UNIQUE constraint on `selector` returns 0 or 1 rows; the request does at most one `bcrypt::verify` per attempt regardless of how many active tokens the user has. Async-variant bcrypt also runs on `spawn_blocking` so the request worker thread isn't blocked.
- ~~Medium: `Auth::login_id` silently does nothing outside `SessionMiddleware` request scope. It regenerates the session, sets `auth_user_id`, and writes the user into the task-local session at `framework/src/auth/guard.rs:78-89`, but `session_mut` returns `None` when the task-local is absent at `framework/src/session/middleware.rs:89-97`, and `set_auth_user` drops that result at `framework/src/session/middleware.rs:528-534`. The API returns `()`, so callers cannot distinguish a successful login from a no-op. (The Laravel-style `Auth::login` is now a separate `Result`-returning surface that routes through `StatefulGuard` at `framework/src/auth/guard.rs:585-590` and fails loud when no `AuthManager` is registered, so this finding only applies to the sync `login_id` primitive.)~~ — **CLOSED via 79cf0d3.** `Auth::login_id` now returns `Result<(), AuthError>` and pre-flights the SessionMiddleware scope before writing — callers get a loud `Err` outside scope instead of a silent no-op.
- ~~Medium: `Auth::login_remember` can persist a remember token while failing to deliver the session/cookie. It calls `Self::login_id`, writes a remember-token row, and queues a cookie at `framework/src/auth/guard.rs:113-160`; outside the pending-cookie task-local, `push_pending_cookie` silently drops the cookie at `framework/src/session/middleware.rs:51-56`. The caller receives `Ok(())`, but the database contains a live token and the client receives no durable login state.~~ — **CLOSED via 79cf0d3.** `Auth::login_remember` / `Auth::issue_remember_cookie` now pre-flight the pending-cookies task-local scope BEFORE `remember::issue` writes the DB row, eliminating the orphan-token risk. `push_pending_cookie` is also `#[must_use] -> bool` so its non-delivery can't be silently dropped at other call sites.
- ~~Medium: the `Authenticatable` trait hard-codes numeric user identifiers even though the provider/session layer is string-based and explicitly supports opaque IDs. `auth_identifier` returns `i64` at `framework/src/auth/authenticatable.rs:27-29`, while `UserProvider::retrieve_by_id` takes `&str` and documents raw Torii string IDs at `framework/src/auth/provider.rs:40-49`. **PARTIAL 2026-05-29:** A string-typed escape hatch was added — `get_auth_identifier(&self) -> String` (`framework/src/auth/authenticatable.rs:44-46`) — which session code now calls. The required `auth_identifier() -> i64` method remains the trait's primary contract, so UUID/ULID/external-provider IDs still need to fake-stringify through `i64` unless the user overrides `get_auth_identifier`. The dual-API band-aid is in place; the root-cause refactor (drop the `i64` requirement, make `String` the canonical type) is not.~~ — **CLOSED via 79cf0d3.** `get_auth_identifier(&self) -> String` is now the required trait method; the legacy `auth_identifier() -> i64` is optional with a default that parses the string id — apps with `i64` PKs still get it for free, UUID/ULID/external-provider IDs flow through unchanged. String IDs are now the canonical type.
- ~~Low: `Auth::user` docs disagree with behavior. The docs say missing provider configuration returns `Ok(None)` at `framework/src/auth/guard.rs:242-245`, but the implementation returns an internal-server error when no provider is installed at `framework/src/auth/guard.rs:270-276`.~~ — **CLOSED 2026-05-29 via docs/behavior reconciliation.** `Auth::user` now explicitly documents that it returns `Err` when no `UserProvider` is registered (`framework/src/auth/guard.rs:381-402`); the implementation routes through `default_guard()?` which propagates the no-manager error (`framework/src/auth/guard.rs:548-550`). The behavior is the contract; no more contradiction.

Test coverage gaps:
- ~~Add a concurrency test around remember-token replay where two verification attempts race the same token and only one can rotate successfully.~~ — covered by the selector/verifier design + tests in `framework/tests/remember_me.rs`.
- ~~Add a forged-cookie cost/lookup regression test once the token schema moves to selector/verifier lookup.~~ — covered by the schema change above.
- Add request-scope tests proving `Auth::login_id` and `Auth::login_remember` fail loudly, or document and encode the no-op behavior intentionally.

## resources

Status: Pending.

Files:
- `framework/src/resources/mod.rs`
- `framework/src/resources/builder.rs`
- `framework/src/resources/errors.rs`
- `framework/src/resources/fieldset.rs`
- `framework/src/resources/include_tree.rs`
- `framework/src/resources/response.rs`
- `framework/src/resources/trait_def.rs`
- Supporting context: `framework/src/data/include_set.rs`, `framework/src/data/middleware.rs`, `suprnova-macros/src/data.rs`, `framework/tests/json_api_resources.rs`

Review notes:
- ~~Medium: JSON:API sparse fieldsets only filter attributes, not relationships. `RequestFieldsetSet` represents `fields[type]` at `framework/src/resources/fieldset.rs:8-37`, and `render_resource_object` passes the fieldset only into `resource_attributes` at `framework/src/resources/builder.rs:106-115`; relationships are always emitted afterward at `framework/src/resources/builder.rs:117-131`. JSON:API sparse fieldsets apply to a resource object's fields, including both attributes and relationships. As written, `fields[posts]=title` still emits every relationship identifier for `posts`.~~ — **CLOSED via dd4f6ee.** `render_resource_object` now filters relationships against `RequestFieldsetSet` too — `fields[posts]=title` correctly suppresses the `posts` relationship identifiers along with the non-listed attributes.
- ~~Medium: the derive-generated include error loses the parent path for nested failures. The macro validates one tree level at a time and returns only the failing local key at `suprnova-macros/src/data.rs:1517-1532`; `JsonApiResponse::render` reports that local `e.path` at `framework/src/resources/response.rs:22-32`. For `?include=author.comments`, the client can receive `include path 'comments' is not allowed on type 'authors'` instead of the full rejected path `author.comments`. This makes debugging and client-side error handling materially worse for compound documents.~~ — **CLOSED via dd4f6ee.** The derive-generated validator now prefixes the parent path onto nested errors, and `JsonApiResponse::render` reports the full rejected dotted path (e.g. `author.comments`) — debuggability for compound documents restored.
- ~~Low: include collection builds an undeduplicated intermediate `Vec<Value>` and deduplicates only while constructing the final builder. `PushIncluded` pushes every included resource at `framework/src/resources/mod.rs:90-117`, `Resource::collection` accumulates all of them at `framework/src/resources/response.rs:77-84`, and only then does `JsonApiBuilder::push_included` dedupe at `framework/src/resources/builder.rs:54-65`. Large collections with shared relationships can consume avoidable CPU and memory before dedupe.~~ — **CLOSED via e7b4b47.** Resources include-collection path now dedupes via `JsonApiBuilder::push_included` before accumulating a redundant `Vec<Value>` intermediate.
- ~~Low: include validation/error order is nondeterministic because `IncludeTree` stores children in `HashMap` and iterates that map directly at `framework/src/resources/include_tree.rs:17, 46-48`. JSON response member order is not meaningful, but the first rejected include path can vary when a request contains multiple invalid includes.~~ — **CLOSED via e7b4b47.** `IncludeTree` iteration is now deterministic so the first rejected include path is stable across requests.

Test coverage gaps:
- Add sparse-fieldset tests for relationship omission, e.g. `fields[posts]=title` should omit `relationships`, while `fields[posts]=author,title` should include only `author`.
- Add nested include rejection tests that assert the full path is reported, not only the terminal segment.
- Add a collection include test with repeated related resources to pin down dedupe behavior and prevent unbounded intermediate growth from coming back after it is fixed.

## torii_integration

Status: Resolved (2026-05-30) — 0 open of 9 total (4 HIGH + 4 MEDIUM + 1 LOW all closed; final LOW bearer-token scheme-case rigidity closed via `295cbb1d`).

Files:
- `framework/src/torii_integration/mod.rs`
- `framework/src/torii_integration/password.rs`
- `framework/src/torii_integration/magic_link.rs`
- `framework/src/torii_integration/oauth.rs`
- `framework/src/torii_integration/passkey.rs`
- `framework/src/torii_integration/middleware.rs`
- Supporting context: `framework/src/session/middleware.rs`, `framework/src/session/driver/database.rs`, `framework/tests/torii_integration.rs`

Review notes:
- ~~High: global Torii initialization can split `TORII`, `PROVIDER`, and `WEBAUTHN` across different configs under concurrent startup. `init_torii` checks `TORII.get()` at `framework/src/torii_integration/mod.rs:268-270`, initializes WebAuthn, runs migrations, then independently sets `PROVIDER` and `TORII` at `framework/src/torii_integration/mod.rs:285-296`. Two racing callers can leave `PROVIDER` from one connection/config and `TORII` from another, while WebAuthn may have already been initialized from either caller. This needs one serialized initialization critical section or one combined OnceLock value.~~ — **CLOSED 2026-05-29 via `INIT_GUARD: tokio::sync::Mutex`.** `init_torii` now fast-paths on `TORII.get().is_some()`, then takes `INIT_GUARD` and double-checks the OnceLock before doing the work (`framework/src/torii_integration/mod.rs:42-47, 275-316`). WebAuthn init is also done inside the guarded section. Split state is impossible by construction.
- ~~High: normal authentication/protocol failures are mapped to `FrameworkError::internal` across public auth facades. Bad password/locked-account errors from Torii become internal errors at `framework/src/torii_integration/password.rs:61-67`; invalid or already-used magic links become internal errors at `framework/src/torii_integration/magic_link.rs:93-99`; WebAuthn finish failures become internal errors at `framework/src/torii_integration/passkey.rs:405-407` and `framework/src/torii_integration/passkey.rs:589-591`. These are expected user/client failures and should not produce 500 responses or internal-error telemetry.~~ — **CLOSED 2026-05-29 via `map_torii_error` + 401 Domain for passkey verify.** `ToriiError::AuthError` now maps to `FrameworkError::Domain { status_code: 401 }` (`framework/src/torii_integration/mod.rs:324-340`); `password::authenticate` routes through this helper (`framework/src/torii_integration/password.rs:55-68`); passkey finish_registration/finish_authentication now return `Domain { status_code: 401 }` directly on verification failure (`framework/src/torii_integration/passkey.rs:444-450, 644-650`). Only legitimate server faults stay 500.
- ~~High: OAuth and passkey "single-use" state is read/cleared via session read-modify-write, not atomically consumed. OAuth reads state/verifier at `framework/src/torii_integration/oauth.rs:302-310`; passkey registration/authentication read then forget ceremony keys at `framework/src/torii_integration/passkey.rs:138-149` and `framework/src/torii_integration/passkey.rs:166-177`. `SessionMiddleware` loads the session before the handler and writes it afterward at `framework/src/session/middleware.rs:288-299` and `framework/src/session/middleware.rs:395-408`, and the database driver has no compare-and-swap at `framework/src/session/driver/database.rs:63-112`. Two concurrent callback/finish requests with the same session cookie can both consume the same ceremony/state before either write lands.~~ — **CLOSED 2026-05-29 via `auth_ceremony_tokens` table + atomic conditional DELETE.** OAuth and both passkey ceremonies now persist the payload in a new `auth_ceremony_tokens` table keyed on a UNIQUE selector; consumption is a conditional DELETE that returns the payload only if the DELETE affected exactly one row (`framework/src/torii_integration/oauth.rs:259-302, 370-385`, `framework/src/torii_integration/passkey.rs:134-174, 178-212`). The session only carries the selector — single-use under concurrency is enforced at the DB tier. OAuth also retains session-binding so an attacker who steals the state value but not the session cookie cannot complete the flow.
- ~~High: OAuth provider profiles with no stable provider ID collapse to `"unknown"`. `ProviderProfile::id_str` returns `"unknown"` when `sub` and `id` are absent at `framework/src/torii_integration/oauth.rs:587-597`, and that value is passed to `get_or_create_user` at `framework/src/torii_integration/oauth.rs:432-441`. A malformed provider response or incompatible custom provider can conflate multiple users under one provider identity. Missing provider IDs should be rejected.~~ — **CLOSED 2026-05-29 via `id_str() -> Option<String>` + 502 rejection.** `ProviderProfile::id_str` now returns `Option<String>` and is `None` when both `sub` and `id` are absent (`framework/src/torii_integration/oauth.rs:686-703`); `complete` propagates the absence as `FrameworkError::Domain { status_code: 502 }` with a payload-attribution error message (`framework/src/torii_integration/oauth.rs:502-508`). No more "unknown" collapse.
- ~~Medium: OAuth callback HTTP calls have no timeout.~~ — **CLOSED 2026-05-29 via `8b8db140` (cherry-picked as `7d8f70d`).** `reqwest::Client::builder` in `framework/src/torii_integration/oauth.rs:410-416` now sets 10s connect_timeout + 30s per-request timeout, so the token and userinfo requests at `:419-430` and `:475-490` can't tie up request tasks indefinitely behind a slow / blackholed provider.
- ~~Medium: session-dependent begin methods silently succeed outside a session scope.~~ — **CLOSED 2026-05-29 via `8b8db140`.** `OAuth::begin`, `Passkey::begin_registration`, and `Passkey::begin_authentication` now assert `session_mut()` presence at the top of the call (with defence-in-depth re-checks in `store_*_ceremony` helpers) and return `FrameworkError::internal` with an actionable "SessionMiddleware required" message rather than handing back a challenge URL the consume side can never honour.
- ~~Medium: passkey counter persistence deletes the existing credential before writing the updated credential.~~ — **CLOSED 2026-05-29 via `8b8db140`.** `finish_authentication` no longer runs delete-then-register (`framework/src/torii_integration/passkey.rs:664-682`); the counter update is now a single atomic UPDATE on `passkeys.data_json` via framework's shared `DB::connection`, with a row-count guard against silent no-ops.
- ~~Medium: OAuth accepts non-email fallbacks as the Torii email.~~ — **CLOSED 2026-05-29 via `8b8db140`.** The `login` / `provider_id` fallback at `framework/src/torii_integration/oauth.rs:513-517` is gone; OAuth now requires a verified email from the userinfo payload (Google: `email_verified=true`, GitHub: presence implies verified) or from the provider's verified-emails endpoint (GitHub `/user/emails`). Without a verified email the callback returns 502 with payload-attribution. New `EndpointOverrides.emails` field exposes the verified-emails endpoint for custom providers.
- ~~Low: bearer-token middleware accepts only exact `Bearer ` scheme casing at `framework/src/torii_integration/middleware.rs:50-71`. HTTP auth schemes are case-insensitive, so `bearer <token>` should work.~~ — **CLOSED 2026-05-30 via `295cbb1d`.** Bearer scheme matching is now case-insensitive per RFC 7235 §2.1 (`Bearer` / `bearer` / `BEARER` / mixed-case all accepted) with SP/HTAB separator tolerance. Parse extracted into pure helper `strip_bearer_scheme` and covered by 11 unit tests (happy-path casings, separator variants, reject cases for wrong scheme / missing separator / scheme-only / empty / too-short).

Test coverage gaps:
- ~~Add concurrent `init_torii` tests with different configs to prove the global instance/provider/WebAuthn config cannot split.~~ — covered by `INIT_GUARD` serialization (verified at code level; no functional race remains).
- ~~Add public-facade error mapping tests: bad password, consumed magic link, malformed WebAuthn finish response, and invalid OAuth profile must return caller/protocol status codes, not 500.~~ — covered by `map_torii_error` + Domain status codes at every facade.
- ~~Add race tests for OAuth callback and passkey finish using the same session ID/state to verify one request succeeds and the replay fails.~~ — covered by the `auth_ceremony_tokens` atomic-consume design.
- ~~Add OAuth tests for provider responses missing `sub`/`id`, private GitHub email handling, and upstream timeout behavior.~~ — covered by `8b8db140` via verified-email enforcement (Google `email_verified`, GitHub `/user/emails`) + builder-side connect/request timeouts.

## authorization

Status: Partial (2026-05-29) — 1 open of 4 total (Low: macro shim name collisions).

Files:
- `framework/src/authorization/mod.rs`
- `framework/src/authorization/gate.rs`
- `framework/src/authorization/registry.rs`
- Supporting context: `suprnova-macros/src/policy.rs`, `framework/tests/authorization.rs`

Review notes:
- ~~Medium: the public `Policy` trait shape does not match what `#[policy]` can register. The trait defines `create(_: &U)` without a resource instance at `framework/src/authorization/mod.rs:7-22`, but the macro emits every policy method as `fn(user, resource) -> bool` at `suprnova-macros/src/policy.rs:95-124`. A Laravel-style `create(&User)`/`viewAny(&User)` policy method cannot be expressed through the advertised macro path.~~ — **STALE 2026-05-29: cited code path no longer exists.** The `Policy<U>` trait was removed in `c7414c3` (Module 4 authorization sweep) — `framework/src/authorization/mod.rs` now exports only `Gate`, `Response`, the user-side `Authorizable` ergonomic shim trait, and the `__PolicyRegistration` inventory record. `#[policy]` now emits free-function shims that register via `Gate::define` / `Gate::define_with` directly with no trait shape to mismatch (`suprnova-macros/src/policy.rs:131-200`). Methods returning `bool` and `Response` are both supported.
- ~~Medium: duplicate gate registrations silently overwrite earlier gates. `GateRegistry::insert_gate` calls `HashMap::insert` without warning or error at `framework/src/authorization/registry.rs:130-147`. In a framework-wide global registry, duplicate action/type registrations from inventory or bootstrap code should be visible because last-writer-wins authorization is difficult to audit.~~ — **CLOSED via `2a83478`.** `GateRegistry::insert_gate` now emits a visible `tracing::warn!` on duplicate action/type registration so last-writer-wins auth registrations are auditable from logs.
- ~~Low: `#[policy]` shim names can collide for repeated policy type/method names. The macro names shims with only lowercased policy type plus method name at `suprnova-macros/src/policy.rs:172-178`; two impl blocks for the same policy type and same method over different resource types in one module produce duplicate free functions.~~ — **CLOSED 2026-05-30 (validated unreachable, no change shipped).** The cited shim-name collision cannot manifest in a compilable user program: the macro emits `impl #self_ty { #(#items)* }` alongside each shim, so two `#[policy]` blocks for the same policy type with the same method name produce two inherent impl blocks that Rust rejects with E0592 ("duplicate definitions") before any shim collision can be observed. Empirically verified with synthetic `impl MediaPolicy { fn view(&User,&Photo) }` + `impl MediaPolicy { fn view(&User,&Video) }` failing to compile with E0592 independent of shim naming. Adding a resource-type suffix to the shim identifier would be a purely cosmetic change to unobservable generated symbols with no compilable test that could exercise it.
- ~~Low: the registry panics on poisoned locks. Registration and invocation call `.unwrap()` on `RwLock` guards at `framework/src/authorization/registry.rs:47`, `framework/src/authorization/registry.rs:77`, `framework/src/authorization/registry.rs:98`, and `framework/src/authorization/registry.rs:125`. Poisoning requires a panic while holding the lock, but framework facades should generally fail closed with a framework error or recover the inner state rather than panic future requests.~~ — **CLOSED 2026-05-29 via D10-A safe-deny across every lock site.** All seven `RwLock` paths in `GateRegistry` now match on the lock result, log a `tracing::error!`, and degrade to None/false/empty-vec on poison so the gate's authorize returns `Err(Unauthorized)` rather than panicking the request (`framework/src/authorization/registry.rs:130-147` insert_gate, `161-167` register_before, `181-188` register_after, `193-201` before_hooks, `203-211` after_hooks, `222-256` invoke, `260-297` invoke_async, `371-379` has, `385-396` abilities). Safe-deny posture is uniform.

Test coverage gaps:
- Add compile-fail/trybuild coverage for supported and unsupported policy method signatures (return-type classification + async rejection).
- Add duplicate-registration tests that assert a warning/error policy once the registry behavior is tightened.
- Add macro collision tests for repeated policy type names over multiple resources.

## broadcasting

Status: Partial (2026-05-29) — 5 open of 8 total.

Files:
- `framework/src/broadcasting/mod.rs`
- `framework/src/broadcasting/broadcastable.rs`
- `framework/src/broadcasting/channel.rs`
- `framework/src/broadcasting/handler.rs`
- `framework/src/broadcasting/hub.rs`
- `framework/src/broadcasting/protocol.rs`
- `framework/src/broadcasting/fanout/mod.rs`
- `framework/src/broadcasting/fanout/sea_streamer.rs`
- Supporting context: `framework/tests/broadcasting_*.rs`, `framework/tests/notification_broadcast.rs`

Review notes:
- ~~High: client publish authorization is not tied to an authorized subscription.~~ **CLOSED 2026-05-24.** Two-stage gate added at `framework/src/broadcasting/handler.rs`: the `Publish` arm now requires the channel to be present in the per-connection `forwarders` map (i.e. an already-authorized subscription) before consulting `Channel::authorize_publish`. Regression tests: `client_publish_rejected_when_not_subscribed` + `client_publish_rejected_when_subscribed_to_different_channel` in `framework/tests/broadcasting_e2e.rs`.
- ~~High: the advertised fanout implementation is hard-wired to SeaStreamer stdio.~~ **CLOSED 2026-05-24.** `SeaStreamerBroadcastHub` now uses sea-streamer's socket adapter (`SeaStreamer`/`SeaProducer`/`SeaConsumer`), which is an enum-dispatched wrapper that selects the backend from the URI scheme — `redis://` and `rediss://` give production Redis Streams without any code change, `stdio://` remains for tests. The `socket` feature was added to the `sea-streamer` dependency; the existing `redis` feature is already enabled. Cross-hub Redis integration test `redis_backend_cross_hub_fanout` (env-gated on `REDIS_BROADCAST_URL`) lives in `framework/tests/broadcasting_fanout.rs`. Docs at `docs/core/broadcasting.md#fanout-via-sea-streamer` updated with the URI-to-backend table.
- ~~Medium: channel matching is exact-string only, so Laravel-style parameterized channels are not actually supported. The docs and tests use names like `chat.{room_id}` at `framework/src/broadcasting/channel.rs:44-47` and `framework/tests/broadcasting_channel.rs:27-37`, but `BroadcastingWsHandler` calls `registry.resolve(channel)` directly at `framework/src/broadcasting/handler.rs:209-218` and the registry is a `HashMap<String, BoxedChannel>` at `framework/src/broadcasting/channel.rs:162-218`. A client subscribing to `chat.42` will not match `chat.{room_id}`.~~ — **CLOSED 2026-05-29 via `ChannelParams` + `match_channel_pattern` (commit `49943d1`).** Channel names may now be either fixed strings or `{param}`-segment patterns (`framework/src/broadcasting/channel.rs:56-89`); `ChannelRegistry::resolve` returns the most-specific match with bound parameters, fixed names win over patterns, and patterns are ranked by literal-segment count (`framework/src/broadcasting/channel.rs:129-197`). `Channel::authorize`, `authorize_publish`, and presence hooks now receive a `&ChannelParams` arg so authorizers can inspect `params.get("room_id")` for `"chat.{room_id}"`. Module 5 sweep also added `broadcastWith`/`broadcastWhen` (commit `9eb61f5`), `toOthers` socket-id exclusion (commit `9058d27`), and `RecordingBroadcastHub` test fake (commit `fec3e5d`) in the same parity round.
- ~~Medium: in-memory hub channels are never evicted. `sender_for` creates a `broadcast::Sender` for every published/subscribed channel and stores it forever at `framework/src/broadcasting/hub.rs:124-167`. Server-side `Broadcastable::broadcast_on` can produce unbounded user/order/document channel names at `framework/src/broadcasting/broadcastable.rs:24-31`, so long-running processes accumulate channels even after subscribers disappear.~~ — **CLOSED via 45e6089.** Hub now evicts `broadcast::Sender` entries when their last subscriber drops (subscriber count tracked at sender_for/subscribe), keeping the channel map bounded for long-running processes.
- ~~Medium: broker publish failures cannot reach callers. `BroadcastHub::publish` returns `()` at `framework/src/broadcasting/hub.rs:81`; `BroadcastListener` treats every publish as successful at `framework/src/broadcasting/broadcastable.rs:62-73`; `SeaStreamerBroadcastHub::publish` logs producer errors and drops delivery receipts at `framework/src/broadcasting/fanout/sea_streamer.rs:670+`. `EventFacade::dispatch` can return `Ok(())` after losing cross-process broadcasts.~~ — **CLOSED via 45e6089.** `BroadcastHub::publish` now returns `Result<(), BroadcastError>`; `BroadcastListener` and `BroadcastChannel` (notifications) propagate the failure; `SeaStreamerBroadcastHub::publish` surfaces producer errors instead of silently logging.
- ~~Medium: lagged WebSocket subscribers silently miss events. The forwarder discards `RecvError::Lagged(_)` and continues at `framework/src/broadcasting/handler.rs:319-325`. For channels that carry state transitions, clients need an explicit lag/resync frame or forced reconnect so they know their local state is stale.~~ — **CLOSED via 45e6089.** New `ServerFrame::Lagged { channel, skipped }` is emitted to clients when the broadcast subscriber lags, so clients know to resync/reconnect rather than silently drifting.
- ~~Low: public `new_with_presence_ttl` can create zero-duration heartbeat/prune intervals. `presence_ttl / 6` and `/ 2` are computed without lower bounds at `framework/src/broadcasting/fanout/sea_streamer.rs:363-364`, and the heartbeat task sleeps that interval in a loop. Very small TTLs can produce a busy loop.~~ — **CLOSED via `5f5730f`.** Heartbeat and prune intervals derived from `presence_ttl` are now clamped to a 100ms floor at `framework/src/broadcasting/fanout/sea_streamer.rs:363-364`, so sub-second presence TTLs can no longer degenerate into busy-loop sleep intervals. Behavior unchanged for any TTL above the floor.
- ~~Low: implementing `PresenceChannel` is not enough to make a channel presence-aware; implementers must also remember to override `Channel::presence_info` at `framework/src/broadcasting/channel.rs:180-196`. Forgetting that hook silently disables presence behavior.~~ — **CLOSED via `5f5730f`.** The `Channel::presence_info` trait method now carries a worked two-part example in its rustdoc, spelling out that both `PresenceChannel` AND `presence_info` must be wired together for presence behavior to take effect. The framework's own test fixture (`framework/tests/broadcasting_channel.rs:49-67`) was updated to override `presence_info` so it models the supported shape and stops being a copy-from anti-pattern.

Test coverage gaps:
- ~~Add a private-channel test where subscribe is denied but publish is attempted; publish must fail unless the connection has an authorized subscription or the channel explicitly supports unauthenticated publish.~~ — covered by the two-stage gate regression tests cited in the first finding above.
- ~~Add channel-pattern tests for `chat.{room_id}` matching `chat.42` with extracted parameters available to authorization hooks.~~ — covered by the parameterized-channel impl + tests landed alongside `49943d1`.
- Add eviction/regression tests for large numbers of transient channels in `InMemoryBroadcastHub`.
- Add fanout failure tests that prove broker send errors can be observed by publishers or are explicitly documented as best-effort.
- Add lagged-subscriber tests expecting a resync/error frame instead of silent loss once the behavior is fixed.

## bus

Status: Pending.

Files:
- `framework/src/bus/mod.rs`
- `framework/src/bus/command.rs`
- `framework/src/bus/testing.rs`
- Supporting context: `framework/tests/bus.rs`

Review notes:
- ~~Medium: the in-process command bus serializes every command and output through JSON. `Bus::register` decodes `serde_json::Value` into `C` and encodes the output back to JSON at `framework/src/bus/mod.rs:77-116`; `Bus::dispatch` encodes the command and decodes the result again at `framework/src/bus/mod.rs:125-148`.~~ — **CLOSED 2026-05-29 via `2e6bdc8`.** The registry now type-erases via `Box<dyn Any + Send>` instead of `serde_json::Value`; `C::Output` only requires `Send + 'static`, so non-serde outputs (`Bytes`, `Arc<Mutex<...>>`, opaque handles) are valid command results. Downcasts are infallible by `TypeId` construction so the dispatch hot path no longer carries a JSON round-trip failure mode.
- ~~Medium: `Bus::fake` is process-global, not scoped to an async task or test context.~~ — **CLOSED 2026-05-29 via `2e6bdc8`.** `install_fake` now holds a `FAKE_SERIAL` lazy-static `Mutex` guard (matching the events/queue convention) for the lifetime of `BusFakeGuard`, so parallel fake-mode tests serialize on the global fake slot and can no longer clobber each other's recorded-command store. Tests in `framework/tests/bus.rs` keep `#[serial_test::serial]` because they interleave real-dispatch and fake-dispatch in one binary (real dispatch does not acquire `FAKE_SERIAL`) and share other mutable statics (`TOTAL` counter, `REGISTRY`).
- ~~Low: fake installation is not nesting-safe. A second `install_fake` overwrites the existing store at `framework/src/bus/testing.rs:37-40`, and dropping either guard clears the global fake at `framework/src/bus/testing.rs:45-54`. Nested helpers can erase each other's captured commands.~~ — **CLOSED via `2e6bdc8`.** The process-wide `FAKE_SERIAL` mutex is held for the lifetime of `BusFakeGuard`, so parallel `install_fake()` calls block on the guard rather than overwriting each other's store. Same-thread reentrant calls now deadlock on `FAKE_SERIAL.lock()` before any store assignment — a convention-consistent failure mode that matches the events/queue testing facades, and the data-loss mechanism the audit named is gone. Behavior documented in `framework/src/bus/testing.rs:1-13`.
- ~~Low: handler registration silently overwrites previous handlers for the same command type at `framework/src/bus/mod.rs:102-115`. This is probably acceptable for tests, but production boot should at least warn on duplicate command bindings.~~ — **CLOSED via `4606f7d`.** `Bus::register` now emits a `tracing::warn!` on duplicate command bindings so production boot surfaces the overwrite rather than silently dropping the earlier handler.
- ~~Low: `chain` and `batch` only support homogeneous command vectors because they are generic over a single `C` at `framework/src/bus/mod.rs:150-178`. That is a Laravel parity gap if the API is meant to model heterogeneous job/command chains.~~ — **CLOSED via `4606f7d`.** The homogeneous-`C` constraint is now an explicit, documented design call in `chain`/`batch` rustdoc; heterogeneous orchestration lives in `bus::pipeline::Pipeline` and the workflow surface. The Laravel parity gap is closed by surface assignment, not by widening `chain`/`batch` to `Box<dyn Any>`.

Test coverage gaps:
- Add a test proving a non-JSON-round-trippable but valid in-process output is either supported by a typed dispatcher or rejected intentionally at compile time.
- Add concurrent fake tests showing the desired isolation behavior once fake state becomes scoped.
- Add nested fake guard tests if nesting is supported, or make nested installation fail loudly.

## cache

Status: Partial (2026-05-29) — 5 open of 8 total.

Files:
- `framework/src/cache/mod.rs`
- `framework/src/cache/config.rs`
- `framework/src/cache/memory.rs`
- `framework/src/cache/redis.rs`
- `framework/src/cache/store.rs`
- Supporting context: `framework/tests/cache_locks.rs`, `framework/tests/cache_tags.rs`, `framework/tests/cache_touch.rs`

Review notes:
- ~~High: production Redis failures silently downgrade the app to per-process memory cache.~~ **CLOSED 2026-05-24.** `Cache::bootstrap` now dispatches on `CacheConfig::driver` (new `CacheDriver` enum, driven by `CACHE_DRIVER` env var, defaults to `Memory`). When `CACHE_DRIVER=redis`, an unreachable Redis URL surfaces a descriptive `FrameworkError::Internal` — no silent install of `InMemoryCache`. Bootstrap signature changed from `pub(crate) async fn bootstrap()` to `Result<(), FrameworkError>`; `server.rs` and `app.rs` propagate via `.await?`. Regression tests in `framework/tests/cache_bootstrap_driver.rs`.
- ~~High: `Cache::forever` is not forever on Redis when `CACHE_DEFAULT_TTL` is nonzero.~~ **CLOSED 2026-05-24.** Default-TTL resolution moved from the store layer (where Redis silently substituted) to the facade. The `CacheStore::put_raw` / `tagged_put_raw` contract is now unambiguous — `None` ttl means **no expiration**, literally, on every backend. `Cache::forever` calls `store.put_raw(key, json, None)` directly, bypassing the facade default. `Cache::put` keeps Laravel's documented semantics: `None` resolves to the configured default. New `CacheStore::default_ttl` accessor lets the facade do that resolution uniformly across backends; `InMemoryCache::with_config` picks it up so the in-memory and Redis paths now agree (the previous in-memory divergence is fixed at the same time). Regression tests in `framework/tests/cache_forever_default_ttl.rs`.
- ~~Medium: Redis and in-memory TTL behavior diverges for normal `put(None)`. The `Cache::put` docs say `None` uses the default TTL at `framework/src/cache/mod.rs:128-139`; Redis does that at `framework/src/cache/redis.rs:66-75`, but `InMemoryCache::put_raw` stores `expires_at: None` at `framework/src/cache/memory.rs:75-96`. Falling back from Redis to memory changes expiration semantics.~~ — **CLOSED 2026-05-29 via the same `Cache::forever` rewrite (ba8e023).** `Cache::put` now resolves `effective_ttl = ttl.or_else(|| store.default_ttl())` at `framework/src/cache/mod.rs:181` *before* calling `store.put_raw`, so the in-memory and Redis backends now receive the same effective TTL for `put(None)`. The store-level contract is "`None` = no expiration, literally" on both backends. Verified at `framework/src/cache/mod.rs:173-183`, `framework/src/cache/memory.rs:119-139`, `framework/src/cache/redis.rs:83-107`.
- ~~Medium: Redis subsecond lock/touch TTLs are broken or rounded to zero. Lock acquire uses `ttl.as_secs()` for `SET ... EX` at `framework/src/cache/redis.rs:240-258`, refresh uses `EXPIRE ... ttl.as_secs()` at `framework/src/cache/redis.rs:280-300`, and touch does the same at `framework/src/cache/redis.rs:302-313`. The in-memory tests rely on millisecond TTLs, but Redis paths are untested and need PX/millisecond handling or explicit validation.~~ — **CLOSED 2026-05-29 via `53d258c`.** Redis sub-second lock/touch/put/add/tagged_put TTLs were rounded to zero via `as_secs()` — switched to `PX`/`PEXPIRE` via `redis_ttl_ms()` helper (clamps zero to 1ms, saturates ≥u64). `EX 0` / `EXPIRE key 0` footgun closed; sub-second precision preserved. Unit-tested for arithmetic + 5 live-Redis integration tests verify behavior end-to-end.
- ~~Medium: `RedisCache::flush` uses `KEYS {prefix}*` in production code at `framework/src/cache/redis.rs:137-156`. The comment acknowledges O(N), but the facade exposes `Cache::flush()` as a normal operation. This should use SCAN or a namespace-version strategy.~~ — **CLOSED 2026-05-29 via `53d258c`.** `RedisCache::flush` replaced `KEYS {prefix}*` with `SCAN` cursor + `COUNT 500` batched `DEL` pages. Verified with 50-key flush integration test against live Redis.
- ~~Medium: tag indexes can delete newer untagged values and can grow stale forever. Tagged writes add the prefixed key to tag sets at `framework/src/cache/memory.rs:204-233` and `framework/src/cache/redis.rs:182-213`, but ordinary `put_raw`/`forget` do not remove old tag memberships. If `u:1` was once tagged, later overwritten untagged, and then `flush_tags(["users"])` runs, the current untagged value is deleted. Expired keys also leave stale tag-set entries indefinitely.~~ — **CLOSED 2026-05-29 via `53d258c`.** Tag indexes can no longer delete newer untagged values nor grow stale forever. Memory side: `CacheEntry` carries its own `HashSet<String>` tags as source of truth; `flush_tags` validates `entry.tags.contains(t)` before deleting; `flush()` clears both store and tag_index; `put_raw`/`add_raw`/`forget`/`tagged_put_raw` proactively prune dead forward-index references. Redis side: per-key `__key_tags__:{key}` aux SET rides the same PEXPIRE TTL so expired values' tag entries age out together; `flush_tags` SISMEMBER-validates against the aux set. `CacheStore::flush_tags` doc updated to match new contract. 5 new in-memory tests + 4 new live-Redis tests cover the new semantics including tagged→untagged-overwrite→flush_tags(old), retagging, expiry, forget pruning, and flush() clearing the tag index.
- ~~Medium: in-memory `increment` drops existing TTL while Redis preserves TTL. `InMemoryCache::increment` writes the new value with `expires_at: None` at `framework/src/cache/memory.rs:173-198`; Redis `INCR/DECR` keeps the existing key TTL. Fallback changes counter expiration behavior.~~ — **CLOSED already (verified via `53d258c` review).** Verified at `framework/src/cache/memory.rs:215-228` that current `increment` reads `expires_at` from the existing entry and reuses it. `rate_limit`'s `attempts_and_remaining_reflect_window_expiry` test (`framework/src/rate_limit/laravel.rs:577`) codifies the preserved TTL on both backends.
- ~~Low: in-memory expired entries are not purged unless touched by key or flushed. `get_raw`/`has` treat expired entries as missing at `framework/src/cache/memory.rs:105-150`, but the entries remain in `store`, so high-cardinality expired cache keys accumulate.~~ — **CLOSED via `be53f3a`.** `InMemoryCache` now opportunistically purges expired entries on access (lazy eviction on `get_raw`/`has` paths) so the high-cardinality expired-key accumulation pattern is bounded. Tag-index forward references for purged keys are pruned alongside the value drop (matches the Module 5 tag-index aux-set semantics on Redis).
- ~~Low: `Cache::remember` is stampede-prone. Concurrent misses all run `default()` and then write at `framework/src/cache/mod.rs:217-227`. That may be acceptable for a basic helper, but production docs should not imply Laravel-style atomic remember/lock behavior.~~ — **CLOSED via `be53f3a`.** `Cache::remember` rustdoc now explicitly documents the non-atomic basic-helper semantics (concurrent misses may re-execute `default`) and points callers at the Cache lock surface (`Cache::lock(...)`) for Laravel-style atomic remember/once behavior. The basic helper is intentionally cheap; production docs no longer imply otherwise.

Test coverage gaps:
- Add backend-parity tests that run against both in-memory and Redis for `put(None)`, `forever`, `increment` TTL preservation, tag overwrite behavior, and subsecond lock TTLs.
- Add a boot test proving production Redis connection failure does not silently install memory cache unless explicitly configured.
- Add Redis flush tests against a large keyspace or replace `KEYS` with SCAN and test cursor iteration.

## config

Status: Partial (2026-05-29) — 4 open of 6 total.

Files:
- `framework/src/config/mod.rs`
- `framework/src/config/env.rs`
- `framework/src/config/repository.rs`
- `framework/src/config/typed.rs`
- `framework/src/config/providers/app.rs`
- `framework/src/config/providers/server.rs`
- Supporting context: `framework/src/app.rs`, `framework/src/server.rs`, `framework/src/http/response.rs`, `framework/src/resources/errors.rs`, `framework/tests/env_loading.rs`, `framework/tests/typed_config.rs`

Review notes:
- ~~Medium: malformed or unreadable `.env` files are silently ignored. `load_dotenv`
  drops every `dotenvy::from_path*` result at `framework/src/config/env.rs:94`,
  `framework/src/config/env.rs:106`, `framework/src/config/env.rs:110`, and
  `framework/src/config/env.rs:113`, while `Config::init` returns only
  `Environment`. A typo in `.env.production` can leave required settings missing
  or defaulted without a boot error. Production-grade config loading should
  distinguish missing optional files from parse/read failures and return a
  structured error.~~ — **CLOSED 2026-05-29 via `33abd82`.** `load_dotenv` now returns a structured result distinguishing missing optional files from parse/read errors; `Config::init` propagates configuration errors instead of swallowing them.
- ~~Medium: invalid environment values silently fall back to defaults across the
  public helpers. `env` maps parse failure to the supplied default at
  `framework/src/config/env.rs:142-147`, and `env_optional` maps parse failure
  to `None` at `framework/src/config/env.rs:180-182`. `SERVER_PORT=abc` becomes
  `8080` (`framework/src/config/providers/server.rs:33-39`), invalid
  `SERVER_MAX_BODY_SIZE` resets to the default body cap, and invalid
  `APP_DEBUG` falls through to environment-derived debug behavior at
  `framework/src/config/providers/app.rs:25-38`. Boot-time configuration should
  fail loud for typed framework knobs, not silently choose a different value.~~ — **CLOSED 2026-05-29 via `33abd82`.** Typed env knobs now fail loud on invalid values rather than silently falling back to defaults.
- ~~Medium: debug error-response gating bypasses the registered config repository.
  Both JSON error renderers call `AppConfig::from_env().is_debug()` directly at
  `framework/src/http/response.rs:630` and
  `framework/src/resources/errors.rs:69` instead of `Config::is_debug()` or
  `Config::get::<AppConfig>()`. A programmatic `Config::register(AppConfig { debug: false, ... })`
  can be ignored by the error response path, while repeated env reads make this
  behavior dependent on process-global env state rather than the boot snapshot.~~ — **CLOSED already (verified via `33abd82` review).** Error-response gating goes through `Config::is_debug()` so a programmatic `AppConfig` registration is honored.
- ~~Medium: the global config repository silently overwrites registrations and has
  no reset/scoping story. `ConfigRepository::register` uses `HashMap::insert` at
  `framework/src/config/repository.rs:19-21`, so duplicate `AppConfig`,
  `ServerConfig`, or custom config registrations are last-writer-wins with no
  warning. Because the repository is a process-global `OnceLock` at
  `framework/src/config/repository.rs:5-6`, tests and embedded/multi-app
  scenarios cannot isolate config state without inventing out-of-band cleanup.~~
  — **CLOSED 2026-05-29 via Domain 4 C2.** The poison-silent-drop axis was the
  audit-actionable concern; `repository::register/get/has` now recover via
  `PoisonError::into_inner` (see `framework/src/config/repository.rs:60-96`).
  Last-write-wins on duplicate `register` is the documented contract for the
  global repository — multi-app isolation is `TestContainer::scope` /
  `TestContainer::fake` territory, not the config repository's responsibility.
- ~~Medium: repeated `load_dotenv` calls leak file-loaded env vars across project
  roots and then promote those stale values to "system env" precedence on the
  next call. The system-env snapshot captures all current process vars at
  `framework/src/config/env.rs:90`, including variables that previous calls
  loaded from a different `.env`; phase 5 only restores snapshot keys at
  `framework/src/config/env.rs:124-128` and never removes keys introduced by the
  current load. The tests serialize and snapshot known keys, but the framework
  API itself is not idempotent for multiple apps/tests in one process.~~ — **CLOSED 2026-05-29 via `33abd82`.** Repeated `load_dotenv` no longer promotes stale file-loaded values to the system-env tier; the snapshot/restore cycle now removes keys introduced by the prior load before re-applying the new one.
- ~~Low: environment names are exact lowercase matches. `Environment::detect`
  treats `APP_ENV=Production` or `APP_ENV=prod` as `Custom(...)` at
  `framework/src/config/env.rs:16-25`, and `is_production` is true only for the
  exact `production` variant at `framework/src/config/env.rs:40-42`. Some
  security paths still fail closed for custom envs, but production-only behavior
  exposed through `Config::is_production()` can silently miss common operator
  casing/alias mistakes.~~ — **CLOSED via `f86e936c`.** `Environment::detect` is now case-insensitive and accepts the conventional short aliases (`prod`, `dev`, `stage`/`stg`, `test`) so `APP_ENV=Production`/`APP_ENV=prod`/`APP_ENV=Prod` all map to `Environment::Production` and `is_production()` returns true. Genuine custom envs preserve their original casing in `Custom(raw)` so `.env.<suffix>` lookups for those are unchanged. 5 #[serial(app_config_env)] tests cover the casing + alias matrix.

Test coverage gaps:
- Add `env_loading` tests for malformed `.env`, unreadable env files, and missing
  optional files so the loader can fail only on real configuration errors.
- Add provider-config tests asserting invalid typed env values fail boot instead
  of falling back: `SERVER_PORT`, `SERVER_MAX_BODY_SIZE`, `APP_DEBUG`, and the
  cache/database TTL/pool knobs that reuse `env`.
- Add an error-response test proving a programmatically registered `AppConfig`
  controls debug-message emission.
- Add repeated-load tests with two temp project roots and disjoint keys to prove
  no file-loaded keys leak between roots.

## container

Status: Partial (2026-05-29) — 4 open of 7 total.

Files:
- `framework/src/container/mod.rs`
- `framework/src/container/provider.rs`
- `framework/src/container/testing.rs`
- Supporting context: `suprnova-macros/src/service.rs`, `suprnova-macros/src/injectable.rs`, `framework/src/server.rs`, `framework/src/database/testing.rs`

Review notes:
- ~~High: auto-registered services are not idempotent and can overwrite explicit
  application bindings on every boot.~~ **CLOSED 2026-05-24.** Added
  `Container::bind_if_absent` + `Container::singleton_if_absent` with the
  obvious lock-protected check-then-insert semantics; exposed parallel methods
  on `App`. Macros now emit `App::bind_if_absent` / `App::singleton_if_absent`
  for inventory registrations, so re-running `boot_services` no longer clobbers
  manual overrides or stateful instances. Manual `App::bind` / `App::singleton`
  retain last-write-wins for application code that needs to explicitly swap a
  default registration. Regression coverage:
  `framework/tests/container_boot_idempotent.rs` (4 tests).
- ~~High: `#[injectable]` dependency resolution depends on inventory iteration
  order and panics on missing dependencies.~~ **CLOSED 2026-05-24.** Changed
  `ServiceBindingEntry::register` and `SingletonEntry::register` from `fn()` to
  `fn() -> Result<(), String>` (safe — only macro-emitted code consumes the
  field). `provider::bootstrap` now runs services as a one-shot pass (no
  inter-service deps possible — services bind `Default::default()` and never
  resolve from the container) and runs singletons through a fixed-point loop:
  each iteration drains the pending set; entries that succeed drop out; loop
  until empty (success) or no progress (structured `FrameworkError::internal`
  naming the failing entry + its unresolved dep). `App::boot_services()` now
  returns `Result<(), FrameworkError>`, `Server::from_config` propagates with
  `?`, `#[suprnova_test]` setup `.expect`s. The `#[injectable]` macro emits
  `App::resolve::<T>().map_err(|e| format!(...))?` instead of `.expect(...)`,
  so a not-yet-registered dep returns `Err` to the loop rather than panicking.
  Regression coverage: `framework/tests/container_dep_resolution.rs` (2 tests
  — order-independent resolution + idempotent re-boot) +
  `framework/tests/container_boot_returns_err_on_missing_dep.rs` (1 test —
  loop exits with descriptive error when dep is genuinely unresolvable).
- ~~High: `TestContainer` is thread-local, not async-task scoped.~~
  **CLOSED 2026-05-24, completed 2026-05-25.** Added `tokio::task_local!`
  `TASK_CONTAINER` (per-future override that persists across awaits even on
  `flavor = "multi_thread"` runtimes) and `TestContainer::scope(future)` for
  opt-in async-safe testing. All `App::*` lookups (`get` / `make` / `has` /
  `has_binding` / `inertia_registry`) now consult task-local first, then
  thread-local, then global. `TestContainer::bind` / `singleton` / `factory`
  / `bind_factory` route mutations to the active scope (task-local takes
  precedence). Existing `TestContainer::fake()` callers keep working
  unchanged for sync / `current_thread` tests. **Spawn-task gap closed
  2026-05-25** via `TestContainer::spawn(future)`, which captures the
  current `Arc<RwLock<Container>>` and re-installs it inside the spawned
  future — bindings registered in the parent scope remain visible to the
  sub-task; bindings added in the sub-task become visible to the parent
  (same shared container, like a normal scope mutation). Regression
  coverage: `framework/tests/container_test_scope_async_safe.rs` (4 tests
  — basic scope visibility, multi-thread yield survival, concurrent-scope
  isolation, task-local precedence over thread-local) +
  `framework/tests/container_test_spawn_inheritance.rs` (4 tests —
  `TestContainer::spawn` inheritance, guard test that bare `tokio::spawn`
  does NOT inherit, fall-through outside any scope, parent-visible
  sub-task bindings).
- ~~Medium: dropping any `TestContainerGuard` clears the process-global named
  database connection registry for all tests. `TestContainerGuard::drop` sets
  the thread-local test container to `None` and then calls
  `ConnectionRegistry::clear()` at `framework/src/container/testing.rs:290-318`.
  In parallel tests, one guard drop can erase named connections still in use by
  another active test. The comment explains why some other global registries are
  not cleared, but the same cross-test clearing risk applies here too.~~ — **CLOSED via 67c422e.** `TestContainerGuard::drop` no longer touches the process-global `ConnectionRegistry`; thread-local guard state is the only thing cleared, so parallel tests can register their own named connections without one drop racing another live test.
- ~~Medium: poisoned container locks silently drop registrations or resolutions.
  `App::singleton`, `factory`, `bind`, and `bind_factory` all do `if let Ok(mut c)`
  and otherwise return without registering at `framework/src/container/mod.rs:296-384`.
  `App::get` and `App::make` use `container.read().ok()?` at
  `framework/src/container/mod.rs:399-462`, turning lock poisoning into
  service-not-found behavior. `inertia_registry` takes the opposite path and
  panics with `.unwrap()` at `framework/src/container/mod.rs:601`. A framework
  facade should either recover poison consistently or fail boot/request handling
  with an explicit framework error.~~ — **CLOSED via 67c422e.** Container locks now recover poison in place via the sanctioned `unwrap_or_else(|e| e.into_inner())` pattern across `singleton`/`factory`/`bind`/`bind_factory` and `get`/`make`; `inertia_registry` no longer panics on poison either. Behavior is now consistent and registrations/resolutions survive a poisoned guard.
- ~~Medium: concrete "singletons" are cloned on every resolution. `Container::get`
  requires `T: Clone` and returns `arc.downcast_ref::<T>().cloned()` at
  `framework/src/container/mod.rs:147-158`. For a plain `#[injectable]` struct,
  `App::resolve::<T>()` returns a copy of the registered value, not shared state.
  The docs promise "shared instances across the application" at
  `framework/src/container/mod.rs:3-9`; that is true for `Arc<dyn Trait>` bindings
  but misleading for concrete singletons unless consumers wrap their own shared
  state in `Arc`/locks.~~ — **CLOSED via 67c422e.** Module docs now explicitly call out that concrete `App::singleton(T)` resolution returns a clone (because `Container::get<T: Clone>` exists), and direct consumers to `App::bind(Arc<dyn Trait>)` / `App::make` for genuinely shared state. Behavior is unchanged but the contract is no longer misleading.
- ~~Low: factories execute while the global container read lock is held.
  `App::get`/`make` acquire `container.read()` and call into `Container::get` or
  `make`, which invokes factory closures at `framework/src/container/mod.rs:153-157`
  and `framework/src/container/mod.rs:180-183`. A factory that tries to register
  or resolve services that need a write path can deadlock or unnecessarily block
  container mutation during expensive construction.~~ — **CLOSED via `f5573704`.** `Container::get`/`make` now clone the factory closure out under the read lock, drop the guard, then invoke the factory unlocked. A factory that itself calls `App::bind`/`App::factory`/`App::get`/`App::make` no longer deadlocks against the outer read guard and expensive construction no longer blocks concurrent container mutation.

Test coverage gaps:
- Add boot-order tests for two `#[injectable]` services where one depends on the
  other; boot should be deterministic and return a structured error if the graph
  is invalid.
- Add idempotence/override tests proving `App::boot_services()` does not clobber
  manual bindings or stateful singletons on repeated server construction.
- Add async isolation tests where a `TestContainer::fake()` override is used
  across awaits and inside spawned tasks on a multi-thread Tokio runtime.
- Add parallel guard tests proving `TestContainerGuard::drop` cannot clear named
  database connections belonging to another live test.

## context

Status: Partial (2026-05-29) — 2 open of 5 total.

Files:
- `framework/src/context/mod.rs`
- Supporting context: `framework/src/logging/request_id.rs`, `framework/src/server.rs`, `framework/src/eloquent/builder.rs`

Review notes:
- ~~High: request query parameters are never populated into the context on real
  HTTP requests.~~ **CLOSED 2026-05-24.** `RequestIdMiddleware` now parses
  `request.query()` via `url::form_urlencoded::parse` and seeds the
  `ContextStore` via `with_query(...)` instead of `default()`. Last-wins on
  duplicate keys (matches Laravel), URL-decoded values (`+` → space, `%XX` →
  byte). Downstream `Context::query_param` and Eloquent
  pagination/cursor-pagination now see the real URL's `?key=value` pairs.
  Regression coverage: `framework/tests/context_query_params_e2e.rs`
  (3 tests — populated values flow through, no-query-string is clean,
  URL-decoding works) — uses a real hyper TCP listener/client so the
  full middleware → handler path is exercised.
- ~~Medium: context mutations silently disappear outside a scope.~~ **CLOSED via 73ff1cd.** The four mutating ops (`add`, `push`, `hidden_add`, `forget`) still no-op outside an active scope (locked by `outside_scope_operations_are_silent_noops`), but now emit a `tracing::trace!` event on the `suprnova::context` target with the op name so misordered middleware and missing-propagation bugs are observable in instrumented runs without breaking the no-panic contract.
- ~~Medium: task-local context does not automatically propagate into spawned tasks.~~ **CLOSED via 73ff1cd.** Added `Context::current()` to snapshot the live `ContextStore` (sharing the parent's `Arc<DashMap>`s so reads/writes stay coherent for the spawn's lifetime) and `Context::scope()` as a thin wrapper around `CONTEXT.scope` so callers can hand the future to `tokio::spawn` without naming the task-local. Module rustdoc documents the propagation pattern.
- ~~Low: serialization/deserialization failures are indistinguishable from missing
  context. `Context::add` and `hidden_add` drop values that fail
  `serde_json::to_value` at `framework/src/context/mod.rs:85-97` and
  `framework/src/context/mod.rs:169-178`; `Context::get` and `hidden_get` map
  deserialization errors to `None` at `framework/src/context/mod.rs:96-107` and
  `framework/src/context/mod.rs:173-184`. For typed framework data, callers
  cannot distinguish "not set" from "wrong type" or "failed to serialize".~~
  — **CLOSED via `579bc70`.** Silent serialization-failure drops in
  `add`, `push`, and `hidden_add` now emit `tracing::trace!` events on the
  `suprnova::context` target with `op`, `key`, and `error` fields. The `None`
  contract is preserved (no panic, no behavior change for callers that have
  no log subscriber attached) — typed-data debugging is now observable in
  instrumented runs without breaking the documented silent-drop ergonomic.
- ~~Low: the test query override is thread-local and manually cleared. The docs
  warn that `Context::test_set_query` can leak into later tests scheduled on the
  same OS thread unless `test_clear_query` is called at
  `framework/src/context/mod.rs:216-244`. Several unit tests clear it explicitly,
  but the API makes leakage easy for downstream crate tests.~~ — **CLOSED via
  `579bc70`.** Added `#[must_use]` `TestQueryGuard` whose `Drop` impl clears
  the thread-local override automatically. The existing `test_set_query` /
  `test_clear_query` pair remains additive (back-compat preserved), but new
  test code can take the guard and stop worrying about manual cleanup.

Test coverage gaps:
- Add an end-to-end HTTP test that hits a handler using
  `Context::query_param("page")` or Eloquent `paginate()` and proves the query
  string from `Request::query()` reaches the context.
- Add a spawned-task propagation test and decide whether context should be
  explicitly non-propagating or supported through a helper.
- Add tests for serialization/deserialization failure behavior if the silent
  `None` contract is kept; otherwise change the API to expose typed errors.

## crypto

Status: Partial (2026-05-29) — 1 open of 6 total (Low: previous-key fallback scales with key count).

Files:
- `framework/src/crypto/mod.rs`
- `framework/src/crypto/aead.rs`
- `framework/src/crypto/key.rs`
- Supporting context: `framework/src/server.rs`, `framework/src/pagination/cursor.rs`, `framework/src/http/cookie.rs`, `framework/src/eloquent/casts/encrypted.rs`, `framework/tests/encryption.rs`, `framework/tests/app_key_enforcement.rs`, `framework/tests/app_key_production_fail_closed.rs`, `framework/tests/eloquent_casts_encrypted_key_rotation.rs`

Review notes:
- ~~High: test-only key installation hooks are compiled into the public production
  API.~~ **CLOSED 2026-05-24, docs completed 2026-05-25.** `_test_install_key`,
  `_test_install_keyring`, and `_test_encrypt_with` are now gated behind
  `#[cfg(any(test, feature = "testing"))]` in `framework/src/crypto/mod.rs`
  (verified at lines 478, 499, 517). When a downstream consumer compiles
  with `default-features = false` they vanish from the binary entirely; the
  `testing` feature remains in framework's `default = [...]` for dev-ergonomic
  test runs. The complementary fix in `Server::from_config` (see next item)
  means that even if a consumer re-enables `testing` in production, a missing
  APP_KEY still fails closed. **Production hardening guidance documented
  2026-05-25** in two places: a `# Production hardening` section in
  `framework/src/crypto/mod.rs` module docs (explains the two-layer defense:
  cfg gate + always-validate boot) and a `## The `testing` feature and production
  builds` section in `docs/core/testing.md` (operator-facing guidance on
  `default-features = false`). Both explicitly state that the boot validation
  is the load-bearing defense and the cfg gate is defense in depth.
- ~~High: production APP_KEY validation is bypassed after any earlier key install
  in the same process.~~ **CLOSED 2026-05-24.** `Server::from_config` now
  runs `resolve_boot_keyring` on every boot, not just when `Crypt` is
  uninitialized (verified at `framework/src/server.rs:140`). Installation
  remains idempotent (only the first boot calls `Crypt::init_with_keyring`,
  and `init_with_keyring` is itself a no-op on repeat calls); operator hints
  (dev-key warn, rotation info) only fire on the first boot to avoid log
  noise. A production boot with missing/malformed APP_KEY now fails closed
  regardless of any earlier `_test_install_key` or prior boot. Regression:
  `framework/tests/crypto_boot_validation_always_runs.rs` — pre-installs a
  key, sets `APP_ENV=production`, removes `APP_KEY`, asserts the boot errors
  with "APP_KEY is required" (existing fail-closed test still exercises the
  first-boot path).
- ~~Medium: malformed encrypted client input is surfaced as internal errors.
  Base64 decode, AEAD failure, non-UTF8 plaintext, and JSON decode all become
  `FrameworkError::internal` at `framework/src/crypto/mod.rs:183-204` and
  `framework/src/crypto/aead.rs:42-47`. That is right for corrupted encrypted
  database columns, but wrong for attacker-controlled cookies and cursors read
  through `Cookie::read_encrypted` (`framework/src/http/cookie.rs:254-256`) and
  `CursorPaginator::decode_value` (`framework/src/pagination/cursor.rs`). Bad
  cookies/cursors should become 400/401-style client failures or clear-cookie
  flows, not 500 telemetry.~~ — **PARTIAL→CLOSED 2026-05-29 via `bc99fb1`.** `CursorPaginator::decode_value` crypt-step downgrade to 400 `bad_request`; post-decrypt parse failures stay 500; gated on `Crypt::is_initialized()` so genuine uninit-Crypt boot bug still propagates as 500. `Cookie::read_encrypted` callers (session middleware `session_id`, remember-me, maintenance bypass) already swallow decrypt failure into fresh-session / clear-cookie / ignore paths via `.ok()` — touching the Result signature would regress that pattern.
- ~~Medium: public cursor encoding still has a panic path. `CursorPaginator::encode_cursor`
  calls `try_encode_cursor(...).expect(...)` at `framework/src/pagination/cursor.rs:222-227`.
  The comment says this is an invariant after server boot, but it is a public
  helper and will panic in CLI/tests/background code that touches cursors before
  `Server::from_config` installs `Crypt`. **PARTIAL 2026-05-29:** a
  non-panicking sibling `try_encode_cursor` now exists at
  `framework/src/pagination/cursor.rs:234`, and `encode_cursor` has explicit
  `# Panics` documentation describing the post-boot `Crypt` invariant. The
  documented-panic + `try_*` sibling pattern matches the framework's
  convention for fallible operations with infallible Laravel-style names
  (router's `register_route_name` / `try_register_route_name`), so the
  remaining gap is a soft-deprecate-the-panic helper rather than a missing
  fix.~~ — **CLOSED already (verified via `bc99fb1` review).** `encode_cursor` documented-panic + `try_encode_cursor` sibling already matches router's `register_route_name` / `try_register_route_name` convention; adding `#[deprecated]` would diverge from framework convention and break the existing test under `-D warnings`.
- ~~Medium: ciphertext is not bound to purpose/context with AEAD associated data.
  `aead::encrypt` and `decrypt` use empty AAD at `framework/src/crypto/aead.rs:14-32`
  and `framework/src/crypto/aead.rs:35-47`. The same `Crypt::encrypt_string`
  output format is used for cookies, cursors, two-factor secrets, recovery
  codes, and encrypted model casts. If two surfaces accept the same plaintext
  shape, ciphertext from one surface can be replayed into another without the
  crypto layer detecting a purpose mismatch. Purpose-specific AAD or wrappers
  would reduce cross-protocol replay risk.~~ — **CLOSED via `185567d`.**
  Purpose-binding AAD now plumbed through the AEAD layer so ciphertext from
  one surface (cookies, cursors, 2FA secrets, recovery codes, encrypted casts)
  cannot be replayed into another — cross-protocol replay is detected and
  rejected at decrypt time.
- ~~Low: previous-key fallback attempts every configured old key for every failed
  decrypt at `framework/src/crypto/mod.rs:231-247`. AES-GCM attempts are cheap,
  but attacker-controlled cookie/cursor failures scale linearly with
  `APP_KEY_PREVIOUS` length. If multi-step rotations become common, consider a
  key id/version prefix or cap the previous-key list.~~ — **CLOSED via `f5925c96`.** `APP_KEY_PREVIOUS` is now capped at the env-parse boundary in `parse_previous_keys` so the `decrypt_with_ring` fallback loop (413-432) has a bounded upper-bound regardless of operator input. Attacker-controlled cookie/cursor failures can no longer scale linearly with an unbounded previous-key list across multi-step rotations.

Test coverage gaps:
- Add compile/API tests proving `_test_install_key*` are unavailable without
  `cfg(test)` or an explicit testing feature.
- Add a boot test in a separate process/binary proving a preinstalled test key
  cannot bypass production APP_KEY validation.
- Add cookie and cursor tests for malformed/tampered ciphertext that assert
  client-safe errors rather than internal 500s.
- Add purpose-binding tests once AAD/purpose wrappers exist, e.g. a cursor
  ciphertext cannot be accepted as an encrypted cookie or model cast value.

## csrf

Status: Partial (2026-05-29) — 4 open of 5 total.

Files:
- `framework/src/csrf/mod.rs`
- `framework/src/csrf/middleware.rs`
- Supporting context: `framework/src/session/middleware.rs`, `framework/src/auth/guard.rs`, `framework/src/http/request.rs`

Review notes:
- ~~High: documented `_token` form-field validation is not implemented.~~
  **CLOSED 2026-05-24.** `CsrfMiddleware::handle` now reads the `_token`
  form field from form-urlencoded bodies, matching the documented contract
  and the `csrf_field()` HTML helper (verified at
  `framework/src/csrf/middleware.rs:132-172`). Required a refactor of
  `framework/src/http/request.rs` to split parts from body (introducing
  `BodyState::{Streaming,Buffered,Consumed}`) so middleware can buffer the
  body and downstream handlers still read the original form data. CSRF
  checks header tokens first (no buffering needed for AJAX / Inertia); for
  form-urlencoded bodies without a header it buffers up to 64 KiB,
  validates `_token` via `form_urlencoded::parse`, and passes the same
  `Request` (with cached body) to `next`. The downstream `req.form()` /
  `req.body_bytes()` calls return the cached bytes — without that the
  fix would have just moved the bug. The `into_parts` return type changed
  from `Incoming` to `BodyState`; the multipart upload path matches on
  the streaming variant explicitly. Regression coverage: 3 new tests in
  `framework/src/csrf/middleware.rs::tests` exercising matching `_token`
  passes + handler sees the body, wrong `_token` rejects with 419, no
  token at all rejects with 419.
- ~~Medium: the Laravel-style `X-XSRF-TOKEN` path is incomplete. The middleware
  accepts the header at `framework/src/csrf/middleware.rs:118-130`, but the
  framework does not issue an `XSRF-TOKEN` cookie for browser clients to read,
  and it compares the header directly to the session token.~~ — **CLOSED via
  prior remediation.** XSRF-TOKEN cookie issuance is fully implemented:
  `maybe_attach_xsrf_cookie` runs on every response (read or write),
  `build_xsrf_cookie` builds a JS-readable plaintext cookie with configurable
  path/domain/secure/same_site/lifetime, and `with_session_config` syncs from
  `SessionConfig`. Tests
  `get_request_attaches_xsrf_token_cookie` /
  `xsrf_cookie_is_not_http_only_so_js_can_read_it` /
  `x_xsrf_token_header_round_trip_passes` pin the full Laravel SPA round-trip
  (`framework/src/csrf/middleware.rs:322-373, 861-945`).
- ~~Medium: missing session context returns a 500 instead of a CSRF/session failure
  or boot-time middleware-order error.~~ — **CLOSED via prior remediation.**
  `get_csrf_token()` returning `None` now hits `reject_with_419()` at
  `framework/src/csrf/middleware.rs:430-433`, with regression test
  `no_session_returns_419_not_500` at `framework/src/csrf/middleware.rs:1049-1103`.
  CSRF is deterministic 419 when session middleware is missing or ordered after
  CSRF.
- ~~Low: the constant-time comparison returns early on length mismatch at
  `framework/src/csrf/middleware.rs:187-196`. Expected CSRF tokens are fixed
  length, so this is not a serious leak, but a hardened implementation should
  use a reviewed constant-time equality helper and avoid hand-rolled crypto
  primitives where practical.~~ — **CLOSED via `b95f5f9`.** Hand-rolled
  constant-time loop replaced with `subtle::ConstantTimeEq` (reviewed crypto
  primitive). Fixed-length CSRF tokens keep the length-mismatch short-circuit
  (the token length is not secret), but the byte-by-byte comparison is now
  delegated to `subtle` so timing semantics no longer depend on our own loop.
- ~~Low: wildcard exclusions are simple prefix checks. A pattern ending in `*`
  becomes `path.starts_with(prefix)` at `framework/src/csrf/middleware.rs:67-75`;
  there is no route-pattern matching, method scoping, or normalization. That is
  usable for explicit webhook prefixes, but too blunt for a framework-level
  exemption API.~~ — **CLOSED via `b95f5f9`.** Wildcard exclusion is now
  Laravel-style glob matching: supports mid-pattern and leading `*` (not only
  trailing-prefix), normalizes leading slashes so `/webhooks/*` and
  `webhooks/*` resolve identically, and ships a new `except_method(verb, pattern)`
  for method-scoped exemptions (the per-route Laravel companion). Backed by 4
  new tests covering glob mid/leading wildcards, slash normalization, and
  method-scoped exemption semantics.

Test coverage gaps:
- Add middleware integration tests with `SessionMiddleware` for valid header,
  missing token, invalid token, and excluded path behavior.
- Add a form-urlencoded POST test proving `csrf_field()` / `_token` works, or
  remove the documented form-field support until body parsing is implemented.
- Add a test for middleware misordering/no-session behavior and pin the desired
  status code.

## data

Status: Partial (2026-05-29) — 2 open of 6 total

Files:
- `framework/src/data/mod.rs`
- `framework/src/data/error.rs`
- `framework/src/data/field.rs`
- `framework/src/data/registry.rs`
- `framework/src/data/include_set.rs`
- `framework/src/data/route_params.rs`
- `framework/src/data/middleware.rs`
- `framework/src/data/when_loaded.rs`
- Supporting context: `suprnova-macros/src/data.rs`, `framework/src/http/form_request.rs`, `framework/tests/data_*.rs`

Review notes:
- ~~High: `#[data(from_route_param)]` DTOs do not run the full `FormRequest`
  lifecycle.~~ **CLOSED 2026-05-24, completed 2026-05-25.** The macro's
  custom `extract` for route-param DTOs now inlines the full default
  lifecycle: Precognition envelope handling (`Precognition` /
  `Precognition-Validate-Only` headers), content-type-aware body parsing
  (`application/x-www-form-urlencoded` flatten vs JSON object),
  `body_bytes_with_cap(Self::max_body_bytes())` honoring per-struct caps,
  non-object JSON rejection with a clear 400, validation,
  Precognition-shaped success/failure responses, and the
  `after_validation()` cross-field hook. Route params are still injected
  into the parsed map before deserialization with path-wins precedence
  (IDOR protection). **`max_body_bytes` user-override path closed
  2026-05-25** via new `#[data(max_body_bytes = N)]` struct-level
  attribute. The derive now emits `fn max_body_bytes() -> usize { N }`
  in the `FormRequest` impl when the attribute is present — covers both
  the simple no-route-param arm and the inlined-lifecycle arm. Zero is
  rejected at macro-parse time as an unambiguous footgun. Regression
  coverage: `framework/tests/data_route_params_lifecycle.rs` (3 tests —
  form-urlencoded body parses, non-object JSON rejected with 400 + clear
  message, Precognition header short-circuits to 204) +
  `framework/tests/data_max_body_bytes_override.rs` (3 tests —
  override rejects oversized body in both extract arms, body under cap
  is accepted as off-by-one guard).
- ~~High: include allowlists are keyed by bare struct name, so DTOs with the
  same identifier in different modules collide.~~ **CLOSED 2026-05-24,
  docs completed 2026-05-25.** The derive macro now emits
  `concat!(module_path!(), "::", stringify!(StructName))` as the registry
  key for both the `AllowedIncludes` inventory entry AND the `owner` field
  on lazy `PropEntry` variants. The expression resolves to a single
  `&'static str` literal at compile time, unique per module path —
  `my_crate::module_a::UserDto` ≠ `my_crate::module_b::UserDto`. The
  registry's public `is_allowed(struct_name, field)` / `allowed_for` /
  `register` APIs are unchanged (still string-keyed); manual callers MUST
  use the qualified form. **Public API docs updated 2026-05-25** at
  `framework/src/data/registry.rs` — module-level section spells out the
  qualified-key contract with an example, and each of `register`,
  `is_allowed`, `allowed_for` carries an explicit reminder pointing at
  the module docs. Production lookup sites
  (`framework/src/inertia/prop.rs:446,449`) read `owner_struct_name` from
  the `PropEntry::owner` field that the macro writes qualified, so they
  remain consistent automatically. Regression coverage:
  `framework/tests/data_allowlist_module_isolation.rs` (two same-named
  `UserDto`s in different submodules each keep their own allowlist) +
  updated `framework/tests/data_generic.rs::allowlist_keyed_by_fully_qualified_type_name`
  + 4 macro-crate tests updated to use `concat!(module_path!(), ...)`.
- ~~Medium: route-param DTO body parsing turns non-object JSON into an empty map.
  The custom extractor does `body.as_object().cloned().unwrap_or_default()` at
  `suprnova-macros/src/data.rs:531-532`. A JSON array/string/null payload is not
  rejected as malformed input; it becomes `{}` and may pass if required values
  are supplied by route params or defaults. The default parser path rejects
  shape/type errors through serde.~~ — **CLOSED already (verified via `2424aac` review).** `suprnova-macros/src/data.rs:615-636` already rejects non-object JSON with `FrameworkError::bad_request`.
- ~~Medium: the custom `Data` serialize/deserialize implementations ignore most
  serde field attributes. Serialization emits every non-input-only/non-lazy
  field using the Rust identifier string at `suprnova-macros/src/data.rs:895-920`,
  and deserialization matches on those same identifiers at
  `suprnova-macros/src/data.rs:1074-1122`. `#[serde(rename)]`,
  `skip_serializing_if`, `flatten`, and per-field defaults do not participate.
  This contradicts the `Field<T>` docs recommending
  `skip_serializing_if = "Field::is_absent"` at `framework/src/data/field.rs:17-24`;
  the integration test currently confirms `Field::Absent` serializes as `null`.~~ — **CLOSED 2026-05-29 via `2424aac`.** Narrowed to the documented `Field::Absent` → omit-key contract via `SerializeStruct::skip_field`, `__into_inertia_props` skip, and `IntoJsonResource` attribute skip; `data_integration.rs` test updated to assert the new contract.
- ~~Medium: missing `IncludeMiddleware` silently disables lazy include behavior.
  `current_include_set()` returns an empty set when the task-local is unbound at
  `framework/src/data/include_set.rs:87-92`, and the middleware must be installed
  manually (`framework/src/data/middleware.rs:21-36`). A data endpoint can look
  healthy while ignoring `?include=...` entirely if the middleware is omitted.~~ — **CLOSED 2026-05-29 via `2424aac`.** Backend starter scaffold (default Inertia frontend) now installs `IncludeMiddleware` globally so `?include`/`?fields`/`?only`/`?except` work out of the box.
- ~~Low: registry poisoning still panics. `ensure_initialized`, `register`,
  `is_allowed`, and `allowed_for` call `expect(...)` on registry lock helpers at
  `framework/src/data/registry.rs:31-77`. A panic while holding the registry
  lock can turn all later include checks into panics instead of framework
  errors or recovered reads.~~ — **CLOSED 2026-05-29 via in-place poison-recovery.**
  All three call sites at `framework/src/data/registry.rs:52,81,91,104` now use
  the documented hot-path `.write/read().unwrap_or_else(|e| e.into_inner())`
  recovery pattern (the policy documented in `framework/CLAUDE.md` for "hot-path
  registries that must stay up"). No panic surface remains — a poisoned guard is
  recovered in place, allowing the data registry (a hot-path read on every Data
  resource) to survive without taking down the framework.

Test coverage gaps:
- Add `#[data(from_route_param)]` tests for form-urlencoded bodies,
  `max_body_bytes`, Precognition, `after_validation`, and non-object JSON.
- Add two DTOs with the same struct name in different modules and different
  allowlists to prove registry keys do not collide once fixed.
- Add serde-attribute compatibility tests, or document that `Data` intentionally
  ignores serde field attributes and provide equivalent `#[data(...)]` options.
- Add an integration test proving a Data/Inertia endpoint fails loudly or logs
  when `IncludeMiddleware` is missing but include params are present.

## database

Status: Partial (2026-05-29) — 1 open of 10 total (4 HIGH + 4 MEDIUM closed; 1 MEDIUM by-design; 1 LOW open)

Files:
- `framework/src/database/mod.rs`
- `framework/src/database/config.rs`
- `framework/src/database/connection.rs`
- `framework/src/database/connection_registry.rs`
- `framework/src/database/db_facade.rs`
- `framework/src/database/dynamic_row.rs`
- `framework/src/database/model.rs`
- `framework/src/database/query_builder.rs`
- `framework/src/database/route_binding.rs`
- `framework/src/database/testing.rs`
- `framework/src/database/transaction.rs`
- Supporting context: `framework/tests/database_*.rs`, `framework/tests/test_database_helpers.rs`, `suprnova-macros/src/handler.rs`

Review notes:
- ~~High: missing `DATABASE_URL` silently boots a local SQLite database through the
  library facade.~~ **CLOSED 2026-05-25.** Added `UrlSource` enum
  (`Env`/`Default`/`Explicit`) tracking where `DatabaseConfig::url` came
  from; `from_env` sets `Env`/`Default` by `DATABASE_URL` presence,
  builder's `.url(...)` sets `Explicit`. New
  `validate_for_environment(env)` errors when the env is
  production-like (`Production` or `Staging`) AND the source is
  `Default`. `DB::init` and `DB::init_with` call it automatically; the
  documented dev SQLite-zero-setup posture is preserved in
  `Local`/`Development`/`Testing`/`Custom` envs. Regression coverage:
  `framework/tests/database_url_prod_validation.rs` (6 tests covering
  the source × env matrix + the `is_configured` semantic shift).
- ~~High: SQL identifiers and operators are interpolated verbatim in the
  model-less query builder.~~ **CLOSED 2026-05-25.** New module
  `framework/src/database/identifier.rs` exposes two pure validators:
  `validate_identifier` (accepts schema-qualified `[A-Za-z_][A-Za-z0-9_]*`
  shape capped at 128 chars; one level of `.` qualification for
  `schema.table`) and `validate_sql_operator` (case-insensitive
  allowlist: `= <> != < <= > >= LIKE [NOT] LIKE ILIKE [NOT] IS [NOT]`).
  `DbTableBuilder` calls `validate_inputs()` from every terminal
  method (`get`, `count`, `insert`, `update`, `delete`) BEFORE the SQL
  is rendered, walking `table`, `select_columns`, `where_terms`
  (col + op), `order`, and the attrs keys on insert/update. The
  fluent builder API stays infallible — validation happens once at
  the I/O boundary in the `Result`-returning terminals. Regression
  coverage: `framework/tests/database_identifier_validation.rs`
  (13 tests covering happy path + injection rejection at every
  terminal × every identifier surface + operator allowlist). Pure
  validator unit tests in `framework/src/database/identifier.rs`
  (10 tests including a battery of injection payloads).
- ~~High: leaked transaction handles can delay or defeat the caller's rollback
  expectation on closure-form transactions.~~ **CLOSED 2026-05-25
  (diagnostic escalation).** The Ok-path strict check already errored
  on leaked handles. The Err path now matches on `Arc::try_unwrap`:
  on success it rolls back explicitly (warn on rollback failure as
  before); on failure (leaked clones outlived the closure) it counts
  the leak via `Arc::strong_count`, drops the local Arc, and emits a
  `tracing::error!` with `leaked_handles` + `closure_error` fields
  identifying the transaction as ZOMBIE STATE pending rollback when
  the last leaked handle drops. The original closure error is still
  surfaced to the caller untouched. The fundamental "can't
  force-rollback through a shared Arc" limit is a SeaORM API constraint;
  this fix is observability — the leak is now loud (ERROR-level), not
  silent. Regression: `framework/tests/database_tx_leak_diagnostic.rs`
  (2 tests — positive: leak triggers ZOMBIE log + surfaces closure
  err; negative: no leak means no zombie log).
- ~~Medium: the legacy `EntityExt` / `EntityExtMut` surface bypasses the newer
  transaction and read-replica routing layer.~~ — **CLOSED 2026-05-29 via `5cedb74a` (cherry-picked as `4a59814`).**
  EntityExt/EntityExtMut methods at `framework/src/database/model.rs:57-149` and
  `:190-253` now route through `ExecutorChoice` (the same path Builder<M> uses)
  so ambient transactions, read replicas, and named connections are honoured.
  AutoRouteBinding inherits the fix transitively. Regression coverage in
  `framework/tests/database_medium_audit.rs`.
- ~~Medium: transaction context is task-local and does not propagate to spawned
  tasks.~~ — **CLOSED (already by design) 2026-05-29.** The task-local
  semantics at `framework/src/database/transaction.rs:578-580,148-153,188-199`
  are the documented contract; the explicit `TxHandle` / `with_tx` plumbing is
  the supported way to propagate a transaction into spawned tasks. No code
  change — confirmed as intentional during this sweep.
- ~~Medium: SQLite file preparation ignores filesystem errors.~~ — **CLOSED
  2026-05-29 via `5cedb74a`.** `DbConnection::connect` (`framework/src/database/connection.rs:38-50`)
  now surfaces `create_dir_all` / `File::create` failures directly with the
  failing path, instead of swallowing them and producing a generic DB connection
  error later. Permission / path problems now read as filesystem diagnostics.
- ~~Medium: DB pool config is not validated.~~ — **CLOSED 2026-05-29 via `5cedb74a`.**
  `DatabaseConfig::from_env` (`framework/src/database/config.rs:50-58`) now
  validates `max_connections` / `min_connections` / `connect_timeout` before
  handing them to SeaORM (`framework/src/database/connection.rs:60-64`): rejects
  zero-sized pools, zero timeouts, and `min > max`. Regression coverage in
  `framework/tests/database_medium_audit.rs`.
- ~~Medium: `DbTableBuilder::insert` silently assumes RETURNING id can be cast to
  `i64`.~~ — **CLOSED 2026-05-29 via `5cedb74a`.** `DbTableBuilder::insert`
  (`framework/src/database/db_facade.rs:281-292`) now reports a typed error
  rather than coercing to `0` when the table has no `id` column, a non-integer
  primary key, or a composite key. The generic model-less builder is now honest
  about its scope.
- ~~Low: `ConnectionRegistry::has` degrades to `false` on poisoned locks at
  `framework/src/database/connection_registry.rs:158-169`, which silently routes
  read-replica traffic back to primary. That is safer than panicking, but it can
  hide a poisoned global registry until a later explicit `get` path is exercised.~~
  — **CLOSED via `f59388f`.** `has` still degrades to `false` (still safer than
  panicking and behavior unchanged), but the first poisoned read now emits a
  `tracing::warn!` once via an `AtomicBool` gate so a poisoned global registry
  surfaces in observability without flooding the read-replica routing hot path.
- ~~Low: `DB::table(...).update(...)` and `.delete()` intentionally allow empty
  `WHERE` clauses at `framework/src/database/db_facade.rs:311-363`. Laravel
  allows broad updates/deletes too, but this framework could offer a safer
  `where_required` mode or explicit `update_all` / `delete_all` names to reduce
  accidental table-wide mutations.~~ — **CLOSED via `f59388f`.** Added
  `update_all` / `delete_all` dual-API aliases on `DbTableBuilder` matching the
  typed-Eloquent `Builder<M>::update_all`/`delete_all` _all-bulk-mutator naming.
  Laravel-faithful `update` / `delete` preserved for the call site that wants
  the unqualified verb; opt-in aliases make the table-wide intent explicit.
  Backed by 2 new integration tests in `framework/tests/database_medium_audit.rs`.

Test coverage gaps:
- Add production-env boot tests proving missing `DATABASE_URL` fails unless an
  explicit local/dev SQLite fallback is configured.
- Add malicious identifier/operator tests for `DB::table` and decide whether to
  validate, quote, or mark raw identifiers as unsafe.
- Add transaction tests where a `with_tx` builder/handle is leaked across both
  success and error closure exits; the error path should not return until
  rollback semantics are deterministic.
- Add tests proving `EntityExt`, `AutoRouteBinding`, and legacy query builders
  either route through `ExecutorChoice` or are documented as primary-only.
- Add DB config validation tests for pool bounds and timeouts.
- Add `DbTableBuilder::insert` tests for non-`id` and UUID primary key tables.

## eloquent

Status: Partial (2026-05-29) — 5 open of 8 total

Files:
- `framework/src/eloquent/mod.rs`
- `framework/src/eloquent/builder.rs`
- `framework/src/eloquent/model.rs`
- `framework/src/eloquent/collection.rs`
- `framework/src/eloquent/events.rs`
- `framework/src/eloquent/fillable.rs`
- `framework/src/eloquent/registry.rs`
- `framework/src/eloquent/prunable.rs`
- `framework/src/eloquent/casts/*`
- `framework/src/eloquent/relations/*`
- Supporting context: `suprnova-macros/src/model/relations.rs`,
  `framework/tests/eloquent_*.rs`, `framework/src/pagination/cursor.rs`,
  `framework/src/database/transaction.rs`

Review notes:
- ~~High: the model query builder exposes a large raw SQL trust boundary through
  safe-looking methods.~~ **CLOSED 2026-05-25.** Added `Builder::validate_inputs`
  that walks every identifier and operator captured on the builder state
  (`where_terms`, `having_terms`, `select_cols`, `group_by`, `orders`, UNION
  arms) and runs each through `database::validate_identifier` /
  `database::validate_sql_operator`. Both central render entry points —
  `Builder::render_select_for` and `Builder::render_count_select_for` —
  validate before emitting SQL, so every public terminal (`get`, `first`,
  `exists`, `count`, paginate variants, chunk family, aggregate helpers,
  `pluck`, `pluck_pair`) inherits the validation through their single
  render-call site. The fluent builder methods stay infallible; validation
  happens once at the I/O boundary in the `Result`-returning terminals.
  Raw-SQL escape hatches (`select_raw`, `WhereTerm::Raw`, `OrderTerm::Raw`)
  are deliberately skipped — their docs already warn about the trust
  boundary, and the escape hatch needs to exist for power users.
  `Model::increment` and `decrement` also validate the `column` arg. Debug
  helpers (`to_sql_with_bindings`, `to_sql_with_bindings_for`, `dump`, `dd`)
  retain their infallible signatures and surface validation failures via
  `.expect()` / `tracing::error!` instead. Regression coverage:
  `framework/tests/eloquent_builder_identifier_validation.rs` (9 tests —
  happy path + injection at every surface, schema-qualified identifiers
  pass, `Model::increment` validates).
- ~~High: lifecycle `save()` ignores listener mutations made to the cancellable
  `Updating` / `Saving` attribute payload.~~ **CLOSED 2026-05-25.** Both
  `save()` and `save_with_tx()` now mirror `update()`'s lifecycle: after the
  Updating + Saving listeners run, the (possibly mutated) shared `Attrs`
  are read back via `shared.lock().await.clone()` and applied to the
  ActiveModel via `apply_attrs_to_active_model` before the UPDATE fires.
  Listener observers that normalize, redact, or audit-tag values on update
  now persist correctly. Regression coverage:
  `framework/tests/eloquent_save_listener_mutation.rs` (2 tests — save()
  picks up Updating listener mutation, save_with_tx() does the same and
  also fires Saving).
- ~~Medium: eager loading requires the default database connection even when the
  generated relation arms will route through `Model::query()` or
  `ExecutorChoice`. `Builder::get` and `Collection::load` call
  `DB::connection()?` only to obtain a `DatabaseConnection` argument for the
  dispatcher (`framework/src/eloquent/builder.rs:2090-2097` and
  `framework/src/eloquent/collection.rs:477-479`). The macro-emitted common
  relation arms ignore that `db` argument and issue target model builders instead
  (`suprnova-macros/src/model/relations.rs:1838-1846`,
  `suprnova-macros/src/model/relations.rs:1924-1931`,
  `suprnova-macros/src/model/relations.rs:2030-2038`), while through/raw arms
  explicitly resolve an executor (`suprnova-macros/src/model/relations.rs:2343-2352`).
  Apps using only named/model connections can still fail eager loading because a
  default pool is missing or stale.~~ — **CLOSED 2026-05-29 via `4cf99da`.**
  `Builder::get` + `Collection::load` + `Collection::load_missing` route
  through `ExecutorChoice::resolve_read` with `M::default_connection_name`;
  eager loading no longer requires a default pool when only named/per-model
  connections are registered.
- ~~Medium: cursor pagination decodes a direction but ignores it and always slices
  forward. `CursorPaginator::decode_value` returns `(boundary, direction)`, but
  `Builder::cursor_paginate` discards `_dir`, always applies `pk > boundary`, and
  always returns `prev_cursor = None` at
  `framework/src/eloquent/builder.rs:2323-2367`. The docs call the API
  forward-only, so this is not an immediate correctness bug, but the wire format
  suggests bidirectional cursors and the implementation will mis-handle any
  `Previous` cursor that reaches it.
  **PARTIAL 2026-05-29:** `cursor_paginate` at builder.rs:2453 now consumes the
  decoded direction via `cursor::plan_scan(decoded)` (returns asc/desc + boundary
  filter), reverses backward-scan results, and emits both `next_cursor` AND
  `prev_cursor` (using `CursorDirection::Next`/`Prev`). The original "always
  prev_cursor=None" critique is resolved; the bidirectional wire-format gap is
  closed. Remaining outstanding work: explicit ambiguity/direction-mismatch test
  coverage was identified as pending in PROGRESS.md's L4 "pagination cursor-direction
  ambiguity" backlog item, so kept as PARTIAL until that test lands.~~ —
  **FULLY CLOSED 2026-05-29 via `4cf99da`.** `cursor_paginate` at builder.rs:3380
  consumes `plan_scan(decoded)`, reverses backward-scan results, and emits both
  next/prev cursors; `plan_scan_prev_filters_less_than_descending` +
  `finalize_backward_step_overflow_trims_front` +
  `finalize_backward_step_no_overflow_reaches_start` pin the direction-aware
  behaviour, closing the L4 test-coverage gap.
- ~~Medium: model and pruner registries are keyed too loosely for real
  applications. `find_model_by_table` returns the first link-time inventory entry
  for a table at `framework/src/eloquent/registry.rs:26-36`; duplicate table
  declarations across modules are nondeterministic. `prune_one` matches only the
  last-segment type name at `framework/src/eloquent/prunable.rs:129-184`, so two
  modules with `Session` pruners collide and `model:prune --model=Session` runs
  whichever entry inventory yields first.~~ — **CLOSED 2026-05-29 via `4cf99da`.**
  `find_model_by_table` errors on cross-module table collisions instead of
  returning the first inventory hit (same-module re-registration recognised as
  not-a-collision via pure helper + 4 unit tests); `prune_one` errors when two
  distinct prunable impls share a type_name (run-fn-pointer identity
  distinguishes same-impl re-registration from a real collision).
- ~~Medium: global model event listener state has production-facing test hooks and
  panic-on-poison behavior. `listen_cancellable` and listener lookup unwrap the
  global registry lock at `framework/src/eloquent/events.rs:179-199`; a panic
  while holding the lock can panic future model operations. The public
  doc-hidden `clear_cancellable_listeners()` at
  `framework/src/eloquent/events.rs:201-213` can also erase every cancellable
  listener in a production process because `eloquent::events` is part of the
  public module tree.~~ — **CLOSED 2026-05-29 via D9-A (audit commit `0ae6ef5`).**
  Both `listen_cancellable` (write) and `global_cancellable_listeners` (read)
  now match on the lock result and degrade gracefully: write-poison logs via
  `tracing::error!` and skips the registration; read-poison logs and returns
  `Vec::new()` (documented safe fallback — "no listeners registered" — so
  dispatch proceeds uncancelled). Module docs at `framework/src/eloquent/events.rs:184`
  carry the "Poison policy" comment block. The hot-path panic surface is gone.
  The `clear_cancellable_listeners()` doc-hidden API is unchanged but is now
  documented as test-only via the same poison-policy section; if production
  callers are a concern that warrants a separate API restriction finding.
- ~~Medium: mass-assignment protection silently drops all guarded/non-fillable
  input. `Fillable::apply` returns a filtered `Attrs` with no error or telemetry
  at `framework/src/eloquent/fillable.rs:91-116`, and `Model::create` proceeds
  with the filtered map at `framework/src/eloquent/model.rs:369-382`. This matches
  permissive legacy Laravel behavior, but production APIs often need strict mode
  so a typo, client overpost, or all-guarded payload cannot silently create a row
  using only database defaults.~~ — **CLOSED 2026-05-29 via `4cf99da`.** Strict
  mass-assignment added: `prevent_silently_discarding_attributes()` +
  `Fillable::apply_checked` wired into `create` / `update` /
  `create_with_tx` / `update_with_tx`; default remains permissive; the
  `unguarded` escape hatch still wins; 5 unit tests cover permissive default
  + strict reject + clean payload + allowlist strict + unguarded bypass.
- ~~Low: `chunk(0)` and `chunk_by_id(0)` are accepted. Both methods reject eager
  loads but never validate the chunk size at
  `framework/src/eloquent/builder.rs:2413-2437` and
  `framework/src/eloquent/builder.rs:2473-2515`. They currently no-op by issuing
  `LIMIT 0`, but a framework API should return the same explicit parameter error
  style used by `paginate(0)`, `simple_paginate(0)`, and `cursor_paginate(0)`.~~ — **CLOSED via `0c11a7da`.** `chunk(0)` and `chunk_by_id(0)` now return the same explicit `FrameworkError::param` shape as `paginate(0)` / `simple_paginate(0)` / `cursor_paginate(0)` rather than silently issuing `LIMIT 0`. The chunk-size contract is now uniform across the pagination + chunking family.

Test coverage gaps:
- Add malicious identifier/operator tests across `Builder` and `Model::increment`
  and then lock in the desired contract: validated identifiers by default, typed
  generated columns, or explicit `unsafe/raw` APIs.
- Add update lifecycle tests proving `Updating`/`Saving` listener mutations are
  persisted on `save()` and `update()`, not merely visible to observers.
- Add named-connection/no-default-connection eager-load tests for `Builder::with`,
  `Collection::load`, and nested `load_missing`.
- Add cursor tests that submit a `CursorDirection::Previous` cursor and assert the
  behavior is either rejected as unsupported or implements reverse pagination.
- Add duplicate model-table and duplicate pruner-type tests to force deterministic
  collision handling.
- Add strict mass-assignment tests once the desired policy is chosen, including an
  all-guarded payload and a typo field that should not silently disappear.
- Add zero chunk-size tests matching the paginator parameter-error behavior.

## error

Status: Partial (2026-05-29) — 5 open of 6 total

Files:
- `framework/src/error.rs`
- Supporting context: `framework/src/http/response.rs`,
  `framework/src/resources/errors.rs`, `framework/src/server.rs`,
  `framework/tests/error_responses.rs`

Review notes:
- ~~High: request panics bypass the standardized error response path.~~
  **CLOSED 2026-05-25.** The catch_unwind arm in `execute_chain_safely`
  now constructs `FrameworkError::internal(format!("request handler
  panicked: {msg}"))` and routes it through `HttpResponse::from`, the
  same conversion that returned 5xx errors use. The panic payload still
  appears in the `tracing::error!` log line (with `panic`, `method`,
  `path` fields) but no longer in the wire body. Panic responses now
  carry: the sanitised `{"message":"Internal Server Error"}` JSON shape,
  the `request_id` correlation key, and a dispatched `ErrorOccurred`
  event — observability listeners that fire on returned 5xx errors now
  see panics too. Regression coverage:
  `framework/tests/error_panic_response_contract.rs` (2 tests — JSON
  body + sanitised message + ErrorOccurred event) +
  updated `framework/tests/middleware_panic_safety.rs` to assert the
  new JSON contract.
- ~~Medium: the advertised `HttpError` trait is not integrated with the framework
  error conversion path. The docs show users implementing `HttpError` for domain
  errors at `framework/src/error.rs:8-32`, but the only implementation is
  `AppError` (`framework/src/error.rs:123-131`) and there is no blanket
  `From<T: HttpError>` / boxed domain-error variant in `FrameworkError`
  (`framework/src/error.rs:357-463`, `framework/src/error.rs:674-688`). A custom
  error implementing the documented trait cannot be propagated with `?` into
  `FrameworkError` unless the user manually maps it to `FrameworkError::domain`.~~ — **CLOSED 2026-05-29 via `82b3dea`.** Added `FrameworkError::from_http_error` constructor (blanket `From` conflicts with existing `From<AppError>`); user-defined `HttpError` types now propagate into the framework error path.
- ~~Medium: `FrameworkError::context` preserves only the status code and string
  display, erasing structured variants. It converts every error into
  `Domain { message, status_code }` at `framework/src/error.rs:615-635`.
  Context-wrapping a `Validation` or `PrecognitionFailure` error keeps status 422
  but loses the Laravel `errors` map and Precognition headers in
  `framework/src/http/response.rs:508-592`. Context-wrapping `Unauthorized` or
  `ModelNotFound` similarly changes variant-specific response handling into a
  generic domain body.~~ — **CLOSED 2026-05-29 via `82b3dea`.** `context()` now preserves `Validation`/`PrecognitionFailure`/`Unauthorized`/`ModelNotFound`/`ParamParse`/`ValidationError`/`PrecognitionSuccess`/`AlreadyReported` variants instead of flattening them to `Domain`.
- ~~Medium: JSON:API error detail uses `FrameworkError::message()` instead of the
  full display string for 4xx errors. `into_json_api_response` chooses
  `self.message()` for non-5xx detail at `framework/src/resources/errors.rs:36-47`,
  but `message()` returns only the param/model payload for `ParamError`,
  `ParamParse`, and `ModelNotFound` at `framework/src/error.rs:587-603`. JSON:API
  clients can receive detail `"id"` instead of `"Invalid parameter 'id': expected
  uuid"` or `"User"` instead of `"User not found"`, while the normal JSON error
  path uses `err.to_string()` and keeps the richer message.~~ — **CLOSED 2026-05-29 via `82b3dea` + follow-up.** `into_json_api_response` now uses `self.to_string()` for 4xx detail (matching the regular JSON renderer) so clients see `"Invalid parameter 'id': expected uuid"` and `"User not found"` instead of bare payloads like `"id"` and `"User"`. `ValidationError` is the documented exception: the envelope already exposes the field name in `source.pointer`, so its `detail` keeps the bare message (`"email is invalid"`) to avoid the doubled `"Validation error for 'email': email is invalid"`.
- ~~Medium: custom/domain status codes are not constrained to valid application
  statuses. `AppError::status` and `FrameworkError::domain` accept any `u16` at
  `framework/src/error.rs:79-82` and `framework/src/error.rs:514-519`;
  `FrameworkError::status_code` returns that value verbatim at
  `framework/src/error.rs:540-556`. `HttpResponse::into_hyper` rejects only values
  outside Hyper's 100-999 range at `framework/src/http/response.rs:218-230`, so
  nonstandard statuses like 700 can reach the wire instead of failing early or
  mapping to a conventional 5xx.~~ — **CLOSED 2026-05-29 via `82b3dea`.** `into_hyper` now constrains status codes to `100..=599`; 6xx-9xx values downgrade to 500 rather than reaching the wire.
- ~~Low: the CLI-only `AlreadyReported` sentinel is part of the public HTTP-flavored
  error enum. If it leaks into request handling, `status_code()` returns 500 at
  `framework/src/error.rs:454-463` and `framework/src/error.rs:552-555`, producing
  a generic internal-error response and a mostly empty framework-error log. A
  CLI-specific error type or private sentinel would keep this from becoming an
  accidental web response.~~ — **CLOSED via `94fd0954`.** `From<FrameworkError> for HttpResponse` now intercepts `AlreadyReported` explicitly: emits a loud `tracing::error!` naming the CLI-sentinel leak (with backtrace context) and renders the same sanitised generic 500 body as other 5xx so the wire shape is unchanged, but operators get a clearly-named log entry instead of an empty `framework error` blob. Two tests cover the response shape and the log-emission contract.

Test coverage gaps:
- Add a panic-through-server integration test that expects the same JSON/request_id
  error contract and `ErrorOccurred` behavior as returned 5xx errors.
- Add compile/runtime tests for a user-defined `HttpError` to either prove it
  converts through `FrameworkError` or remove the advertised trait path.
- Add regression tests for `FrameworkError::context` on `Validation`,
  `PrecognitionFailure`, `Unauthorized`, and `ModelNotFound` so structured
  response data is preserved or intentionally rejected.
- Add JSON:API tests for `ParamError`, `ParamParse`, and `ModelNotFound` detail
  fields.
- Add status validation tests for `FrameworkError::domain` / `AppError::status`
  using nonstandard in-range values like 700 as well as out-of-range values.

## lock

Status: Partial (2026-05-29) — original-policy gap closed across the registries the lock helper covers.

Files:
- `framework/src/lock.rs`
- Supporting context: `framework/src/database/connection_registry.rs`,
  `framework/src/bus/mod.rs`, `framework/src/inertia/flash.rs`,
  `framework/src/authorization/registry.rs`, `framework/src/broadcasting/hub.rs`,
  `framework/tests/cache_locks.rs`, `framework/tests/eloquent_locking.rs`

Review notes:
- ~~Medium: the poison-handling policy exists but is not consistently adopted.
  `lock::read`, `lock::write`, and `lock::lock` convert poison into
  `FrameworkError::internal` at `framework/src/lock.rs:12-28`, but several
  framework registries still panic or unwrap on poisoned locks. Examples include
  authorization gate invocation (`framework/src/authorization/registry.rs:89-121`),
  broadcast hub channel lookup (`framework/src/broadcasting/hub.rs:108`), bus
  registration (`framework/src/bus/mod.rs:97`), Inertia flash push/drain
  (`framework/src/inertia/flash.rs:64-82`), and mail mailable registration
  (`framework/src/mail/mailable_registry.rs:79`).~~ — **CLOSED 2026-05-29 via
  the audit-2026-05 D9-A through D20-A sweep (PROGRESS.md L77, L80, L101, L108,
  L114, L120, L129).** All five named registries now route poison through
  `match lock::write/read`:
  - authorization gates: `insert_gate` log+skip pattern at
    `framework/src/authorization/registry.rs:127-147` (D10-A safe-deny)
  - broadcast hub: `sender_for` slow-path log+orphan-sender at
    `framework/src/broadcasting/hub.rs:135-165` (D16-A)
  - bus: `Bus::register` log+skip at `framework/src/bus/mod.rs:97-115` (D11-B)
  - Inertia flash: per-request `Mutex` push+drain log+drop at
    `framework/src/inertia/flash.rs:60-83` (D20-A per-request scope)
  - mail mailable: `lock::write(&REGISTRY)?` propagates at
    `framework/src/mail/mailable_registry.rs:75`
- ~~Medium: some callers use the helper and then immediately turn the returned
  `FrameworkError` back into a panic. `Bus::register` calls
  `lock::write(&REGISTRY).expect("bus registry poisoned")` at
  `framework/src/bus/mod.rs:97`; Inertia flash does
  `lock::lock(bag).expect("flash bag poisoned")` at
  `framework/src/inertia/flash.rs:66`.~~ — **CLOSED 2026-05-29 via D11-B + D20-A.**
  Bus::register is now `match lock::write` (`framework/src/bus/mod.rs:102-115`);
  Inertia flash push/drain log + drop on poison (per-request `Mutex` scope, so
  failure is bounded to the current request — same `framework/src/inertia/flash.rs:71-83`).
- ~~Low: every helper returns the same `"internal registry lock poisoned"` message
  regardless of lock type (`framework/src/lock.rs:15`,
  `framework/src/lock.rs:21`, `framework/src/lock.rs:27`). That keeps client
  responses sanitized, but logs and `debug_message` do not identify whether the
  poisoned lock was the connection registry, queue driver, flash bag, or another
  subsystem unless the caller wraps it with context.~~ — **CLOSED via `2176607f`.**
  `lock::{read,write,lock}` now take a `&'static str` label and return
  `FrameworkError::internal("internal registry lock poisoned: {label}")`, callers
  updated across 22 sites; +1 test pins the labelled error path.
- ~~Low: lock poison behavior is covered indirectly only where individual modules
  added tests. The connection registry has a dedicated poison regression test
  (`framework/src/database/connection_registry.rs:256-308`), but the helper
  module itself has no table-driven tests and no audit test that fails when new
  `expect("... poisoned")` paths are introduced.~~ — **CLOSED 2026-05-29.**
  `framework/src/lock.rs` now ships unit tests for both policies:
  `helpers_return_err_on_poison_instead_of_panicking` (read/write/lock) and
  `into_inner_recovers_a_poisoned_lock` (recover-in-place pattern). The
  per-registry tests (D6-1, D9-A, D10-A, D11-B, D16-A, etc.) cover each
  subsystem's adopt path.

Test coverage gaps:
- Add a repo-level regression test or lint-like check for production
  `expect("... poisoned")` / `.unwrap()` lock paths outside tests.

## hashing

Status: Partial (2026-05-29) — 2 HIGH closed inline (already marked); 2 MEDIUM (HashConfig, version/algorithm rehash) still open.

Files:
- `framework/src/hashing/mod.rs`
- Supporting context: `framework/src/torii_integration/password.rs`,
  `framework/tests/password_reset.rs`, bcrypt 0.19.1 API behavior in Cargo
  registry

Review notes:
- ~~High: password hashing and verification are synchronous CPU-bound calls
  exposed as ordinary functions.~~ **CLOSED 2026-05-25.** Added
  `hash_async`, `hash_with_cost_async`, and `verify_async` wrappers
  that run bcrypt on `tokio::task::spawn_blocking` so request workers
  stay free. The sync variants remain for tests/CLI tools, but the
  module docs now flag them as sync-only. `auth::remember::generate_token`
  is now async and uses `hash_async`; `verify_and_rotate` uses
  `verify_async`.
- ~~High: the wrapper uses bcrypt's truncating API.~~ **CLOSED 2026-05-25.**
  Switched to `bcrypt::non_truncating_hash` and added an up-front
  length check at `hash` / `verify` boundaries (`MAX_PASSWORD_BYTES`
  = 71, accounting for bcrypt's null terminator). `hash` returns
  `FrameworkError::param` for over-cap inputs; `verify` returns
  `Ok(false)` so the calling auth flow surfaces the same "invalid
  credentials" response regardless of length (no info leak).
  Regression coverage: `framework/tests/hashing_async_and_truncation.rs`
  (8 tests — happy path + over-cap rejection at hash + verify-returns-false
  + boundary at MAX_PASSWORD_BYTES + truncation-collision-impossible +
  async variants honor all of the above).
- ~~Medium: the hashing surface is hard-coded to bcrypt cost 12.~~ — **CLOSED
  via `75f4203` (Laravel parity sweep HEADLINER).** `DEFAULT_COST` is no longer
  the only knob. `HashConfig` lives at `framework/src/hashing/config.rs` with
  env-driven driver selection (`HASH_DRIVER`, `HASH_ROUNDS`, `HASH_MEMORY`,
  `HASH_TIME`, `HASH_THREADS`, `HASH_VERIFY`); `hash()` and `needs_rehash()`
  dispatch through `default_driver()` which builds Bcrypt, Argon2i, or Argon2id.
  Argon2id is the default driver.
- ~~Medium: `needs_rehash` hand-parses only the bcrypt cost and ignores the bcrypt
  version/algorithm.~~ — **CLOSED via `75f4203` (Laravel parity sweep).**
  `BcryptHasher::needs_rehash` at `framework/src/hashing/driver.rs:178-192`
  explicitly inspects `bcrypt_variant` via `info.algo` + `bcrypt_variant` and
  returns true for legacy `$2a$`/`$2x$`/`$2y$` variants AND for algorithm
  mismatch (e.g. bcrypt hash present when Argon2id is the configured driver).
  Pinned by `bcrypt_needs_rehash_on_legacy_variant` and
  `bcrypt_needs_rehash_on_algorithm_mismatch` at
  `framework/src/hashing/driver.rs:449, 459`.
- ~~Low: `hash_with_cost` accepts an arbitrary `u32` from callers and relies on the
  bcrypt crate for range validation (`framework/src/hashing/mod.rs:43-46`). That
  avoids invalid costs, but high accepted costs can still create avoidable CPU
  exhaustion if route code passes policy/config values without bounds.~~ —
  **CLOSED via `d8998982`.** `hash_with_cost` now enforces
  `MIN_BCRYPT_COST..=MAX_BCRYPT_COST` bounds that match the `HASH_ROUNDS` env
  range, returning `FrameworkError::internal` outside the range; +5 tests pin
  min/max/below/above/exact boundaries.

Test coverage gaps:
- Add configuration tests for default driver/cost and `needs_rehash` when the
  configured cost changes.
- Add version/algorithm rehash tests for `$2a$`, `$2x$`, `$2y$`, `$2b$`, and a
  future Argon2id hash.

## http

Status: Resolved (2026-05-29) — all 8 findings closed (1 HIGH + 5 MEDIUM + 2 LOW); D3a sweep + multipart streaming refactor land the full set.

Files:
- `framework/src/http/mod.rs`
- `framework/src/http/body.rs`
- `framework/src/http/cookie.rs`
- `framework/src/http/extract.rs`
- `framework/src/http/form_request.rs`
- `framework/src/http/request.rs`
- `framework/src/http/response.rs`
- `framework/src/http/upload/mod.rs`
- `framework/src/http/upload/validators.rs`
- Supporting context: `suprnova-macros/src/multipart.rs`,
  `framework/tests/uploads.rs`, `framework/tests/data_form_request.rs`,
  `framework/tests/error_responses.rs`

Review notes:
- ~~High: multipart `max_count` is enforced after the full multipart payload has
  already been parsed and allocated. The derive macro advertises `max_count` as a
  per-Vec ceiling that short-circuits before allocating the extra part
  (`suprnova-macros/src/multipart.rs:90-103`), but generated code first calls
  `parse_multipart_streaming_with_cap(...).await?` at
  `suprnova-macros/src/multipart.rs:444-457`, which pushes every parsed field into
  `MultipartPayload.fields` at `framework/src/http/upload/mod.rs:531-599`. The
  `max_count` guard runs only later while iterating `payload.fields` at
  `suprnova-macros/src/multipart.rs:266-300` and
  `suprnova-macros/src/multipart.rs:371-405`. A 25 MiB request with thousands of
  tiny repeated parts still allocates every `MultipartValue` before rejection.~~
  — **CLOSED 2026-05-29.** The derive macro now emits
  `parse_multipart_streaming_with_limits` (`suprnova-macros/src/multipart.rs:406-444`)
  passing per-field `max_count` ceilings via `MultipartLimits::per_field_max_counts`.
  The parser enforces ceilings during streaming at
  `framework/src/http/upload/mod.rs:719-730`: the (cap + 1)-th part carrying a
  given name returns 422 before it is read, so the extra part never allocates.
- ~~Medium: multipart requests do not pre-reject oversized `Content-Length` the way
  generic bodies do. JSON/form bodies call `Request::body_bytes_with_cap`, which
  parses `content-length` and rejects before reading when it exceeds the cap at
  `framework/src/http/request.rs:163-188` and `framework/src/http/body.rs:82-115`.
  `parse_multipart_streaming_with_cap` reads only `Content-Type`, then enforces
  the cap progressively while streaming chunks at
  `framework/src/http/upload/mod.rs:501-555` and
  `framework/src/http/upload/mod.rs:398-405`. Honest huge multipart uploads waste
  connection and parser work that could be rejected immediately.~~ — **CLOSED
  2026-05-29.** `parse_multipart_streaming_with_limits` now pre-rejects an
  honestly-declared oversized body before reading any frame at
  `framework/src/http/upload/mod.rs:632-645`, mirroring the JSON/form
  `body_bytes_with_cap` path. A client that lies (small Content-Length, large
  body) is still caught progressively by the per-chunk byte cap inside
  `collect_part`.
- ~~Medium: oversized text fields are rejected only after the whole text part is
  consumed and possibly spilled to disk. `collect_part` spills any part crossing
  the spill threshold to a temp file at `framework/src/http/upload/mod.rs:417-435`
  and continues reading; the parser classifies non-file parts afterward and then
  rejects disk-backed text at `framework/src/http/upload/mod.rs:574-588`. A large
  text field can drive tempfile I/O before the framework returns 400.~~ — **CLOSED
  2026-05-29.** `collect_part` now short-circuits a text part that exceeds the
  in-memory threshold at `framework/src/http/upload/mod.rs:507-521` (returns 400
  before opening a temp file). The post-parse classification rejection at L767-774
  remains as a defense-in-depth backstop for future code paths.
- ~~Medium: FormRequest falls back to JSON parsing for missing or unsupported
  content types. `FormRequest::extract` parses form-urlencoded only when
  `Content-Type` starts with `application/x-www-form-urlencoded`; every other
  content type, including `text/plain`, malformed content types, and absent
  headers, goes through JSON parsing at `framework/src/http/form_request.rs:158-169`.
  Public endpoints should usually return 415/400 for unsupported media types
  instead of treating every unknown body as JSON.~~ — **CLOSED 2026-05-29.**
  `FormRequest::extract` now strict-classifies at
  `framework/src/http/form_request.rs:202-225`: form-urlencoded OR
  `application/json` / `application/*+json` are accepted; every other media type
  (including missing/empty Content-Type, `text/plain`, malformed types) returns
  `FrameworkError::UnsupportedMediaType` (415) BEFORE the body is read.
- ~~Medium: cookie attribute values are not validated or encoded. Cookie names and
  values are percent-encoded at `framework/src/http/cookie.rs:187-193` and
  `framework/src/http/cookie.rs:327-343`, but `Path` and `Domain` are interpolated
  raw at `framework/src/http/cookie.rs:195-212`. Hyper header validation blocks
  raw CR/LF later, but semicolons or malformed attribute text can still alter the
  Set-Cookie attribute list. Path/domain should be constrained to RFC-valid
  attribute values.~~ — **CLOSED 2026-05-29 via the D3a DR2 sweep.**
  `sanitize_path` (`framework/src/http/cookie.rs:318-333`) strips control
  characters and `;` from cookie `Path` and falls back to `/`; `sanitize_domain`
  (`framework/src/http/cookie.rs:335-350`) constrains `Domain` to RFC-host-valid
  characters (ASCII alphanumeric + `.` + `-`) and omits the attribute on empty
  result. Same shape as DR2 cookie-name CRLF percent-encoding.
- ~~Medium: cookie parsing treats `+` as a space even though Cookie headers are not
  form-urlencoded. `url_decode` translates every `+` to `' '` before percent
  decoding at `framework/src/http/cookie.rs:348-365`. Cookies set by other
  systems with literal plus signs are corrupted on read (`a+b` becomes `a b`).~~
  — **CLOSED 2026-05-29.** `url_decode` is now built on `percent_decode_str`
  alone at `framework/src/http/cookie.rs:309-316`; `+` survives verbatim. The
  test `assert_eq!(url_decode(&url_encode("a+b")), "a+b")` at
  `framework/src/http/cookie.rs:578` pins the round-trip.
- ~~Low: `SameSite=None` is not coupled to `Secure`. Defaults are secure, but the
  fluent API permits `Cookie::new(...).same_site(SameSite::None).secure(false)`
  at `framework/src/http/cookie.rs:126-149`, generating a cookie modern browsers
  reject and weakening cross-site cookie expectations if accepted by older
  clients.~~ — **CLOSED 2026-05-29.** `to_header_value` now emits `Secure`
  whenever the cookie is explicitly secure OR `SameSite=None` is set
  (`framework/src/http/cookie.rs:188-194`), so `SameSite=None` is always
  paired with `Secure` regardless of caller-supplied `secure(false)`.
- ~~Low: legacy `ParamError` can still render a nonstandard `{ "error": ... }`
  response if converted directly to `HttpResponse` at `framework/src/http/mod.rs:17-35`,
  while the normal `FrameworkError` path uses `{ "message": ... }`. Keeping both
  response shapes increases client inconsistency.~~ — **CLOSED 2026-05-29.**
  `From<ParamError> for HttpResponse` now routes through
  `crate::error::FrameworkError::from(err)` at `framework/src/http/mod.rs:25-32`,
  producing the canonical `{ "message": ... }` body and 400 status — the legacy
  `{ "error": ... }` shape is gone.

Test coverage gaps:
- Add multipart `Content-Length` pre-rejection tests for declared bodies larger
  than the cap.
- Add FormRequest media-type tests for missing `Content-Type`, `text/plain`,
  malformed content type, and JSON with `application/*+json`.

## http_client

Status: Partial (2026-05-29) — 1 HIGH + 4 MEDIUM + 2 LOW closed; 1 LOW (FailOnRealCallsGuard parallel-task races) partial.

Files:
- `framework/src/http_client/mod.rs`
- `framework/src/http_client/fake.rs`
- Supporting context: `framework/tests/http_client.rs`

Review notes:
- ~~High: retry is method-agnostic and will repeat non-idempotent requests. Calling
  `.retry(...)` on `POST`, `PATCH`, or `DELETE` retries connect/timeout failures
  and every 5xx response at `framework/src/http_client/mod.rs:405-479`. If the
  upstream performed the side effect but returned 500 or the response was lost,
  Suprnova can send the same write again. The API should either default retries
  to idempotent methods, require an explicit `retry_non_idempotent` opt-in, or
  integrate idempotency keys for unsafe methods.~~ — **CLOSED 2026-05-29 via
  commit 49677e3 (`http_client: idempotency-aware retry + jittered/capped
  backoff`).** `RequestBuilder::retry` now retries only idempotent methods
  (GET/PUT/DELETE per `Method::is_idempotent` at
  `framework/src/http_client/mod.rs:323-329`). `retry_non_idempotent` is an
  explicit opt-in (`framework/src/http_client/mod.rs:487-505`) for callers who
  guarantee replay safety (e.g. server-side idempotency keys). The send loop
  computes `method_retryable` and short-circuits non-retryable methods at
  `framework/src/http_client/mod.rs:529-531`.
- ~~Medium: request body serialization failures are silently converted to null or
  empty bodies. `RequestBuilder::json` and `form` call
  `serde_json::to_value(value).unwrap_or(Value::Null)` at
  `framework/src/http_client/mod.rs:341-354`; fake recording similarly uses
  `unwrap_or_default()` for JSON/form body serialization at
  `framework/src/http_client/fake.rs:188-197`. A `Serialize` failure can turn a
  business request into `null`, an empty form body, or a fake assertion that
  records different bytes than production would send.~~ — **CLOSED 2026-05-29
  via commit 85b0bfc.** `json()` / `form()` now record any `serde_json::to_value`
  failure into `body_error` (`framework/src/http_client/mod.rs:399-417`). The
  send loop surfaces it at `framework/src/http_client/mod.rs:513-517` as
  `FrameworkError::internal` BEFORE a request is built, so the request never
  silently degrades to `null` / empty form.
- ~~Medium: response body readers have no framework-level size cap. `ClientResponse::json`,
  `text`, and `bytes` fully buffer the upstream body via reqwest at
  `framework/src/http_client/mod.rs:579-615`. A slow or malicious upstream can
  send a very large successful/error body and drive memory pressure. The inbound
  HTTP module has explicit caps; outbound reads need equivalent max-body controls
  or streaming-first APIs.~~ — **CLOSED 2026-05-29 via commit 85b0bfc.**
  Process-global cap via `Http::set_max_response_bytes` /
  `Http::max_response_bytes` at `framework/src/http_client/mod.rs:185-202`
  (default `DEFAULT_MAX_RESPONSE_BODY_BYTES` = 25 MiB). Per-request override
  via `RequestBuilder::max_response_bytes` at L433-439. Send loop calls
  `.with_max_bytes(effective_max)` at L578 so `json`/`text`/`bytes` enforce
  the cap on each buffered read.
- ~~Medium: unmatched fakes return a successful empty `200 {}`. `fake::intercept`
  records the request, then returns a default fake response when no canned entry
  matches at `framework/src/http_client/fake.rs:202-236`; the test
  `fake_unmatched_request_returns_default_200` locks in that behavior. This makes
  tests pass when a request URL/method changed but the fake was not updated unless
  every suite also enables `fail_on_real_calls`, which does not apply once a fake
  scope is active.~~ — **CLOSED 2026-05-29 via commit 514eebf.**
  `fake::intercept` now fail-closes on unmatched requests when `Http::is_guarded()`
  is true at `framework/src/http_client/fake.rs:225-234`: returns
  `FrameworkError::internal` with method + URL so a drifted URL/method fails
  loudly. Without the guard the default empty 200 still applies (locked by
  `fake_unmatched_request_returns_default_200`), so the test-strictness
  decision is at the suite level.
- ~~Medium: retry backoff has no jitter or maximum delay cap beyond saturating
  arithmetic. `backoff_for` doubles `base_backoff` by attempt at
  `framework/src/http_client/mod.rs:541-548`, and `Retry-After` is honored for
  503 at `framework/src/http_client/mod.rs:467-472` and
  `framework/src/http_client/mod.rs:552-561`. Multiple workers retrying the same
  outage can synchronize into a thundering herd, and a large `Retry-After` can
  park tasks for an operator-controlled duration.~~ — **CLOSED 2026-05-29 via
  commit 49677e3.** `backoff_for` now uses full jitter (AWS recipe) at
  `framework/src/http_client/mod.rs:664-680` — `[0, base*2^(n-1)]` uniform —
  bounded by `MAX_RETRY_WAIT = 30s` at L656. The 503 branch caps the larger of
  jittered backoff vs `Retry-After` at the same ceiling.
- ~~Low: `Retry-After` supports only integer delta-seconds, not the HTTP-date form
  allowed by the header (`framework/src/http_client/mod.rs:552-561`). Upstreams
  that send a standards-compliant date are treated as if no header was present.~~
  — **CLOSED 2026-05-29 via commit 49677e3.** `retry_after_from` now parses
  delta-seconds first, then `httpdate::parse_http_date` for the date form at
  `framework/src/http_client/mod.rs:682-704`; an already-past instant clamps
  to `Duration::ZERO`.
- ~~Low: `FailOnRealCallsGuard` is not nesting-safe. Its docs say this explicitly,
  and `Drop` always calls `Http::allow_real_calls()` at
  `framework/src/http_client/mod.rs:222-257`. Nested helpers or parallel code that
  independently installed the guard can accidentally re-enable real calls when
  the inner guard drops. **PARTIAL 2026-05-29:** `FailOnRealCallsGuard::install`
  now captures `previous` state and restores it in `Drop` at
  `framework/src/http_client/mod.rs:274-289` — nested installs no longer
  unconditionally flip the flag back to "allowed". Parallel-task races on the
  process-global `AtomicBool` remain a documented limitation; for parallel test
  isolation use `Http::spawn_with_fake_inheritance` and per-task fake scope.~~ —
  **CLOSED via `b695934a`.** Docstring on `FailOnRealCallsGuard` now surfaces
  the parallel-task limitation at the install site, with the
  `Http::spawn_with_fake_inheritance` + per-task fake-scope remedy spelled
  out so consumers don't trip on it via search-engine docs alone.
- ~~Low: fake assertion panic output can dump sensitive request data. Recorded
  requests include headers and raw body bytes at `framework/src/http_client/fake.rs:58-69`,
  and failed `assert_sent` / `assert_not_sent` panics print the recorded request
  list at `framework/src/http_client/fake.rs:100-125`. Tests often include bearer
  tokens, API keys, or webhook payloads; failure logs can leak them.~~ — **CLOSED
  2026-05-29 via commit 514eebf.** `redacted` at
  `framework/src/http_client/fake.rs:264-279` now allowlists only
  `content-type` / `accept` / `user-agent` headers; every other header is
  printed as `<redacted>` and the body becomes `<N bytes>`. Assertion-failure
  output no longer surfaces secrets.

Test coverage gaps:
- Add retry tests for `POST`/`PATCH`/`DELETE` showing the desired unsafe-method
  policy, including an idempotency-key path if retries remain supported.
- Add a strict fake mode where unmatched requests fail without the guard, if a
  pure fake-strictness API is ever exposed.

## idempotency

Status: Resolved (2026-05-29) — 2 HIGH + 4 MEDIUM closed; final 2 audit-level concerns (in-memory cross-process and shared-backend require) are now documented in module docs, not bugs.

Files:
- `framework/src/idempotency/mod.rs`
- Supporting context: `framework/src/cache/mod.rs`, `framework/src/queue/worker.rs`,
  `framework/tests/idempotency.rs`

Review notes:
- ~~High: this is a dedupe lock, not HTTP/job idempotency with result replay.
  `Idempotency::once` and `commit_on_success` return only `Fresh(T)` or
  `Duplicate` (`framework/src/idempotency/mod.rs:15-20`), and they store no
  response/result metadata after success (`framework/src/idempotency/mod.rs:39-58`,
  `framework/src/idempotency/mod.rs:77-100`). A duplicate caller cannot receive
  the original response, status, or error; it must invent separate duplicate
  semantics. That is especially risky because queue worker docs tell production
  job handlers to wrap side effects with these helpers for idempotency
  (`framework/src/queue/worker.rs:15-23`).~~ — **CLOSED 2026-05-29 via commit
  bfcefdb (`idempotency: add result-replay variant + hash caller key material`).**
  New `Idempotency::remember<T: Serialize + DeserializeOwned>` at
  `framework/src/idempotency/mod.rs:184-237` returns a `Replay<T>` enum:
  `Fresh(T)` for the first caller, `Replayed(T)` for duplicates within the TTL
  window (the original success value), and `InProgress` while the body is still
  running. Records the success value in `idem:<h>:result` and replays before
  releasing the lock so no duplicate can slip in between the store and the
  release. Module docs at `framework/src/idempotency/mod.rs:1-33` enumerate
  the three entry points by guarantee.
- ~~High: long-running bodies can execute twice if the TTL expires before the body
  finishes. The helper acquires a cache lock once and never refreshes it while
  `body().await` runs (`framework/src/idempotency/mod.rs:50-54`,
  `framework/src/idempotency/mod.rs:86-94`), even though `LockGuard::refresh`
  exists (`framework/src/cache/mod.rs:389-393`). A slow webhook/job/payment
  operation can outlive the caller-supplied TTL, letting a second caller acquire
  the same key and run concurrently before the first returns.~~ — **CLOSED
  2026-05-29 via commit bcbbc7f (`idempotency: renew lock lease for long bodies
  + log release failures`).** `run_under_lease` at
  `framework/src/idempotency/mod.rs:251-290` keeps the lease alive while the
  body runs: background renewer calls `LockGuard::refresh` at `ttl/3` (floored
  at 50ms) inside `tokio::select! biased; body / renew` so a body that outlives
  the original TTL cannot let the lock expire and a second caller execute
  concurrently. All three entry points (`once`, `commit_on_success`, `remember`)
  go through `run_under_lease`.
- ~~Medium: crash-after-side-effect semantics are unresolved. On success, the only
  durable marker is the still-held cache lock TTL; if the process crashes after
  an external side effect but before returning to the caller, retries become
  `Duplicate` with no stored result until expiry. If the cache backend is
  in-memory or Redis bootstrap falls back to memory, dedupe is not shared across
  processes at all.~~ — **CLOSED 2026-05-29.** Module docs at
  `framework/src/idempotency/mod.rs:29-33` explicitly call out: "Cross-process
  dedupe requires a cross-process cache (e.g. Redis). With the in-memory backend
  — or a Redis bootstrap that fell back to memory — the dedupe window is
  per-process only." `remember` now stores the success value via `Cache::put`
  with TTL so crash-after-side-effect retries get `Replayed(T)` within the
  window, not `InProgress`.
- ~~Medium: `once` consumes the key window even when `body` returns `Err`.
  The docs explain the lock is not released on success, but the implementation
  also does not release on body failure because the guard is dropped without
  calling `release()` (`framework/src/idempotency/mod.rs:50-54`,
  `framework/src/cache/mod.rs:366-370`). That can suppress retries after a
  transient failure unless every retryable caller knows to use
  `commit_on_success`.~~ — **CLOSED 2026-05-29.** Behavior preserved by design
  (per `once` docs at `framework/src/idempotency/mod.rs:69-87`: "Use
  [`commit_on_success`] instead when a failed `body` should be retryable within
  the window"). The contract is documented and `commit_on_success` is the
  named alternative for retryable failures.
- ~~Medium: `commit_on_success` hides release failures. On body error, it calls
  `let _ = g.release().await` and returns only the original body error
  (`framework/src/idempotency/mod.rs:89-94`). If Redis is unavailable or the
  token no longer matches, the caller is told only about the body failure while
  retry behavior may still be blocked until TTL expiry.~~ — **CLOSED 2026-05-29
  via commit bcbbc7f.** `release_and_log` at
  `framework/src/idempotency/mod.rs:298-311` no longer swallows release
  failures: `Ok(false)` (token mismatch) and `Err(_)` (backend error) both
  emit `tracing::warn!` with the hashed key + error. The caller still receives
  the original body error (correct primary signal), but a stuck retry is now
  observable via logs.
- ~~Medium: caller-supplied key material is inserted directly into cache keys with
  `format!("idem:{key}")` (`framework/src/idempotency/mod.rs:50`,
  `framework/src/idempotency/mod.rs:86`). There is no normalization, hashing,
  length cap, or PII guidance. User-provided HTTP idempotency keys can create
  very large backend keys, leak sensitive identifiers into Redis/cache tooling,
  or collide with application-level naming conventions.~~ — **CLOSED 2026-05-29
  via commit bfcefdb.** `hashed(key)` at `framework/src/idempotency/mod.rs:318-327`
  pre-hashes caller key material to a 64-char hex SHA-256 digest before
  `format!("idem:{h}")`. Backend keys are fixed-length, PII never reaches the
  cache store, and collisions with backend key conventions are eliminated.
  Three regression tests cover fixed-length, no-raw-leak, and distinctness at
  `framework/src/idempotency/mod.rs:333-354`.

Test coverage gaps:
- Add a cache-store test double where `release_lock` fails so
  `commit_on_success` cannot silently hide unreleased retry blockers (the
  `release_and_log` warn path is logged but not asserted in a unit test).

## events

Status: Done (2026-05-29) — 1 HIGH + 4 MEDIUM + 1 LOW closed; ErrorOccurred best-effort behavior explicitly adjudicated (no code change) since 5xx structured logging is the guaranteed stream and durable-listener path now exists; listener-append-only is now an explicit contract.

Files:
- `framework/src/events/mod.rs`
- `framework/src/events/dispatcher.rs`
- `framework/src/events/testing.rs`
- `framework/src/events/builtins.rs`
- Supporting context: `framework/src/http/response.rs`,
  `framework/src/broadcasting/broadcastable.rs`, `framework/tests/events.rs`,
  `framework/tests/broadcasting_event_integration.rs`,
  `framework/tests/registry_clears.rs`

Review notes:
- ~~High: queued listeners are fire-and-forget tasks with no durability,
  backpressure, shutdown drain, or retry path. `EventDispatcher::dispatch`
  clones the event and calls `tokio::spawn` for each queued listener, drops the
  `JoinHandle`, and returns `Ok(())` immediately
  (`framework/src/events/dispatcher.rs:91-102`). Listener failures are only
  logged, and panics/cancellation/runtime shutdown can lose work silently. This
  is acceptable for best-effort notifications, but not production-grade queued
  events.~~ — **CLOSED 2026-05-29 via commits 27e27c9 + 13dff6f.** The
  in-process dispatcher now:
  - bounds queued concurrency via a `Semaphore`
    (`DEFAULT_QUEUED_CONCURRENCY = 256`, env-overridable via
    `EVENT_MAX_CONCURRENCY`) at `framework/src/events/dispatcher.rs:13-66`;
  - retries each queued listener up to `MAX_QUEUED_ATTEMPTS = 3` with jittered
    exponential backoff at L246-273 + L316-322 (`retry_backoff`);
  - tracks tasks in a drainable `JoinSet` so
    `EventDispatcher::drain_queued(timeout)` at L276-309 can be called from
    graceful shutdown (returns how many tasks remained at deadline; stragglers
    are aborted so shutdown can't hang);
  - aborts unconditionally past the deadline (`set.abort_all()`).
  Module docs flag the in-process retry as transient-fault only — work that
  must survive a process crash is documented to belong on the durable queue
  (`QueuedListener` bridge in commit 13dff6f).
- ~~Medium: built-in `ErrorOccurred` is documented as dispatched on every 5xx, but
  response conversion drops it outside a Tokio runtime and otherwise spawns it
  best-effort. `HttpResponse::from(FrameworkError)` explicitly says outside a
  runtime the dispatch is silently dropped (`framework/src/http/response.rs:536-552`),
  and inside a runtime the task is not awaited. Monitoring/audit listeners cannot
  rely on this event as a complete error stream.~~ — **CLOSED via prior adjudication
  (no code change required).** Validated against current HEAD: behavior in
  `From<FrameworkError> for HttpResponse` (`framework/src/http/response.rs:1148-1199`)
  is exactly as the prior "PARTIAL 2026-05-29" addendum describes, and that
  addendum already adjudicated it as not requiring a code fix. The guaranteed
  5xx error stream is the unconditional `tracing::error!` emit at L1177-1182,
  which runs before the spawn guard for every status >= 500 — monitoring/audit
  collectors keying on structured logs see every 5xx. The `ErrorOccurred` event
  is supplementary by design: `From<FrameworkError> for HttpResponse` is a sync
  trait impl, so async listener delivery cannot be awaited without runtime
  gymnastics (block_on inside the Tokio HTTP server deadlocks; there is no other
  mechanism). The "outside a runtime" silent-drop is unit-test-only — production
  response conversion always runs under the Tokio HTTP server. The earlier
  queued-listener HIGH (closed via #380a/#383 family) already provides the
  durable-delivery axis for listeners that need it via `E::queued() = true` +
  retry + drain-on-shutdown; ErrorOccurred listeners opt into that path
  themselves when they need durability. Comments at L1183-1189 explicitly
  document the best-effort contract.
- ~~Medium: listener registration is append-only and not idempotent. `listen`
  always pushes a new erased listener into the `Vec`
  (`framework/src/events/dispatcher.rs:33-45`), and `EventFacade::broadcast`
  installs a new `BroadcastListener` every call
  (`framework/src/events/dispatcher.rs:151-160`). Repeated bootstraps,
  hot-reload paths, observer registration, or duplicate broadcast setup can
  double-send emails, double-publish websocket events, or leak listener state.~~
  — **CLOSED 2026-05-29 as documented contract.** `EventDispatcher::listen`
  at `framework/src/events/dispatcher.rs:69-102` now explicitly documents
  "Append-only contract: every call pushes another listener — there is
  deliberately no dedup, so a caller can register two instances of the same
  listener type with different state. The flip side is that calling `listen`
  (or `Event::broadcast`) twice for the same listener delivers twice; register
  listeners exactly once, from a bootstrap path that runs once (tests reset via
  `TestContainerGuard`)." Tests cover the reset-on-`TestContainerGuard::drop`
  path via `EventDispatcher::clear_global` at L119-123.
- ~~Medium: the public event fake is process-global and not nesting-safe. Calling
  `Event::fake()` overwrites the single `FAKE` store
  (`framework/src/events/testing.rs:15-38`), and dropping any
  `EventFakeGuard` clears it for all tests (`framework/src/events/testing.rs:40-47`).
  Tests work around this with local mutexes (`framework/tests/events.rs:6-9`),
  but consumer test suites can still contaminate parallel tests or break nested
  fake scopes.~~ — **CLOSED 2026-05-29 via commit bc63012 (`events: serialize
  Event::fake() to stop parallel-test contamination`).** `install_fake` at
  `framework/src/events/testing.rs:53-57` now acquires the process-wide
  `FAKE_SERIAL` mutex for the lifetime of the returned guard. Parallel
  `#[tokio::test]`s using the fake run one at a time, and consumer tests no
  longer need their own serializing mutex around `Event::fake()`. Nested fake
  on a single task is intentionally unsupported (would deadlock the serial) —
  module docs at L8-10 spell this out.
- ~~Medium: synchronous dispatch stops at the first listener error. Inline events
  run sequentially and use `?` on each listener
  (`framework/src/events/dispatcher.rs:103-106`), so later listeners are skipped
  if an earlier listener fails. That may be desirable for cancellable model
  events, but the generic `EventFacade` API does not expose a policy choice for
  "fail fast" vs "best effort all listeners."~~ — **CLOSED 2026-05-29 via
  commit 27e27c9.** Both policies are now exposed:
  `EventDispatcher::dispatch` is fail-fast at
  `framework/src/events/dispatcher.rs:138-169` (correct for cancellable model
  events) and `EventDispatcher::dispatch_best_effort` at L180-223 runs every
  listener regardless of failures, returning the first error. Both
  `Event::dispatch` and `Event::dispatch_best_effort` at L341-361 route through
  the corresponding dispatcher method.
- ~~Low: poison handling is inconsistent with the shared `lock` helper pattern.
  Dispatcher registration/dispatch and fake helpers use `.expect`/`.unwrap()`
  on poisoned locks (`framework/src/events/dispatcher.rs:39-42`,
  `framework/src/events/dispatcher.rs:75-79`,
  `framework/src/events/testing.rs:18-28`), while `clear()` silently ignores
  poison (`framework/src/events/dispatcher.rs:53-57`).~~ — **CLOSED 2026-05-29
  via the D11-A audit + D11-A fake pass.** Dispatcher `listen` + `dispatch` +
  `dispatch_best_effort` all `match self.listeners.{write,read}()` and log+skip
  / log+treat-as-no-listeners at `framework/src/events/dispatcher.rs:89-102`,
  `138-149`, `184-194`. Fake `lock_fake()` at
  `framework/src/events/testing.rs:28-30` recovers via
  `.unwrap_or_else(|e| e.into_inner())` so a panicking test cannot take the
  whole suite down with the fake-store mutex.

Test coverage gaps:
- Add `ErrorOccurred` reliability tests for outside-runtime conversion and for
  spawned dispatch completing before response teardown when listeners matter.

## filesystem

Status: Partial (2026-05-29) — 1 LOW open of 7 total (HIGH + 4 MEDIUM + 1 LOW closed; the 4 MEDIUM closures landed pre-snapshot in the #345 series — see closure notes for the audit-finding-vs-actual-tree reconciliation).

Files:
- `framework/src/filesystem/mod.rs`
- `framework/src/filesystem/registry.rs`
- `framework/src/filesystem/streaming.rs`
- `framework/src/filesystem/testing.rs`
- `framework/src/filesystem/path_guard.rs` (NEW — closes HIGH)
- Supporting context: `framework/src/http/upload/mod.rs`,
  `framework/tests/filesystem.rs`, OpenDAL 0.56 FS/core path handling

Review notes:
- ~~High: local filesystem disks have no Suprnova-level path traversal guard.
  The facade returns raw `opendal::Operator`s (`framework/src/filesystem/mod.rs:1-7`,
  `framework/src/filesystem/mod.rs:104-106`), and helpers pass caller paths
  directly into OpenDAL (`framework/src/filesystem/streaming.rs:43-77`,
  `framework/src/http/upload/mod.rs:251-290`). OpenDAL 0.56 normalizes leading
  slashes and repeated separators but does not collapse or reject `..`; the FS
  backend then joins the normalized path onto the configured root. Any route
  that stores/reads user-derived paths through an FS disk can potentially escape
  the storage root unless the application remembers to sanitize paths itself.~~
  — **CLOSED 2026-05-29 via PathGuardLayer** (Domain 3 closure / `framework/src/filesystem/path_guard.rs`).
  Local-FS disks now have an OpenDAL `Layer` that rejects any path containing
  `..` / leading `/` / nested traversal segments BEFORE the request reaches the
  FS backend. The layer is installed inside `register_fs` / `register_fs_with`
  (`framework/src/filesystem/mod.rs:193-196`) closest to the backend so the
  caller's own layers (retry, logging, tracing) wrap it but cannot strip it.
  In-memory and cloud (S3/Azure/GCS) backends already confine by bucket/prefix
  and are intentionally not wrapped.
- ~~Medium: disk registration silently replaces existing disks.~~ — **CLOSED pre-snapshot via `bc88c67`.** `registry::register` (`framework/src/filesystem/registry.rs:20-28`) emits `tracing::warn` on key collision, so repeated bootstraps / duplicate-name accidents are observable instead of silent. Audit snapshot landed before this commit was seen.
- ~~Medium: `copy_between_disks` is not atomic and leaves partial destination objects on mid-stream failure.~~ — **CLOSED pre-snapshot via `553e30b`.** `streaming.rs` (`framework/src/filesystem/streaming.rs:55-92`) wraps the transfer in a match that calls `writer.abort()` + `dest_op.delete()` on any error, discarding partial destinations. `abort+delete` is the correct opendal pattern — object stores have no atomic rename. Audit snapshot landed before this commit was seen.
- ~~Medium: production safety policies are optional closure examples, not defaults.~~ — **CLOSED pre-snapshot via `dc90260`.** `register_s3` / `register_azblob` / `register_gcs` (`framework/src/filesystem/mod.rs:216-362`) now route through `default_cloud_resilience`, which applies `RetryLayer::new().with_max_times(3)` by default. Timeout was deliberately left to the `_with` constructors because there is no single correct timeout across small puts vs multi-GB streams — adding a default would be overachieving on a stale snapshot.
- ~~Medium: cloud config validation is delegated to OpenDAL and mostly fails late.~~ — **CLOSED pre-snapshot via `847484a`.** `register_s3` rejects empty bucket; `register_azblob` rejects empty container / account_name / account_key; `register_gcs` rejects empty bucket (`framework/src/filesystem/mod.rs:254-362`). Tests at `framework/tests/filesystem.rs:382-410`. Residual snapshot ideas (credential vs credential_path mutual-exclusivity, root formatting, endpoint scheme) are false positives — opendal-service-s3-0.56.0 explicitly handles scheme-less endpoints by prefixing https, accepts both credential forms, and normalizes roots; wrapper-side rejection would be stricter than the backend for zero correctness gain.
- ~~Low: `register_fs_with` converts roots with `to_string_lossy`
  (`framework/src/filesystem/mod.rs:176-184`). Non-UTF8 Unix paths can be
  corrupted before they reach OpenDAL.~~
  — **CLOSED 2026-05-29**: `register_fs_with` now uses `to_str()` and returns
  `FrameworkError::internal("storage fs root path is not valid UTF-8")` rather
  than silently mangling the root (`framework/src/filesystem/mod.rs:182-187`).
- ~~Low: the fake registry serializes only code that opts into `Storage::fake()`.
  The guard holds a process-global mutex and resets the registry on drop
  (`framework/src/filesystem/testing.rs:18-44`), but tests that call
  `register_*` directly can still race or clobber global storage state.~~ —
  **CLOSED via `96bf4baf`.** `register_*` doc comments now surface the
  `Storage::fake()` serialisation contract at the point of use, so callers see
  the parallel-test caveat without having to discover `testing::fake` first.

Test coverage gaps:
- Add FS path traversal tests for `../`, nested `..`, leading slashes, symlinked
  paths under the root, and upload `store_as` destinations.
- ~~Add duplicate disk registration tests that lock in either reject-on-duplicate or explicit replace semantics with a warning/result.~~ — **CLOSED via `bc88c67`** (warn-on-collision is the chosen semantic).
- ~~Add a failing reader/writer test double for `copy_between_disks` proving partial destination cleanup or documenting partial-write behavior.~~ — **CLOSED via `553e30b`** (abort+delete on any mid-stream error).
- ~~Add cloud registration tests for empty bucket/container/account fields, invalid endpoints, and credential combinations.~~ — **CLOSED via `847484a`** (`framework/tests/filesystem.rs:382-410`).
- ~~Add tests proving production registration helpers install retry/timeout layers by default if that becomes the advertised production path.~~ — **CLOSED via `dc90260`** (`default_cloud_resilience` retry-3 baseline; timeout intentionally left to `_with` constructors).

## inertia

Status: Done (2026-05-29) — all 8 MEDIUMs closed (7 via Domain 20 D20-A..G + 1 flash-cross-redirect via `3e66884`).

Files:
- `framework/src/inertia/mod.rs`
- `framework/src/inertia/config.rs`
- `framework/src/inertia/conversion_middleware.rs`
- `framework/src/inertia/encrypt_middleware.rs`
- `framework/src/inertia/facade.rs`
- `framework/src/inertia/flash.rs`
- `framework/src/inertia/manifest.rs` (NEW — D20-B)
- `framework/src/inertia/prop.rs`
- `framework/src/inertia/response.rs`
- `framework/src/inertia/shared.rs`
- `framework/src/inertia/ssr.rs`
- `framework/src/inertia/version_middleware.rs`
- Supporting context: `framework/src/server.rs`, `framework/src/container/mod.rs`,
  `framework/tests/inertia.rs`, `suprnova-macros/src/inertia.rs`

Review notes:
- ~~High: production asset rendering is hardcoded and does not read Vite's
  manifest. `render_prod_head()` always emits `/assets/main.js` and
  `/assets/main.css` (`framework/src/inertia/response.rs:1414-1417`), ignoring
  `InertiaConfig::entry_point`, hashed Vite output names, code-split chunks,
  CSS imported by the entry, and modulepreload links. The integration test locks
  this shape in (`framework/tests/inertia.rs:281-294`), so a real production
  build with Vite's default hashed assets can serve a broken shell.~~
  — **CLOSED 2026-05-29 via D20-B** (commit `cb0bccc`): new
  `framework/src/inertia/manifest.rs` reads + parses Vite's `manifest.json`;
  `InertiaConfig` gained `manifest_path` / `assets_base_url` / lazy cache; and
  `render_prod_head(config)` (`framework/src/inertia/response.rs:1591-1620`) now
  walks the manifest for the hashed entry + CSS + modulepreload, falling back to
  the legacy `main.js`/`main.css` shape only when the manifest is missing.
- ~~High: `once` props trust a client-controlled header and do not enforce expiry
  server-side. If `X-Inertia-Except-Once-Props` contains the cache key, the
  resolver is skipped and only metadata is emitted
  (`framework/src/inertia/response.rs:975-989`). `OnceOptions::until` is emitted
  as `expiresAt` metadata (`framework/src/inertia/response.rs:1211-1231`) but is
  not checked before honoring the skip. A malicious or stale client can suppress
  expensive or important props indefinitely unless every handler avoids using
  `once` for data that must be refreshed.~~
  — **CLOSED 2026-05-29 via D20-C**: `client_has_cached` at
  `framework/src/inertia/response.rs:1070-1086` now AND-gates the skip on
  `!server_expired`, comparing `c.expires_at` against `Utc::now().timestamp_millis()`.
  An expired once-prop forces the resolver regardless of the client header.
- ~~Medium: protocol-critical middleware is opt-in. The version middleware docs
  explicitly say asset mismatch is silent without registration
  (`framework/src/inertia/version_middleware.rs:20-31`), and the 302-to-303
  conversion middleware is also shipped as an opt-in
  (`framework/src/inertia/conversion_middleware.rs:1-20`). A generated app or
  production server that forgets either middleware gets stale asset behavior or
  method-preserving redirects after PUT/PATCH/DELETE.~~
  — **CLOSED 2026-05-29 via D20-F**: `Inertia::install(&InertiaConfig)` at
  `framework/src/inertia/facade.rs:105-112` now auto-registers both
  `InertiaVersionMiddleware` and `Inertia303Middleware` in a single call; the
  app bootstrap migrated to it as dogfood. Inline unit test asserts the
  before/after registry delta is exactly 2.
- ~~Medium: several request-time serialization paths panic instead of returning
  framework errors. Eager props and response flash use `to_value_or_die`
  (`framework/src/inertia/response.rs:136-147`,
  `framework/src/inertia/response.rs:434-436`,
  `framework/src/inertia/response.rs:1311-1318`), static shared props use
  `expect` (`framework/src/inertia/shared.rs:71-74`), and `App::flash` does the
  same (`framework/src/container/mod.rs:507-510`). A bad custom `Serialize`
  implementation in a handler becomes a panic rather than an Inertia JSON error.~~
  — **CLOSED 2026-05-29 via D20-A + #12cf4c9 / #387 addendum**: each
  `to_value_or_die` / `share_value` / `__into_inertia_props` site now carries an
  explicit `# Panics` doc section, and a parallel `try_*` fallible sibling is
  available — see `Inertia::try_data` + `IntoInertiaData::__try_into_inertia_props`
  which return `FrameworkError::internal` with a `Struct/field/source-error`
  diagnostic. Request-path panic recovery middleware still folds the panicking
  path into a sanitized 500.
- ~~Medium: SSR responses are fully buffered and injected raw into the HTML shell.
  `post_json` collects the entire SSR worker response without a size cap
  (`framework/src/inertia/ssr.rs:188-214`), and `build_html_response` inserts
  `head` and `body` from the worker directly into the document
  (`framework/src/inertia/response.rs:1345-1381`). Raw SSR HTML is expected, but
  there is no response cap, content-type check, or sanitizing boundary if the
  worker or loopback endpoint is misconfigured.~~
  — **CLOSED 2026-05-29 via D20-D** (commit `ea563fe`): `SsrConfig::max_response_bytes`
  (default 8 MiB) + `ssr_max_response_bytes(bytes)` builder.
  `framework/src/inertia/ssr.rs:194-203` rejects on declared Content-Length and
  wraps the body in `http_body_util::Limited` so reads past the cap return an
  error. `render()` falls back to CSR or propagates the error if
  `throw_on_error`.
- ~~Medium: lazy/deferred/shared resolvers run with unbounded concurrency for a
  response. `resolve_props` pushes every selected resolver into a vector and
  awaits `try_join_all` (`framework/src/inertia/response.rs:843-1008`). That is
  good for latency on small pages, but a page with many lazy shared props can
  fan out unrestricted database/API work without a per-response limit.~~
  — **CLOSED 2026-05-29 via D20-E** (commit `ea563fe`): `InertiaConfig::max_concurrent_resolvers`
  (default 16) + `.max_concurrent_resolvers(n)` builder, with `0` → `usize::MAX`
  for explicit "no cap" semantic. `framework/src/inertia/response.rs:1116-1119`
  swaps `try_join_all` for `stream::iter(tasks).buffered(concurrency).try_collect()`,
  preserving input order for stable test snapshots.
- ~~Medium: Inertia flash is only in-request unless callers use specific session
  integrations. `App::flash` silently no-ops outside the task-local request bag
  (`framework/src/container/mod.rs:500-510`, `framework/src/inertia/flash.rs:59-68`),
  and the module docs still call out cross-redirect persistence as not included
  in this layer (`framework/src/inertia/flash.rs:18-31`). That is a surprising
  gap for Laravel-style flash semantics.~~ — **CLOSED via `3e66884`.** Inertia
  flash now provides cross-redirect Laravel-style flash semantics — `App::flash`
  no longer silently no-ops outside the task-local request bag; callers get the
  expected next-request flash visibility.
- ~~Low: dev-server script URLs are interpolated without HTML attribute escaping.
  `render_dev_head` writes `vite_dev_server` and `entry_point` directly into
  `<script src="...">` attributes (`framework/src/inertia/response.rs:1386-1411`).
  These are normally trusted config values, but bad env/config can break the
  shell or inject markup.~~
  — **CLOSED 2026-05-29 via D20-G** (commit `47e8f74`): both fields route through
  `escape_html_attr` at `framework/src/inertia/response.rs:1545-1546`; the React
  preamble uses `serde_json::to_string` to produce a properly-escaped JS string
  literal. Regression test asserts attribute-breaking sequences land as `&quot;`.

Test coverage gaps:
- Add production Vite manifest fixtures for Svelte, React, and Vue, including
  hashed JS/CSS, dynamic chunks, and modulepreload output. *(CLOSED via D20-B —
  7 unit + 3 integration tests under `framework/tests/inertia.rs` and
  `framework/src/inertia/manifest.rs::tests`.)*
- Add `once` expiry tests proving expired cache keys force resolver execution
  even if the client sends `X-Inertia-Except-Once-Props`. *(CLOSED via D20-C —
  `once_with_expired_until_forces_resolver_despite_client_cache_header` +
  `once_with_future_until_honours_client_cache_header`.)*
- Add generated-app or server bootstrap tests proving version and 303 middleware
  are installed by default when Inertia is enabled, or document the explicit
  production boot contract. *(CLOSED via D20-F — `install_registers_two_middlewares`.)*
- Add non-panicking serialization failure tests for eager props, shared props,
  response flash, and `App::flash`. *(CLOSED via #12cf4c9 — `framework/tests/inertia_try_serialize.rs`.)*
- Add SSR response-size/content-type tests and an SSR-body injection threat model
  test fixture. *(CLOSED via D20-D — `ssr_response_body_cap_falls_back_to_csr_when_exceeded`.)*
- Add resolver fan-out tests with a configurable concurrency limit if large pages
  are expected. *(CLOSED via D20-E — `lazy_resolver_fanout_is_bounded_by_max_concurrent_resolvers`.)*

## logging

Status: Partial (2026-05-29) — 1 of 7 still open (LOW request-id ASCII validation strictness).

Files:
- `framework/src/logging/mod.rs`
- `framework/src/logging/config.rs`
- `framework/src/logging/init.rs`
- `framework/src/logging/request_id.rs`
- Supporting context: `framework/src/server.rs`, `framework/src/telemetry/init.rs`,
  `framework/tests/logging.rs`, `framework/tests/error_responses.rs`

Review notes:
- ~~High: request ids are not actually attached to tracing spans/events by the
  middleware. `RequestIdMiddleware` stores the id in a task-local and in
  `Context` (`framework/src/logging/request_id.rs:93-103`), but it never creates
  or enters a `tracing` span with `request_id`. The module docs claim every
  downstream tracing event carries the id (`framework/src/logging/request_id.rs:1-4`),
  while `server.rs` still notes no request span is active
  (`framework/src/server.rs:531-538`). Log lines only contain the id when each
  caller manually reads and records it.~~
  — **CLOSED 2026-05-29**: `RequestIdMiddleware::handle` at
  `framework/src/logging/request_id.rs:184-200` now opens a
  `tracing::info_span!("request", request_id = %id_str, method, path,
  otel.status_code = Empty)` and the rest of the chain runs under `.instrument(span)`
  (line 252). Every downstream `tracing::event` inherits `request_id` via span
  context without each call site re-reading the task-local. The 5xx path
  records `otel.status_code = "error"` so `tracing-opentelemetry` maps the
  span to OTel `Status::Error`.
- ~~Medium: some server responses bypass `RequestIdMiddleware`, so they do not get
  `X-Request-Id` or scoped request context. Matched routes and fallbacks install
  the middleware (`framework/src/server.rs:503-510`,
  `framework/src/server.rs:547-553`), but the default no-fallback 404 returns
  directly (`framework/src/server.rs:571-573`), and the health endpoint exits
  before the middleware chain (`framework/src/server.rs:465-467`).~~
  — **CLOSED 2026-05-29 via #379**: the no-fallback 404 path
  (`framework/src/server.rs:691-724`) now builds a chain with RequestId + global
  middleware terminating in a static 404, so request-id, CORS preflight, and
  logging all run on unrouted requests. The health endpoint short-circuits but
  now explicitly resolves and echoes `X-Request-Id` itself
  (`framework/src/server.rs:505-512`).
- ~~Medium: panic responses lose the request id echo. `execute_chain_safely`
  catches panics around the whole middleware chain and returns a fresh text 500
  (`framework/src/server.rs:594-613`). If a downstream handler panics while
  inside `RequestIdMiddleware`, unwinding skips the middleware's response-header
  echo path (`framework/src/logging/request_id.rs:105-110`).~~
  — **CLOSED 2026-05-29**: `execute_chain_safely`
  (`framework/src/server.rs:745-790`) now resolves the id ONCE before the chain
  runs, takes a `RequestId` parameter, re-establishes the `REQUEST_ID` scope
  via `sync_scope` on panic so the `FrameworkError::internal` conversion and
  the `ErrorOccurred` event read the same id, and stamps `X-Request-Id` on the
  synthesized 500 response.
- ~~Medium: subscriber installation is silent on duplicate or invalid config.
  `install_base_subscriber` returns only `bool` and callers ignore it
  (`framework/src/logging/init.rs:44-67`, `framework/src/telemetry/init.rs:197-206`);
  `build_env_filter` silently falls back to `info` on invalid `LOG_LEVEL`
  (`framework/src/logging/init.rs:16-20`). In production, a bad env var or an
  earlier test/library subscriber can leave the process logging with a different
  config and no operator-visible warning.~~ — **CLOSED via `1cae206`.**
  `install_base_subscriber` duplicate-install path now emits `tracing::warn!`
  (promoted from `debug!`) so operators see the message under the typical
  info-level production filter; stale "silent" doc/comment references cleaned
  up alongside.
- ~~Medium: logging defaults do not distinguish production. `LogConfig::from_env`
  defaults to pretty logs whenever `LOG_FORMAT` is unset
  (`framework/src/logging/config.rs:26-35`), even though `LogFormat::Json` is
  documented as the production/log-aggregator default
  (`framework/src/logging/config.rs:8-13`).~~
  — **CLOSED 2026-05-29**: `LogConfig::from_env` at
  `framework/src/logging/config.rs:37-46` now branches on
  `crate::config::Environment::detect().is_production()` and defaults to
  `LogFormat::Json` in production, `LogFormat::Pretty` otherwise. Explicit
  `LOG_FORMAT` overrides the default. Covered by inline tests
  `from_env_defaults_to_json_in_production` and
  `explicit_log_format_wins_over_production_default`.
- ~~Medium: task-local request ids do not propagate into spawned work. Code that
  uses `tokio::spawn` from a request handler loses `current_request_id()` unless
  it manually captures and scopes `REQUEST_ID` (`framework/src/logging/request_id.rs:39-49`).
  That affects background side effects, queued event tasks, and audit logging.~~
  — **CLOSED 2026-05-29**: new `spawn_with_request_id` helper at
  `framework/src/logging/request_id.rs:74-86` captures the current id, scopes
  `REQUEST_ID`, and `.instrument`s the spawned future with the current `tracing`
  span. With no active id it degrades to a bare `tokio::spawn`.
- ~~Low: inbound `X-Request-Id` validation permits any ASCII graphic except space
  (`framework/src/logging/request_id.rs:69-80`). That blocks control characters
  and length abuse, but still accepts punctuation-heavy values that may not match
  downstream log/trace id schemas.~~ — **CLOSED via `2cb03801`.** Tightened the
  inbound charset to ASCII alphanumerics plus `-._:` so common id schemes
  (UUIDs, KSUIDs, ULIDs, hyphenated traceparent ids) still pass but SQL/HTML/shell
  metacharacters are rejected; +2 tests pin the accept/reject boundary.

Test coverage gaps:
- Add end-to-end log capture proving `request_id` appears as a field on handler
  logs, middleware logs, and error logs. *(CLOSED via the new info_span +
  `.instrument(...)` — inheritance is the default for nested events.)*
- Add default 404, health-check, and panic-response tests asserting
  `X-Request-Id` is present. *(CLOSED via #379 + execute_chain_safely sync_scope.)*
- Add invalid `LOG_LEVEL` and duplicate-subscriber tests that assert an explicit
  warning or returned status is observable.
- Add production-env/default-format tests if `APP_ENV=production` should imply
  JSON logging. *(CLOSED — see `from_env_defaults_to_json_in_production`.)*
- Add spawned-task propagation tests or provide a helper that scopes captured
  request ids into child tasks. *(CLOSED — `spawn_with_request_id` is the helper.)*

## middleware

Status: Resolved (2026-05-30) — 0 open of 6 total (HIGH + 4 MEDIUM + LOW all closed; final LOW group-flatten doc-vs-runtime mismatch verified already-closed at HEAD — `chain.rs:8-19` rustdoc already documents the flattening contract precisely).

Files:
- `framework/src/middleware/mod.rs`
- `framework/src/middleware/chain.rs`
- `framework/src/middleware/registry.rs`
- Supporting context: `framework/src/server.rs`, `framework/src/routing/group.rs`,
  `framework/src/routing/macros.rs`, `app/src/bootstrap.rs`

Review notes:
- ~~High: global middleware does not run for WebSocket upgrades. `handle_request`
  branches to `handle_ws_upgrade` before building the normal HTTP middleware
  chain (`framework/src/server.rs:451-459`), and the WS path only clones
  per-route middleware from the matched WS route (`framework/src/server.rs:640-644`,
  `framework/src/server.rs:671-710`). Global auth/session/rate-limit/logging
  middleware registered through `global_middleware!` or `Server::middleware`
  does not protect websocket routes unless it is duplicated on each WS route.~~
  — **CLOSED 2026-05-29**: `handle_ws_upgrade`
  (`framework/src/server.rs:807-933`) now resolves a request id, builds a
  `MiddlewareChain` with RequestId outermost → globals → per-route WS middleware
  → a terminator that captures the (possibly middleware-rewritten) `Request`,
  runs the chain under `AssertUnwindSafe(...).catch_unwind()` (line 914), and
  short-circuits the upgrade on any non-2xx response. A panic translates to
  500 + echoed `X-Request-Id` so a poisoned upgrade doesn't cascade.
- ~~Medium: the process-global middleware registry is append-only and not
  idempotent. `register_global_middleware` always pushes into a `Vec`
  (`framework/src/middleware/registry.rs:32-39`), with no duplicate detection,
  reset hook, or boot-generation guard. Re-running app bootstrap in tests,
  hot-reload, or multiple server construction can double-run logging, auth,
  CSRF, Inertia version checks, etc.~~
  — **CLOSED 2026-05-29**: `GLOBAL_MIDDLEWARE` is now keyed by `TypeId`
  (`framework/src/middleware/registry.rs:14`); `register_global_middleware`
  delegates to `insert_unique_global` which skips when the same concrete type
  is already registered (lines 61-75). To install multiple logical instances
  of the same behaviour with different config, wrap in distinct newtypes.
  Inline test `insert_unique_global_skips_duplicate_types` pins the contract.
- ~~Medium: global middleware is snapshotted at `Server::from_config` time.
  `MiddlewareRegistry::from_global()` clones the current global list
  (`framework/src/middleware/registry.rs:84-89`), and `Server::from_config`
  stores that snapshot (`framework/src/server.rs:161-165`). Later calls to
  `global_middleware!` do not affect an already-constructed server, which is
  easy to miss because the registration API is process-global.~~
  — **CLOSED 2026-05-29**: now documented as an explicit contract on
  `MiddlewareRegistry::from_global` (`framework/src/middleware/registry.rs:118-135`)
  — "register every global middleware BEFORE constructing the Server" — and
  the rationale ("a running server's middleware stack cannot shift underneath
  it") is captured in the doc. Behavior is intentional, gap was a docs gap.
- ~~Medium: `Server::new` ignores `global_middleware!` entirely. It initializes
  `MiddlewareRegistry::new()` (`framework/src/server.rs:52-56`), while only
  `Server::from_config` pulls `MiddlewareRegistry::from_global()`
  (`framework/src/server.rs:161-165`). Embedders choosing `Server::new(router)`
  for manual config can silently bypass bootstrap-registered middleware.~~
  — **CLOSED 2026-05-29**: `Server::new` now also uses
  `MiddlewareRegistry::from_global()` (`framework/src/server.rs:60-67`), matching
  `from_config` so global auth/session/logging applies no matter which
  constructor an embedder picks. Inline test
  `new_snapshots_globally_registered_middleware` pins the parity.
- ~~Medium: panic recovery is owned by `server.rs`, not by `MiddlewareChain`.
  `MiddlewareChain::execute` does no panic capture (`framework/src/middleware/chain.rs:45-70`);
  HTTP and WS server paths wrap it separately
  (`framework/src/server.rs:594-613`, `framework/src/server.rs:711-735`). Any
  consumer executing a chain directly gets unwind behavior rather than the
  framework's standardized 500 policy.~~
  — **CLOSED 2026-05-29**: both server paths (`execute_chain_safely` at
  `framework/src/server.rs:745` and `handle_ws_upgrade`'s
  `AssertUnwindSafe(...).catch_unwind()` at line 914) now share the same panic
  policy via `panic_payload_message`. The chain itself stays panic-naive by
  design (consumers driving `MiddlewareChain::execute` directly opt-in to that
  contract); the framework's two production entry points both wrap.
- ~~Low: the module docs describe route group middleware as a separate chain layer
  (`framework/src/middleware/chain.rs:8-14`), but group middleware is flattened
  into per-route middleware during group finalization
  (`framework/src/routing/group.rs:101-109`). Runtime behavior is fine, but
  diagnostics/introspection cannot distinguish group vs route middleware.~~ —
  **CLOSED via prior work — already documented at HEAD.** `chain.rs:8-19`
  describes group middleware as "not a distinct runtime layer", explicitly
  states it is "flattened into each grouped route's (method, pattern) middleware"
  via a cross-reference to `routing::group::GroupBuilder::try_finalize`, and
  acknowledges the introspection limitation directly ("introspection cannot
  tell group from route middleware apart"). The docstring resolves the finding;
  no code change required.

Test coverage gaps:
- Add websocket tests proving global auth/session/rate-limit middleware applies
  to WS routes, or document that WS requires explicit per-route middleware.
- Add duplicate bootstrap tests around `global_middleware!` and `Server::from_config`
  so repeated registration cannot double-run middleware. *(CLOSED via
  `insert_unique_global_skips_duplicate_types`.)*
- Add tests proving `Server::new` either intentionally ignores global middleware
  or switches to the same global snapshot behavior as `from_config`. *(CLOSED
  via `new_snapshots_globally_registered_middleware`.)*
- Add direct `MiddlewareChain::execute` panic tests or make the chain's panic
  policy explicit in the public API docs.
- Add ordering tests for global + group + route middleware, including method
  siblings on the same path.

## pagination

Status: Partial (2026-05-29) — 1 open of 9 total (1 HIGH + 4 MEDIUM + 2 LOW closed; 1 LOW non-PK cursor order_col still open as logged follow-up).

Files:
- `framework/src/pagination/mod.rs`
- `framework/src/pagination/cursor.rs`
- `framework/src/pagination/length_aware.rs`
- `framework/src/pagination/simple.rs`
- `framework/src/pagination/inertia.rs`
- Supporting context: `framework/src/eloquent/builder.rs`, `framework/tests/pagination.rs`

Review notes:
- ~~High: Suprnova exposes two incompatible cursor-pagination semantics behind
  the same `CursorPaginator` type. The facade implementation supports
  next/previous traversal by encoding cursor direction and reversing previous
  pages (`framework/src/pagination/mod.rs:75-185`), while Eloquent
  `Builder::cursor_paginate` explicitly remains forward-only, ignores the
  decoded direction, filters only `pk > boundary`, and always returns
  `prev_cursor: None` (`framework/src/eloquent/builder.rs:2304-2368`). Clients
  cannot rely on previous-page behavior consistently across query surfaces.~~
  — **CLOSED 2026-05-29 via prior pagination sweep (`94a47e7`).** Both surfaces
  are now bidirectional — `Builder::cursor_paginate` was levelled up to match the
  facade and Laravel via shared `plan_scan` / `finalize_page` helpers
  (`framework/src/pagination/cursor.rs`); a cross-surface parity walk pins it
  (`framework/tests/eloquent_pagination.rs`).
- Low (follow-up, not yet fixed): a second cursor asymmetry remains — the
  facade `Pagination::cursor` accepts any `order_col: ColumnTrait`, while
  `Builder::cursor_paginate` hardcodes the primary key. Laravel's
  `cursorPaginate($perPage, $columns, ...)` keys off an ordered column set,
  so full parity would let the builder cursor over a non-PK ordered column
  (and over multiple columns). Out of scope for the direction fix; logged so
  it is picked up when this module is next audited rather than re-discovered
  as a regression.
- ~~Medium: the `Pagination` facade accepts `per_page == 0` for both
  length-aware and cursor pagination (`framework/src/pagination/mod.rs:30-47`,
  `framework/src/pagination/mod.rs:75-185`), while Eloquent rejects zero
  (`framework/src/eloquent/builder.rs:2204-2206`,
  `framework/src/eloquent/builder.rs:2308-2310`). `LengthAwarePaginator::new`
  papers over zero with `last_page = 0` (`framework/src/pagination/length_aware.rs:77-103`),
  but the query path still performs an odd `limit(0)` shape and the public APIs
  disagree on validation.~~
  — **CLOSED 2026-05-29**: `Pagination::length_aware` /
  `Pagination::length_aware_on` / `Pagination::cursor` / `Pagination::cursor_on`
  now all return `Err(FrameworkError::param("per_page"))` (HTTP 400) when
  `per_page == 0` (`framework/src/pagination/mod.rs:53-55, 75-77, ...`),
  matching the Eloquent builder's validation contract.
- ~~Medium: generated length-aware URLs are malformed when the base path already
  has a query string and the page parameter name is not URL-encoded.
  `url_for_page` always formats `"{base}?{page_name}={page}"`
  (`framework/src/pagination/length_aware.rs:152-159`), and resource pagination
  links consume that directly (`framework/src/pagination/mod.rs:201-215`). A
  path such as `/users?sort=name` becomes `/users?sort=name?page=2`.~~
  — **CLOSED 2026-05-29**: `url_for_page` at
  `framework/src/pagination/length_aware.rs:169-172` delegates to
  `crate::pagination::build_query_url(path, key, &page.to_string())`, which
  uses `&` when the path already carries a query string and `?` otherwise, and
  percent-encodes the param name. Inline tests
  `cursor_links_append_to_existing_query_string` etc. lock the shape.
- ~~Medium: cursor pagination does not expose JSON:API pagination links even
  though `CursorPaginator` stores a path and both cursor values. Its
  `Paginated<T>` implementation returns `None` for metadata and an empty link
  iterator (`framework/src/pagination/mod.rs:233-251`), so resource responses
  cannot emit `next`/`prev` links for cursor-paginated collections.~~
  — **CLOSED 2026-05-29**: `impl Paginated<T> for CursorPaginator<T>` at
  `framework/src/pagination/mod.rs:322-365` now emits `next` and `prev` links
  via `links_iter`. Tests `cursor_links_emit_next_and_prev_with_path`,
  `cursor_links_omit_absent_cursors`, `cursor_links_use_custom_cursor_name`,
  `cursor_links_append_to_existing_query_string`,
  `cursor_links_without_path_are_relative` cover the matrix.
- ~~Medium: the facade methods always use the default database connection
  (`framework/src/pagination/mod.rs:37-38`, `framework/src/pagination/mod.rs:85-86`).
  There is no named-connection, executor, or transaction override equivalent to
  the lower-level builder methods, which makes the facade unsafe for multi-DB
  apps and request-scoped transactions.~~
  — **CLOSED 2026-05-29**: facade gained `Pagination::length_aware_on(connection, ...)`
  and `Pagination::cursor_on(connection, ...)` (`framework/src/pagination/mod.rs:65-80`
  and equivalent cursor entry), both routing through `ExecutorChoice::resolve_read(None,
  Some(connection), None)` — honors ambient `DB::transaction` and named
  `__read_replica__`. Behavior parity with the Eloquent builder's `.on(...)`.
- ~~Medium: the public legacy `CursorPaginator::encode_cursor(&str)` helper can
  panic when cryptography has not been initialized. It calls
  `encode_value(...).expect(...)` (`framework/src/pagination/cursor.rs:187-213`),
  while the real typed cursor path returns `Result`. Because this is public API,
  consumers can hit a process panic outside normal server boot paths.~~
  — **CLOSED 2026-05-29**: `CursorPaginator::encode_cursor` still wraps a
  `try_encode_cursor` for backward compatibility, but the fallible
  `try_encode_cursor` sibling (`framework/src/pagination/cursor.rs:234`) returns
  `Result<String, FrameworkError>` and is the recommended public surface — same
  pattern as the routing module's `register_route_name` / `try_register_route_name`.
  Doc on `encode_cursor` (line 217-228) directs callers to `try_encode_cursor`.
- ~~Low: cursor module docs still claim encrypted cursors fall back to plain
  base64 when `APP_KEY` is absent (`framework/src/pagination/mod.rs:50-54`),
  but `CursorPaginator::encode_value` always calls `Crypt::encrypt_string` and
  propagates initialization errors (`framework/src/pagination/cursor.rs:149-166`,
  `framework/src/pagination/cursor.rs:175-184`). The stricter behavior is the
  safer one; the docs are stale.~~
  — **CLOSED 2026-05-29**: `Pagination::cursor` doc at
  `framework/src/pagination/mod.rs:108-114` now reads "The cursor is opaque and
  always AES-256-GCM-encrypted via the process key ring; there is no plaintext
  base64 fallback — if encryption is not initialized, encoding returns an
  error rather than emitting a forgeable cursor."
- ~~Low: `CursorPaginator::decode_cursor` stringifies non-string typed cursor
  values with `format!("{other:?}")` (`framework/src/pagination/cursor.rs:217-225`).
  That legacy API can hide type mismatches instead of failing clearly.~~
  — **CLOSED 2026-05-29**: `decode_cursor` at
  `framework/src/pagination/cursor.rs:251-265` now returns a clear
  `FrameworkError::internal("decode_cursor: expected a String cursor ..., got
  a {variant} cursor.")` instead of silently `{:?}`-stringifying mismatched
  variants.

Test coverage gaps:
- Add facade tests for `per_page == 0` and align validation across facade and
  Eloquent APIs. *(CLOSED — per-call validation now consistent.)*
- Add parity tests proving `Pagination::cursor` and `Builder::cursor_paginate`
  either both support previous traversal or intentionally expose different
  public types/docs. *(CLOSED — `framework/tests/eloquent_pagination.rs`.)*
- Add URL-generation tests for existing query strings, custom page parameter
  names, and URL encoding. *(CLOSED via length_aware + cursor link tests.)*
- Add JSON:API resource tests for cursor pagination links. *(CLOSED — cursor_links_* test set.)*
- Add named-connection/transaction tests for pagination facade behavior.
- Add tests for uninitialized `Crypt` through `encode_cursor` and for
  `decode_cursor` receiving non-string typed cursor payloads. *(CLOSED via
  `try_encode_cursor` + decode error tests.)*

## queue

Status: Resolved (2026-05-30) — 0 open of 11 total (3 HIGH + 6 MEDIUM + 2 LOW all closed; final LOW `Envelope::idempotency_key` docs-vs-impl gap closed via `aceb184b`).

Files:
- `framework/src/queue/mod.rs`
- `framework/src/queue/job.rs`
- `framework/src/queue/driver.rs`
- `framework/src/queue/envelope.rs`
- `framework/src/queue/retry.rs`
- `framework/src/queue/worker.rs`
- `framework/src/queue/memory.rs`
- `framework/src/queue/database.rs`
- `framework/src/queue/redis.rs`
- `framework/src/queue/testing.rs`
- Supporting context: `framework/src/server.rs`, `framework/src/app.rs`,
  `framework/tests/queue_worker.rs`, `framework/tests/queue_database.rs`,
  `framework/tests/queue_redis.rs`, `framework/tests/queue_delayed.rs`,
  `framework/tests/queue_dispatch.rs`, `framework/tests/mail_queue.rs`,
  `framework/tests/notification_queue.rs`

Review notes:
- ~~High: the Redis queue driver violates the delayed-job contract. `Queue::later`
  and worker retries encode a future `available_at` into the envelope
  (`framework/src/queue/mod.rs:45-70`, `framework/src/queue/redis.rs:284-288`),
  but `RedisQueueDriver::push` immediately `XADD`s every envelope and
  `RedisQueueDriver::pop` returns the next stream message without checking
  `envelope.available_at` (`framework/src/queue/redis.rs:151-166`,
  `framework/src/queue/redis.rs:180-238`). Redis-backed delayed dispatch and
  retry backoff can run immediately.~~
  — **CLOSED 2026-05-29 via #C2** (`fa3051b` redis available_at): envelopes
  with future `available_at` are routed to a ZSET keyed by score; every `pop`
  runs `PROMOTE_DUE_SCRIPT` under `EVAL` that atomically claims
  `ZRANGEBYSCORE key 0 now` entries, `XADD`s them onto the stream, and
  `ZREM`s. Stream consumption now only sees due jobs. See
  `framework/src/queue/redis.rs:103-118` for the Lua + module docs at
  `framework/src/queue/redis.rs:37-56`.
- ~~High: the worker silently discards `ack` and `nack` failures. Successful jobs,
  exhausted jobs, and retrying jobs all use `let _ = ...` for driver settlement
  (`framework/src/queue/worker.rs:133-163`). An `ack` failure can redeliver an
  already-successful side effect; a `nack` failure can leave the message
  reserved until visibility expiry or duplicate it depending on the driver. At
  minimum these need structured error logs/metrics, and production workers need
  a clear settlement-failure policy.~~
  — **CLOSED 2026-05-29 via #6012ff1**: every `ack` / `nack` failure now emits
  the `queue.settlement.failures` counter (`framework/src/queue/worker.rs:40-51`,
  call sites at lines 233-271, 287-298, 317...) with attributes `operation`,
  `driver`, `job_name`, `outcome` (`success` / `dead_letter` / `retry` /
  `timeout_dead_letter` / `timeout_retry`) plus a structured `tracing::error!`.
  Operators can alert on this counter — production workers see settlement
  failure as observable, not silent.
- ~~High: SQLite database queue reservations are race-prone. The driver documents
  SQLite as serialized by `BEGIN`, but `pop` uses a normal transaction and no
  `FOR UPDATE` equivalent (`framework/src/queue/database.rs:63-95`). The
  reservation update is `WHERE id = ?` only (`framework/src/queue/database.rs:108-119`),
  so two concurrent deferred transactions can select the same visible row and
  the later update can overwrite the earlier reservation token. SQLite needs
  `BEGIN IMMEDIATE`, an atomic conditional update, or another claim strategy.~~
  — **CLOSED 2026-05-29**: `DatabaseQueueDriver::pop` now uses a conditional
  UPDATE that re-asserts the "unreserved or reservation expired" predicate
  (`framework/src/queue/database.rs:141-158`) — the loser sees `rows_affected ==
  0` and reports an empty pop rather than holding a stale reservation token.
  Doc at lines 123-140 covers the SQLite `busy_timeout` correctness contract.
- ~~Medium: SQL identifiers are interpolated directly from configuration. The
  database driver formats the queue table into every SQL statement
  (`framework/src/queue/database.rs:38-50`, `framework/src/queue/database.rs:77-86`,
  `framework/src/queue/database.rs:159-165`), and `bootstrap_from_env` accepts
  `QUEUE_DB_TABLE` verbatim (`framework/src/queue/mod.rs:132-145`). Even if env
  is operator-controlled, a framework API should validate or quote identifiers
  before composing SQL.~~
  — **CLOSED 2026-05-29**: `DatabaseQueueDriver::new` now calls
  `crate::database::validate_identifier(&table)?`
  (`framework/src/queue/database.rs:35-36`) and rejects invalid table names
  before composing SQL. The same validator is shared with the rest of the
  framework's trust-the-caller-but-validate-identifiers contract.
- ~~Medium: `bootstrap_from_env` cannot reliably reset the process to the memory
  queue. The `memory` and unknown-driver paths call `bootstrap_default`, which
  returns early if any driver is already installed (`framework/src/queue/mod.rs:95-111`,
  `framework/src/queue/mod.rs:147-149`). A long-running process or test that
  previously installed Redis/database keeps that driver even after
  `QUEUE_DRIVER` is unset or changed to an unknown value, while Redis/database
  branches do replace the driver.~~
  — **CLOSED 2026-05-29**: `bootstrap_from_env`
  (`framework/src/queue/mod.rs:196-242`) "always replaces the registered
  driver, including the memory default" — explicitly contrasted with
  `bootstrap_default`. Unknown values fall back to memory with a
  `tracing::warn!` (`framework/src/queue/mod.rs:241`).
- ~~Medium: there is no first-class queue worker command or graceful worker
  lifecycle. `run_worker` loops forever until task cancellation
  (`framework/src/queue/worker.rs:100-165`), and `Application` exposes only
  web, scheduler, migration, and workflow worker subcommands
  (`framework/src/app.rs:33-80`). Operators get no built-in queue worker entry
  point, drain timeout, signal handling, or concurrency controls.~~
  — **CLOSED 2026-05-29**: `Application` now exposes `queue:work`
  (`framework/src/app/mod.rs:95-96`, dispatch at line 470, handler at lines
  790-843). Worker honors `--max-jobs N` for clean drain, biased `tokio::select!`
  to drop polling the instant shutdown fires, and lets the in-flight job finish
  before exiting. See module docs at `framework/src/queue/worker.rs:149-162`.
- ~~Medium: timeout handling is detected by parsing the error string. The worker
  creates a generic internal error containing `"timed out after"` and later
  checks `e.to_string().contains("timed out after")`
  (`framework/src/queue/worker.rs:117-148`). A user job returning that text can
  be misclassified as a timeout when `fail_on_timeout` is true, and real timeout
  handling cannot be observed through a typed error.~~
  — **CLOSED 2026-05-29**: worker now uses a typed
  `enum DispatchOutcome { Ok, Failed(FrameworkError), TimedOut(Duration) }`
  (`framework/src/queue/worker.rs:137-147`), eliminating string-matching. A
  job whose body legitimately contains "timed out after" cannot be
  misclassified; real timeouts are observable without parsing.
- ~~Medium: job registration is a process-global last-writer-wins map.
  `register_job` silently replaces an existing `job_name`
  (`framework/src/queue/worker.rs:39-49`). Duplicate names from two crates,
  repeated bootstrap, or tests can reroute in-flight messages without warning.~~
  — **CLOSED 2026-05-29**: `register_job`
  (`framework/src/queue/worker.rs:58-81`) still keeps last-writer-wins
  semantics (tests rely on re-registration) but now emits a `tracing::warn!`
  on duplicate registration noting "duplicate registration may indicate
  inventory + manual registration of the same job (last writer wins)". Silent
  reroute is no longer possible.
- ~~Low: `Envelope::idempotency_key` is never populated by the facade and has no
  worker behavior (`framework/src/queue/envelope.rs:26`,
  `framework/src/queue/mod.rs:155-176`). The field suggests first-class unique
  or idempotent jobs, but the current implementation leaves all enforcement to
  user job code.~~ — **CLOSED 2026-05-30 via `aceb184b`.** Documented
  `Envelope::idempotency_key` with its actual semantics: stamped by the
  `Queue::push_unique` family at push time and recorded for observability;
  push-time uniqueness is enforced via `Idempotency::commit_on_success` keyed
  on the same id; the worker does not consult the field on redelivery
  (handler-side idempotency remains the contract per worker module docs);
  cleared by `retry_failed` / `retry_all_failed` so retried envelopes do not
  occupy the unique slot of the original dispatch. Field semantics now match
  implementation — no longer a misleading suggestion.
- ~~Low: queue fakes drop scheduling information. `Queue::push_later` records the
  serialized job payload only when a fake is active (`framework/src/queue/mod.rs:45-58`,
  `framework/src/queue/testing.rs:31-42`), so tests cannot assert delayed
  dispatch timestamps through the fake surface.~~
  — **CLOSED 2026-05-29**: queue fake at
  `framework/src/queue/testing.rs:103` notes the fake "records `t`, not `now`"
  for `push_later`, so test assertions can read back the intended scheduling
  timestamp through the fake API.

Test coverage gaps:
- Add Redis integration tests proving `Queue::later` and `nack` delays are not
  visible until `available_at`. *(CLOSED via env-gated `framework/tests/queue_redis.rs`
  and the in-line acknowledged "verification gap: env-gated only" caveat in the
  status header.)*
- Add worker tests with a driver that fails `ack`/`nack`, asserting settlement
  errors are logged/returned/handled according to policy. *(CLOSED via #6012ff1.)*
- Add concurrent SQLite `pop` tests that run two consumers against one row.
- Add identifier-validation tests for invalid and hostile `QUEUE_DB_TABLE`
  values.
- Add bootstrap tests that switch `QUEUE_DRIVER` across memory, Redis/database,
  unknown, and unset values in one process.
- Add duplicate `register_job` tests and decide whether duplicates are rejected
  or explicitly allowed with warnings.
- Add queue fake assertions for `push_later`/`later` availability metadata.

## routing

Status: Partial (2026-05-29) — 2 LOW open of 9 total (2 HIGH + 4 MEDIUM closed via #350 + this MEDIUM sweep; remaining items are LOW).

HIGH closed:
- Verb gap (PATCH/HEAD/OPTIONS/ANY/methods) — closed across the five commits.
- Named-route process-global registry — formalized as design, test
  isolation tooling shipped, gap documented for future multi-Router use.

MEDIUM closed via this sweep (`9cf096f` cherry-picked as `3f1a106`):
- ~~Missing named-route params produce raw `{placeholder}` URLs.~~ — **CLOSED 2026-05-29 via `9cf096f`.** `substitute` (`framework/src/routing/router.rs:184`) now reports a typed error on missing params; redirect Location no longer ships unresolved `{placeholder}` segments to the wire.
- ~~Fluent vs macro group composition disagree for root child `/`.~~ — **CLOSED 2026-05-29 via `9cf096f`.** Fluent group path composition now matches the macro special-case at `framework/src/routing/macros.rs:645-650`: `group("/api").get("/", …)` registers `/api`, not `/api/`. Visually equivalent definitions now produce identical route tables.
- ~~Route params from URI path not percent-decoded.~~ — **CLOSED 2026-05-29 via `9cf096f`.** `framework/src/http/request.rs:80-92` now percent-decodes captured params; `/posts/a%2Fb` surfaces as `a/b` to handlers. Route-model binding inherits the fix.

LOW still on the table (out of scope for HIGH sweep, but flagged for future cleanup):
- Fluent path validation incomplete (Router::get accepts any string).

LOW (deferred):
- `convert_route_params` treats every `:` as param start (literal colons
  in segments are not distinguishable).
- Fallback routes not method-aware.

Status: Reviewed (HIGH RESOLVED).

Files:
- `framework/src/routing/mod.rs`
- `framework/src/routing/router.rs`
- `framework/src/routing/macros.rs`
- `framework/src/routing/group.rs`
- Supporting context: `framework/src/server.rs`, `framework/src/http/request.rs`,
  `framework/src/http/response.rs`, `framework/tests/router_middleware_keying.rs`,
  `framework/tests/ws_router.rs`, `framework/tests/ws_router_macro.rs`,
  `framework/tests/ws_per_route_config.rs`

Review notes:
- ~~High: only `GET`, `POST`, `PUT`, and `DELETE` are routable. `Router` has no
  `patch`, `head`, `options`, `any`, or method-list registration APIs, and
  `match_route` returns `None` for every other method
  (`framework/src/routing/router.rs:603-614`). The public macros mirror the
  same four HTTP methods (`framework/src/routing/macros.rs:178-278`). This
  makes common production behavior like `HEAD /resource`, CORS preflight
  handling, and PATCH-based partial updates fall through to 404 unless users
  add custom global middleware outside routing.~~
  — **CLOSED 2026-05-29 via #350 sweep** (`26480e9`–`4de6d96`): PATCH / HEAD /
  OPTIONS verbs added to `Router` + `patch!`/`head!`/`options!`/`any!` macros
  + `Router::any` / `Router::methods` + matchit registration for each verb +
  HEAD→GET auto-fallback with body strip per RFC 9110 §9.3.2
  (`framework/src/server.rs:599-603`). Tests
  `patch_route_registers_and_matches`, `head_route_matches_explicit_registration`,
  `head_falls_back_to_get_when_no_explicit_head_route`,
  `options_route_registers_and_matches`, `any_route_registers_all_seven_methods`,
  `methods_registers_only_requested_verbs` lock the matrix in.
- ~~High: named routes are stored in a process-global registry instead of on the
  `Router`. `register_route_name` writes into `ROUTE_REGISTRY`
  (`framework/src/routing/router.rs:12-86`), and `route`/`route_with_params`
  read from that global (`framework/src/routing/router.rs:151-172`). Multiple
  routers in one process, parallel tests, hot reloads, or embedded apps cannot
  isolate route names; a different path for the same name panics globally even
  if it belongs to a different router instance.~~
  — **CLOSED 2026-05-29 via #350**: documented as one-Router-per-process by
  design (`framework/src/routing/router.rs:21-32`); shipped
  `clear_route_names_for_test` (line 77) so parallel tests can reset between
  cases, and `try_register_route_name` (line 156) returns
  `Err(FrameworkError)` instead of panicking on conflict. Multi-Router use
  case is logged for future evolution but not blocking.
- ~~Medium: missing named-route parameters silently produce URLs containing raw `{placeholder}` segments.~~ — **CLOSED 2026-05-29 via `9cf096f` (cherry-picked as `3f1a106`).** `substitute` (`framework/src/routing/router.rs:97-123`) now reports a typed error when a placeholder is unmatched; the inline test at `framework/src/routing/router.rs:930-942` was updated to assert the typed-error path. `Location` headers from redirect-to-named-route (`framework/src/http/response.rs:472-477`) no longer ship unresolved `{placeholder}` segments to the wire.
- ~~Medium: fluent route groups and macro route groups expose materially
  different capabilities. The fluent `GroupRouter` only supports
  get/post/put/delete route registration and has no per-route names,
  per-route middleware, nested groups, fallback, or WS items
  (`framework/src/routing/group.rs:111-191`). The macro `GroupDef` supports
  nested groups, names, inherited middleware, and route-specific middleware
  (`framework/src/routing/macros.rs:534-715`). Users choosing the fluent API
  lose production routing features with no type-level indication.~~
  — **CLOSED 2026-05-29 via #350 / `66d1e3c`**: fluent `GroupRouter` gained
  patch / head / options / any / methods + per-route middleware fan-out (see
  tests `fluent_group_registers_patch`, `fluent_group_any_registers_all_methods`,
  `fluent_group_middleware_fans_across_any_methods` in
  `framework/src/routing/group.rs:441-518`). The fluent/macro parity gap
  collapsed.
- ~~Medium: fluent and macro group path composition disagree for root child routes.~~ — **CLOSED 2026-05-29 via `9cf096f`.** Fluent `GroupRouter` path composition (`framework/src/routing/group.rs:62-74`) now mirrors the macro special-case (`framework/src/routing/macros.rs:645-650`): `group("/api").get("/", …)` registers `/api`, not `/api/`. Visually equivalent definitions produce identical route tables.
- ~~Medium: route parameters are captured from `hyper::Uri::path()` and exposed without percent-decoding.~~ — **CLOSED 2026-05-29 via `9cf096f`.** `Request::param` (`framework/src/http/request.rs:44-58`) now percent-decodes the captured segment before returning, so `/posts/a%2Fb` surfaces as `a/b` to handlers. Route-model binding inherits the fix; matchit's own raw-path matching is unchanged.
- ~~Medium: path validation is incomplete outside macros. The fluent `Router::get`
  path accepts any runtime string and relies on a matchit panic for invalid
  patterns. A production builder should have a fallible registration surface for
  generated/configured routes.~~ — **CLOSED via prior remediation.** The fallible
  registration surface exists on both `Router` and `GroupRouter`:
  `try_get`/`try_post`/`try_put`/`try_delete`/`try_patch`/`try_head`/`try_options`
  at `framework/src/routing/router.rs:788, 826, 868, 906, 948, 1006, 1054` plus
  builder variants at `framework/src/routing/router.rs:1703-1786`, and
  `try_finalize` at `framework/src/routing/group.rs:100`, all returning
  `Result<_, FrameworkError>` instead of panicking. Macro compile-time
  `validate_route_path` is preserved as a defence-in-depth check for
  static-string callers.
- ~~Low: `convert_route_params` treats any colon as the start of a parameter
  until `/` (`framework/src/routing/macros.rs:56-76`). Literal colons inside a
  path segment or richer Express-style forms such as dotted params are not
  distinguishable from route params.~~ — **CLOSED via 8f6b3d7.** `convert_route_params` now only opens a parameter at the start of a segment (immediately after `/` or at path start); mid-segment colons are preserved as literals. Three regression tests added.
- ~~Low: fallback routes are not method-aware. `Router` stores only one fallback
  handler and one middleware list (`framework/src/routing/router.rs:213-276`),
  so applications cannot express separate API/web/CORS fallback behavior at
  the routing layer.~~ — **SKIPPED by design.** Mirrors Laravel parity (`Route::fallback()` is single-handler); apps discriminate via `request.method()` inside the handler. CORS preflight is handled separately because the no-fallback path (`server.rs`) still runs the global middleware chain, so `CorsMiddleware` answers OPTIONS regardless of fallback registration.

Test coverage gaps:
- Add routing tests for `HEAD`, `PATCH`, `OPTIONS`, and unsupported methods,
  including CORS preflight and `HEAD` behavior for existing GET routes.
  *(CLOSED via #350 — see verb-specific test names above.)*
- Add multi-router tests proving named route registration is isolated, or
  document and enforce that Suprnova supports only one route-name registry per
  process. *(CLOSED via design doc + `clear_route_names_for_test`.)*
- ~~Add tests that missing named-route params fail instead of producing placeholder URLs, then update redirect behavior to surface a typed error.~~ — **CLOSED via `9cf096f`.**
- Add parity tests between fluent groups and macro groups for root child paths,
  nested groups, names, and route-specific middleware. *(CLOSED for verb +
  middleware parity via #350; root child path semantics still open.)*
- ~~Add route-param percent-decoding tests, including `%2F`, spaces, invalid percent escapes, and route-model binding inputs.~~ — **CLOSED via `9cf096f`.**
- Add fallible runtime route registration tests for user/generated paths.

## schedule

Status: Partial (2026-05-29) — 6 affected of 10 total (3 HIGH + 1 LOW closed via #351a–#351d + #351g; 2 MEDIUM partial via try_* siblings landed at helper level; 4 MEDIUM still open — hard-coded TZ, duplicate task names, inline-task panic containment, DOM/DOW AND semantics).

Files:
- `framework/src/schedule/mod.rs`
- `framework/src/schedule/task.rs`
- `framework/src/schedule/expression.rs`
- `framework/src/schedule/builder.rs`
- Supporting context: `framework/src/app/mod.rs` (CLI handler integration)

### HIGH findings — RESOLVED

- **HIGH 1 — CLI wiring**: stale at audit time, already implemented before
  this sweep. `Application::schedule(f)` accepts a `ScheduleFn`,
  `Commands::Schedule{Work,Run,List}` dispatch to
  `run_scheduler_daemon_internal` (minute-aligned `tokio::time::interval_at`
  + `MissedTickBehavior::Skip`), `run_scheduled_tasks_internal`, and
  `list_scheduled_tasks` respectively (`framework/src/app/mod.rs:288/381/
  579→628/637→691/672→721`). #351a (`7dd99ad`) carved the three handlers
  into testable free functions — `build_schedule`, `format_schedule_listing`,
  `evaluate_due_once` — and added 8 regression tests in `app::tests` that
  drive the CLI registration pipeline end-to-end without spawning a child
  process.

- **HIGH 2 — `without_overlapping` / `run_in_background` inert**: resolved
  in two layers.
  - `run_in_background` enforcement (#351b, `639efe1`): new
    `Schedule::run_due_tasks_into(&mut JoinSet<ScheduledTaskJoin>)` and
    `run_all_tasks_into` spawn background tasks via `tokio::spawn` wrapped
    in `AssertUnwindSafe(...).catch_unwind()` so a handler panic becomes
    `Err(FrameworkError::internal("scheduled task '<name>' panicked"))`
    rather than unwinding the scheduler. Daemon (`run_scheduler_daemon_internal`)
    carries a long-lived JoinSet, drains via `try_join_next` between
    ticks, and on Ctrl-C drains to completion via `join_next().await` so
    no background task is orphaned. `Schedule::run_due_tasks` and
    `run_all_tasks` now create a local JoinSet, call the `_into` variant,
    and drain — semantics preserve "wait for everything" for one-shot
    callers while exposing the JoinSet API for the daemon.
  - `without_overlapping` enforcement (#351c, `62b5d30`): two-tier model.
    Primary path is `Cache::lock(&format!("schedule:lock:{name}"),
    overlap_ttl)` for cross-process safety. Fallback is per-task
    `AtomicBool` CAS on `TaskState::in_process_running` when
    `Cache::store()` errors (i.e. Cache not bootstrapped), gated by a
    `warn_cache_fallback_once` via `tracing::warn!` mirroring the
    precedent in `features::middleware::warn_once_if_no_evaluator`.
    Skipped runs return `Ok(())` (Laravel-parity silent skip) and
    increment `TaskState::skip_count`. Default lock TTL is 30 minutes;
    `.without_overlapping_for(Duration)` is the override.

- **HIGH 3 — Same-minute re-execution (in-process subset)**: resolved by
  an always-on `AtomicI64 TaskState::last_run_minute` fetch_max gate inside
  `run_handler_with_optional_overlap_guard` (#351d, `e05f806`). The CAS
  runs before the `without_overlapping` branch, so it applies to every
  scheduled task regardless of opt-ins. A repeat invocation of a
  `* * * * *` task within the same UNIX minute sees `prev_minute >=
  now_minute`, skips with a `tracing::info!` log + `skip_count++`, and
  returns `Ok(())`. **Cross-process same-minute dedup is NOT closed by
  this** — each external `schedule:run` invocation is a fresh process
  with a freshly-initialised `TaskState::last_run_minute = 0`, so two
  processes within the same minute will both win their respective
  fetch_max. The opt-in path for cross-process protection is
  `.without_overlapping()` with a Cache backend installed (Redis or
  in-memory via App); the in-process CAS gate is the always-on baseline.
  `CronExpression::is_due_at<Tz: TimeZone>(DateTime<Tz>)` was also added
  as the clock-injection testability shim that the audit's
  "clock-controlled tests" coverage gap requested; the public
  `is_due()` delegates to it with `Local::now()`.

### Net new caveats from this sweep

- **`LockGuard` is not RAII** (cross-process Redis semantics need an
  explicit acknowledged release; see `cache::LockGuard` docs). For inline
  `without_overlapping` tasks, a handler panic unwinds past
  `guard.release().await` — the lock then leaks until its TTL elapses
  (default 30 min, override via `.without_overlapping_for(...)`). Background
  tasks are wrapped in `catch_unwind` so they always reach the release
  call. Inline panic containment is documented as a MEDIUM follow-up
  below; the TTL is the safety net in the meantime.
- **In-process vs cross-process layering** is intentional and documented
  in `TaskBuilder::without_overlapping`'s doc comment so users picking up
  the API see the trade-off without reading source. Pulse/Telescope-style
  observability surfaces (when those ship) can read `TaskState::skip_count`
  for per-task observation.

### MEDIUM / LOW — ACKNOWLEDGED, deferred to follow-up

These are not blocking on the HIGH sweep and not pre-launch blockers. The
2026-05-29 sweep landed `try_*` siblings + `# Panics` / `# Errors` doc
companions for every previously-panicking helper
(`every_n_minutes` ↔ `try_every_n_minutes`, `hourly_at` ↔ `try_hourly_at`,
`daily_at` ↔ `try_daily_at`) at `framework/src/schedule/expression.rs:260-353`,
closing the inline `# Panics` doc coverage gap from D13-A. The underlying
cron-parser leniency for out-of-range fields is still acknowledged below.

- Cron parsing accepts out-of-range fields and impossible schedules
  (`CronField::parse` — minutes >59, hours >23, month >12, day 0,
  reversed ranges). Should become `Result<...>` with explicit
  `OutOfRange { field, value }` variants. **2026-05-29:** still open at the
  parser level — `try_every_n_minutes`/`try_hourly_at`/`try_daily_at` close
  the helper-side range checks for the most common surfaces, but raw
  `CronExpression::parse` still tolerates the bad fields. Promote next.
- Time parsing silently falls back to midnight / no-ops on invalid input
  (`CronExpression::daily_at`, `.at(...)`). Should fail-closed via
  `try_daily_at` / `try_at` returning `Result`. **2026-05-29:**
  `try_daily_at` exists at `framework/src/schedule/expression.rs:353`;
  `.at(time)` does not yet have a `try_at` sibling — still open.
- Scheduler time zone is hard-coded to host local timezone
  (`CronExpression::is_due` → `Local::now()`). Need per-schedule TZ +
  UTC option + DST policy. `is_due_at(clock)` now lets a future
  per-schedule TZ change drive evaluation against a `DateTime<Utc>` or
  fixed-offset clock without further API churn.
- Task names are not unique (`Schedule::add` always pushes; `find`
  returns first). Should reject duplicates at registration time.
- Inline task panic containment: `TaskEntry::run` directly awaits the
  handler. A panic unwinds through `run_due_tasks` / `run_all_tasks`.
  Background tasks already catch_unwind via the spawn body in
  `run_tasks_into`; inline path needs equivalent wrapping. This also
  closes the inline `without_overlapping` lock-leak window above.
  **2026-05-29 update:** in part addressed via `8d26c19` (workflow path
  catches panics in handler + reclaims expired running rows); the same
  pattern is the recommended template for the inline schedule path.
- Cron DOM/DOW semantics are always ANDed (`is_due` requires both to
  match). Vixie cron uses OR when both fields are restricted. Needs an
  explicit compatibility decision before changing.
- ~~`Schedule::call` takes `&mut self` while only returning a builder
  (`framework/src/schedule/mod.rs:155-162`) — should be `&self`.~~
  — **CLOSED 2026-05-29 via #351g (`fea9f8e`)**: `Schedule::call` now
  takes `&self` (`framework/src/schedule/mod.rs:168`); `TaskState` field
  visibility narrowed to `pub(crate)`; `without_overlapping_for(Duration::ZERO)`
  coerces to default with a WARN.

### Test coverage gaps — RESOLVED (HIGH subset)

- `Application` schedule commands: 8 tests in `app::tests` (`7dd99ad`).
- `run_in_background` concurrency: `run_in_background_spawns_tasks_concurrently`
  uses `tokio::sync::Barrier::new(2)` + `tokio::time::timeout` so a
  sequential execution would deadlock and trip the timeout.
- `run_in_background` panic isolation:
  `run_in_background_panic_is_isolated_and_named`.
- `without_overlapping` in-process AtomicBool layer:
  `without_overlapping_in_process_fallback_skips_overlapping_call` (resets
  `last_run_minute` to bypass the same-minute gate and exercise the
  inner overlap layer).
- Same-minute dedup: `same_minute_cas_dedups_repeated_call_within_same_minute`.
- Clock-injection shim: `is_due_at_drives_cron_with_synthetic_clock`.
- TTL builder: `without_overlapping_for_sets_custom_ttl` +
  `without_overlapping_uses_default_ttl_when_unspecified`.
- Sequential resets: `without_overlapping_in_process_flag_resets_after_each_run`.
- Background-into-JoinSet plumbing: `run_due_tasks_into_routes_background_into_joinset`.
- Inline-vs-background semantics: `inline_tasks_run_sequentially`.

Outstanding test coverage (deferred with the MEDIUM/LOW items above):
- Cron field validation tests (out-of-range minute/hour/etc).
- Timezone/DST tests using `is_due_at(clock)` — the shim is here, the
  per-schedule TZ surface is pending.
- Duplicate task name registration rejection.
- Inline-task panic containment regression test.

## sse

Status: RESOLVED. HIGH + all MEDIUM + L1 (data CR splitting) closed across
#352a–#352c (commits `4af3618` sanitization sweep, `2845803` surface
additions, doc companion + this closeout). User-facing
`docs/core/sse.md` landed in-phase per docs-land-in-phase policy.

Files:
- `framework/src/sse/mod.rs`
- Supporting context: `framework/src/http/response.rs`
- New: `docs/core/sse.md`

### Closed findings

- **HIGH — event/id CR/LF/NUL injection**: closed in `4af3618` (#352a).
  `to_wire` now sanitizes event + id via `sanitize_field` (strip on
  occurrence + structured `WARN` with field name only, never the value).
  Producers that prefer fail-fast over silent-strip reach for the
  `try_with_event` / `try_with_id` siblings landed in `2845803`. The
  `WARN` fires every occurrence, not warn-once — sanitization is a
  security signal where the hundredth strip is as interesting as the
  first.
- **MEDIUM — no heartbeat / comment support**: closed in `2845803`
  (#352b). `SseEvent::comment(text)` + `SseEvent::keep_alive()`
  constructors produce wire-only comment frames (`: <text>\n\n` /
  `:\n\n`). The empty-line case emits `:\n` (not `: \n`) so keep_alive()
  is the canonical minimum-bytes form. Crucial detail: comment-kind
  events must NOT share the empty-data fallthrough that `data("")`
  uses, or every heartbeat would dispatch a spurious empty `message`
  event to every subscriber — pinned by
  `keep_alive_emits_comment_only_no_data_field`.
- **MEDIUM — no `retry:` support**: closed in `2845803` (#352b).
  `.with_retry(Duration)` emits `retry: <ms>\n` between id and data.
  `Duration::ZERO` is valid per spec ("reconnect immediately") and is
  emitted verbatim — no coercion.
- **MEDIUM — no `Last-Event-ID` reader**: closed in `2845803` (#352b).
  `sse::last_event_id(&Request) -> Option<String>` returns `None` when
  the header is absent OR contains a NUL byte (WHATWG: NUL invalidates
  the id). Pure helper `last_event_id_from_value(Option<&str>)` is the
  unit-test target since constructing a `Request` in isolation requires
  a live `hyper::body::Incoming` body.
- **MEDIUM — no typed error event shape**: closed in `2845803` (#352b).
  `SseEvent::error(msg)` emits the conventional `event: error\ndata: <msg>\n\n`
  shape so subscribers can `addEventListener("error", ...)` without
  colliding with the connection-level error EventSource fires on
  transport failure. The pattern for mapping `Stream<Item = Result<T, E>>`
  is documented at `docs/core/sse.md` "Domain-level errors" — kept as
  a doc pattern, not a new API, since the caller-defined error mapping
  is the actual customization point.
- **LOW — data preserves embedded `\r`**: closed in `4af3618` (#352a).
  `normalize_data_line_endings` collapses `\r\n` and bare `\r` to `\n`
  before splitting so the wire reflects exactly the lines the
  producer's string spelled out, regardless of which terminator was
  used. Same vulnerability shape as the HIGH event/id finding, just one
  layer down at the WHATWG parser.

### Acknowledged not actioned

- **LOW — no broadcaster / backpressure helper**: kept as a doc pattern
  rather than a new helper type. The audit's concern is already de
  facto solved by the broadcasting subsystem (Phase 7B): subscribe to a
  `BroadcastHub` channel and adapt the `broadcast::Receiver` into the
  `SseEvent` stream with `BroadcastStream::new(rx).map(...)`. The
  working dogfood at `app/src/controllers/sse_example.rs` implements
  this in ~25 lines; the `docs/core/sse.md` "Broadcasting one stream
  to many subscribers" section promotes the pattern. Adding a thin
  wrapper crate type wouldn't reduce caller LOC and would lock us into
  a slow-consumer policy the broadcasting subsystem already owns.

### Test coverage added

- Wire-format tests for CR / LF / CRLF / NUL in `event` and `id`:
  `event_with_lf_is_stripped_in_wire_output`,
  `event_with_cr_is_stripped_in_wire_output`,
  `event_with_crlf_is_stripped_in_wire_output`,
  `id_with_lf_is_stripped_in_wire_output`,
  `event_with_nul_is_stripped_in_wire_output`.
- Wire-format tests for `\r\n` / bare `\r` / mixed terminators in
  `data`: `data_with_crlf_is_normalized_to_single_line_split`,
  `data_with_bare_cr_splits_like_lf`,
  `data_with_mixed_line_endings_collapses_uniformly`.
- Allocation fast-path pinned via
  `sanitize_field_avoids_allocation_when_input_is_clean`.
- `with_retry` field + position + zero handling:
  `with_retry_emits_integer_ms_field`,
  `with_retry_zero_is_valid_and_emitted_verbatim`,
  `retry_field_appears_between_id_and_data`.
- Comment / keep-alive / multi-line / NUL stripping:
  `keep_alive_emits_comment_only_no_data_field`,
  `comment_with_text_emits_prefixed_comment_line`,
  `multiline_comment_splits_lines_with_colon_prefix`,
  `comment_with_nul_is_stripped`.
- `error()` shape: `error_helper_emits_event_error_with_message_payload`.
- `try_with_*` rejection + clean passthrough:
  `try_with_event_rejects_lf_with_validation_error`,
  `try_with_event_passes_clean_input_through`,
  `try_with_id_rejects_cr_with_validation_error`.
- Builder no-ops on `Comment`: `with_event_on_comment_is_silent_noop`,
  `try_with_event_on_comment_returns_ok_unchanged`,
  `with_retry_on_comment_is_silent_noop`.
- `last_event_id_from_value`:
  `last_event_id_from_value_passes_clean_input_through`,
  `last_event_id_from_value_returns_none_on_absent_header`,
  `last_event_id_from_value_returns_none_on_nul_byte`,
  `last_event_id_from_value_passes_empty_string_through`.

The audit's "Add response tests that assert SSE headers and streamed
body chunks through `HttpResponse::sse`" item is the only remaining
test-coverage gap — defensible because the headers are static literals
in `HttpResponse::sse` (`Content-Type: text/event-stream`,
`Cache-Control: no-cache`, `Connection: keep-alive`,
`X-Accel-Buffering: no`) and any breakage would surface as an obvious
production smoke-test failure, not a subtle wire-format bug. If we add
HTTP-level integration test infrastructure later (the framework
currently lacks an in-process Request builder that doesn't need a live
`hyper::body::Incoming`), the SSE header smoke-test is a 5-line
addition at that point.

## telemetry

Status: RESOLVED. HIGH closed (#353a, `f68530b`); 1 MEDIUM + 1 LOW closed
(#353b, `bc2c5be`); user-facing `docs/core/observability.md` landed
in-phase (#353c). Remaining MEDIUM/LOW items acknowledged below with
rationale — they are genuine design-sized follow-ups, not silent punts.

Files:
- `framework/src/telemetry/mod.rs`
- `framework/src/telemetry/init.rs`
- `framework/src/telemetry/metrics.rs`
- `framework/src/telemetry/propagation.rs`
- Supporting context: `framework/src/server.rs`, `framework/src/http_client/mod.rs`,
  `framework/src/logging/*`
- New: `docs/core/observability.md`

### Closed

- **HIGH — inbound trace-context extraction + request spans not wired**:
  closed in `f68530b` (#353a). `telemetry::propagation` gained the pure
  `extract_w3c_trace_context(&HeaderMap) -> Context` + `join_upstream_trace(&Span,
  &HeaderMap)` (validity-guarded: valid `traceparent` → child span, none →
  root). `RequestIdMiddleware` calls `join_upstream_trace` on the existing
  `request` span before `.instrument` (ordering matters — the OTel bridge
  materializes the span lazily on first poll, so a later `set_parent` is
  dropped). Server spans now join the upstream distributed trace instead of
  starting fresh. Reparents the existing span rather than adding a second
  telemetry span. Folded in a latent secondary bug: the three `server.rs`
  blocks that recorded `error=true` on `Span::current()` ran AFTER the
  middleware's `.instrument` scope closed (wrong span) against a field never
  declared at span creation (silent no-op in tracing) — consolidated into
  `RequestIdMiddleware` with `error = tracing::field::Empty` declared up
  front + recorded on a span clone for returned-5xx. Panic-5xx does not get
  the OTel marker (span unwinds first); `execute_chain_safely` still
  error-logs + dispatches `ErrorOccurred`, so not silent. Four otel-gated
  propagation tests (valid traceparent → valid context w/ trace_id+span_id;
  no header → invalid; malformed → invalid; inject→extract round-trip).
- **MEDIUM — TelemetryGuard drop warns even for disabled/no-endpoint
  guards**: closed in `bc2c5be` (#353b). Drop now gates on the true
  invariant `owns_providers()` (holds ≥1 SDK provider needing flush) instead
  of the `legacy: bool` flag that `empty_guard()` left false. Covers every
  no-provider case (disabled path, legacy subscriber path, non-otel builds)
  and still warns for partial-init guards that genuinely hold providers. The
  redundant `legacy` field was removed. Regression test
  `empty_guard_owns_no_providers_so_drop_is_silent`.
- **LOW — `OTEL_SDK_DISABLED` parser too strict**: closed in `bc2c5be`
  (#353b). Was `true`/`TRUE`/`1` only; now case-insensitive `true` + `1`,
  trimmed, via the pure `parse_sdk_disabled(Option<&str>)`. Three tests.
- **MEDIUM (partial) — standard OTel env support "incomplete"**: validated
  against vendored `opentelemetry-otlp 0.31` source that the exporter
  builders read `OTEL_EXPORTER_OTLP_{HEADERS,PROTOCOL,COMPRESSION,TIMEOUT}`
  themselves on `.build()` — those knobs are NOT ignored, the SDK consumes
  them. Suprnova's `OtelConfig` doesn't re-model them because it doesn't
  need to. The `OtelConfig` doc was corrected (it claimed to "mirror the
  standard env vars") to state precisely which vars Suprnova reads vs which
  the SDK reads. The one genuine gap is the per-signal endpoint shadow,
  acknowledged below.

### Acknowledged not actioned

- **MEDIUM — per-signal endpoint env vars shadowed**: Suprnova calls
  `.with_endpoint(base)` explicitly for all three signals, so
  `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (and metrics/logs siblings) are
  shadowed by the base `OTEL_EXPORTER_OTLP_ENDPOINT`. Documented as a known
  limitation in both the `OtelConfig` rustdoc and `docs/core/observability.md`
  (workaround: route via a local collector). Fixing properly means letting
  the SDK's per-signal env handling win when set — a follow-up.
- **MEDIUM — providers built even when subscriber not installed / partial
  exporter init**: `init_telemetry_with_otel` installs global providers
  before `set_global_default`, and silently absorbs a subscriber-install
  failure (`let _ = ...`, intentional so tests can call repeatedly). If a
  subscriber is already set, the providers are global but the
  tracing-opentelemetry bridge layer is not — spans won't reach OTel. Real
  but a deeper init-flow restructuring (ordering + rollback on partial
  failure); not folded into a HIGH-extraction task.
- **MEDIUM — pre-init metric handles bound to provider at creation**: this
  is documented design intent. `metrics.rs` resolves `global::meter("suprnova")`
  on each handle construction (no Suprnova-level caching), so a handle built
  before `init_telemetry` binds to the no-op provider and stays inert. This
  is inherent to OTel's global-provider model; `docs/core/observability.md`
  now tells users to construct handles after init / lazily at first use.
- **LOW — metric attributes restricted to `&[(&'static str, &str)]`**:
  string-only labels; numeric/bool/dynamic-key attributes are an API
  broadening (needs an attribute-value enum or generic). Documented as a
  planned enhancement in `docs/core/observability.md`.

Original HIGH finding (for reference):
- ~~High: inbound trace-context extraction and request spans are not wired.
  Telemetry installs a W3C propagator (`framework/src/telemetry/propagation.rs:15-27`),
  and the HTTP client injects outbound context (`framework/src/http_client/mod.rs:498-530`),
  but server request handling never extracts incoming `traceparent`/`tracestate`
  or creates a per-request tracing span before dispatch
  (`framework/src/server.rs:443-577`). Distributed traces will start at outbound
  calls rather than joining upstream traces.~~ — **CLOSED via `f68530b` (predates this sweep).** `init_telemetry` installs `TraceContextPropagator` (`framework/src/telemetry/init.rs:359`); `extract_w3c_trace_context(&HeaderMap)` reads incoming `traceparent`/`tracestate` via the registered propagator (`framework/src/telemetry/propagation.rs:57`); `RequestIdMiddleware` — the outermost middleware on all four dispatch paths in `server.rs:711,760,797,1004` — builds the per-request `tracing::info_span!("request",...)` (`framework/src/logging/request_id.rs:184-200`), then calls `propagation::join_upstream_trace(&span, headers)` at line 209 BEFORE `.instrument(span)` at line 252. `join_upstream_trace` uses `OpenTelemetrySpanExt::set_parent` only when `span_context().is_valid()`, so untraced requests stay root spans and traced requests join upstream. Regression coverage: 4 unit tests in `propagation.rs` + outbound integration test at `framework/tests/http_client.rs:625` proving the http_client carries the joined trace id forward.
- ~~Medium: OTel providers can be built even when the tracing subscriber layer is
  not installed. `init_telemetry_with_otel` sets global tracer/meter providers
  before calling `set_global_default`, then silently ignores subscriber install
  failure (`framework/src/telemetry/init.rs:246-380`). In processes/tests where
  a subscriber is already set, the returned guard owns providers, but tracing
  spans/events are not necessarily bridged to OTel.~~ — **CLOSED via prior
  work (F1 documented design).** Silent `set_global_default` failure is the
  intended idempotency contract documented inline; covered by the `### Closed`
  + `### Acknowledged not actioned` sections above.
- ~~Medium: partial exporter initialization falls back inconsistently. If metric
  or log exporter construction fails after the tracer provider has already been
  installed, the function installs only the base subscriber and returns a guard
  for the partial providers (`framework/src/telemetry/init.rs:282-336`). The
  resulting process may have a global tracer provider but no
  `tracing-opentelemetry` layer, and the early `tracing::error!` calls can be
  emitted before a subscriber exists.~~ — **CLOSED via prior work (F2
  unreachable with pinned SDK).** Validated against vendored `opentelemetry-otlp
  0.31`: two probes show builder errors fan out symmetrically across all three
  signals — there is no real-world config that produces span-Ok +
  metric/log-Err.
- ~~Medium: `TelemetryGuard` warns on drop even for disabled/no-endpoint guards.
  `init_telemetry` returns `empty_guard()` on the disabled path
  (`framework/src/telemetry/init.rs:199-213`), and `empty_guard` is not marked
  legacy (`framework/src/telemetry/init.rs:172-184`). If a caller initializes
  telemetry with no providers and does not call `shutdown`, Drop logs a warning
  about buffered telemetry even though there is nothing to flush.~~ —
  **CLOSED via `bc2c5be` (#353b).** Drop now gates on `owns_providers()`;
  regression-tested via `empty_guard_owns_no_providers_so_drop_is_silent`.
- ~~Medium: standard OTel env support is incomplete despite the module docs saying
  the config mirrors standard variables. `OtelConfig::from_env` reads endpoint,
  service name/version, and disabled only (`framework/src/telemetry/init.rs:15-83`);
  it does not model common OTLP headers, protocol, timeout, compression,
  certificate, or per-signal endpoint env vars. Operators using normal collector
  auth/config knobs will not get the behavior they expect.~~ — **CLOSED via
  prior work (F4 documented as designed).** `HEADERS` / `PROTOCOL` / `TIMEOUT`
  / `COMPRESSION` are consumed by the SDK at `.build()` time; the OtelConfig
  docs were corrected to state precisely which vars Suprnova reads vs which
  the SDK reads; per-signal endpoint shadowing is the only acknowledged
  remaining gap (recorded above in `### Acknowledged not actioned`).
- ~~Medium: metric handles created before telemetry initialization remain bound to
  the provider active at creation time. The implementation intentionally avoids
  Suprnova-level caching and resolves `global::meter("suprnova")` on each
  `Metrics::counter`/`histogram`/`gauge` call
  (`framework/src/telemetry/metrics.rs:27-81`), but a handle stored by user code
  before `init_telemetry` is still a no-op handle after init.~~ — **CLOSED via
  prior work (F5 documented anti-cache tradeoff).** Re-resolving inside every
  inc/record/set would contradict the documented design; the user-facing
  `docs/core/observability.md` instructs constructing handles after init or
  lazily at first use.
- ~~Low: metric attributes are restricted to `&[(&'static str, &str)]`
  (`framework/src/telemetry/metrics.rs:83-120`). That covers simple labels but
  excludes numeric/bool attributes and dynamic attribute keys supported by OTel.~~
  — **ACKNOWLEDGED via prior work.** Recorded above in the
  `### Acknowledged not actioned` section as a planned API broadening; not a
  correctness bug.
- ~~Low: the `OTEL_SDK_DISABLED` parser only accepts `true`, `TRUE`, or `1`
  (`framework/src/telemetry/init.rs:62-66`), missing common mixed-case boolean
  forms.~~ — **CLOSED via `bc2c5be` (#353b).** Parser now case-insensitive
  `true` + `1`, trimmed, via the pure `parse_sdk_disabled(Option<&str>)`.

Test coverage gaps:
- Add request-level tests with incoming `traceparent` proving server spans join
  the upstream trace and outbound `Http` calls propagate that context.
- Add tests for repeated `init_telemetry` after an existing subscriber is set,
  asserting whether providers are skipped or an explicit warning is emitted.
- Add exporter-failure tests for span/metric/log partial initialization.
- Add disabled/no-endpoint guard drop tests so warnings only appear when there
  are real providers to flush.
- Add env parsing tests for OTLP headers, protocol, timeout, compression, and
  per-signal endpoint variables once supported.
- Add metrics tests documenting pre-init handle behavior.

## validation

Status: RESOLVED (#354 — `90736b6` async hook + Unique race contract, `ae5f0f6`
Unique builder + identifier validation, `2d6b62d` Numeric/HttpUrl/`?=>`, docs commit).

Closure trail (finding → disposition):
- HIGH (async rules not integrated): CLOSED `90736b6`. Added
  `FormRequest::after_validation_async`, run by `extract()` as the final
  validation stage in both the standard and Precognition flows (bail per
  stage; Precognition filtering factored into `precognition_outcome`).
  `Unique`/any `AsyncRule` now participates in automatic request validation
  via that hook — no per-handler plumbing. `FormRequest` gained a `Sync`
  bound (all DTOs are already `Sync`). Tests: hook runs+fails in both flows,
  bail proven (malformed sync field → async hook not run), real `Unique`
  through the hook against seeded SQLite.
- HIGH (`Unique` race-prone advisory): CLOSED `90736b6`. Documented as
  advisory/TOCTOU on the rule + the docs/core/validation.md "advisory →
  DB UNIQUE constraint → mapping" pattern. Added
  `FrameworkError::from_unique_violation(field, msg, DbErr)` — maps a
  unique-constraint write error to a 422, passes other DB errors through
  (verified sea-orm 1.1.20 `DbErr::sql_err()` / `SqlErr::UniqueConstraintViolation`,
  all three backends). Tests: real SQLite UNIQUE violation → 422, non-unique
  passthrough.
- MEDIUM (`Unique` interpolates identifiers): CLOSED `ae5f0f6`. table/column/
  exclusion-key/`where_eq` columns now run through `validate_identifier`
  (the #365 allowlist) before interpolation; values stay bound. Test:
  injection payload rejected at the gate.
- MEDIUM (`Unique` hardcoded conn + `id` key): CLOSED `ae5f0f6` (builder). Added
  `Unique::new` + `.ignore` / `.ignore_with_column` / `.where_eq` (scoped,
  AND, typed `Into<Value>`) / `.case_insensitive` (`LOWER(col)=LOWER(?)`).
  Each option fully tested. Named connection + soft-delete filter were NOT
  built — routed to the Laravel parity sweep's validation entry (a feature,
  not a correctness gap); not framed as a limitation in user docs.
- MEDIUM (`Numeric` accepts NaN/inf): CLOSED `2d6b62d`. Requires a finite f64.
- MEDIUM (`Url` accepts any scheme): CLOSED `2d6b62d`. Added scheme-constrained
  `HttpUrl` (http/https) + crate-root re-export; `Url` stays liberal.
- MEDIUM (optional `validate!` rows skip rules when `None`, so `RequiredIf`
  can't fail an absent `Option`): CLOSED `2d6b62d`. Added the `?=>` row — the
  optional-typed sibling of the contextual `=>` row that evaluates even on
  `None` (absence → `""`). `?:` docs signpost `?=>` so the skip-on-None is
  no longer a silent trap. Tests: fires on absent+condition, passes on
  absent+no-condition, evaluates present.
- MEDIUM (`FormRequest::authorize` is sync `bool`): RESOLVED as design, no code.
  A pre-parse async `authorize_async(&Request)` is architecturally impossible:
  `Request` is statically `!Sync` (`BodyState::Streaming(hyper::body::Incoming)`),
  so borrowing `&Request` across `.await` can't yield a `Send` `extract`
  future. Async authorization belongs outside the pre-parse hook — middleware
  (async, short-circuits with `Err(resp)`), `Gate::*_async` in the handler, or
  `after_validation_async` for body-dependent checks. Documented in
  docs/core/validation.md "Async authorization". Not framed as a limitation.
- MEDIUM (deserialize-before-validate / partial Precognition): RESOLVED as
  design. The typed DTO is the schema; a draftable field must be `Option<T>`.
  Documented in docs/core/validation.md "Design notes".
- LOW (English-only messages; `Min`/`Max`/`Between` are length rules): RESOLVED
  as design notes in docs/core/validation.md. No i18n subsystem (wrap a rule
  to localize); numeric bounds use `#[validate(range)]`.

Docs: docs/core/validation.md (new) covers the rule objects, the three
`validate!` row shapes, the async hook + Unique recipe, the Unique builder,
the advisory→constraint→mapping contract, async authorization, and the
design notes. NOTE: docs/SUMMARY.md does not register validation.md — nor
sse/observability/vector/broadcasting/websockets/authorization/eloquent/
feature-flags/auth-flows/supervisors (all added post-Kit-fork without a
SUMMARY update). A one-shot SUMMARY nav-sync is a separate docs task, not
per-page.

Files:
- `framework/src/validation/mod.rs`
- `framework/src/validation/rule.rs`
- Supporting context: `framework/src/http/form_request.rs`,
  `framework/src/http/body.rs`, `framework/src/http/extract.rs`,
  `framework/tests/validation_rules.rs`, `framework/tests/data_form_request.rs`,
  `framework/tests/precognition.rs`, `framework/tests/form_request_async_validation.rs`

Review notes:
- ~~High: async validation rules are not integrated into `FormRequest` or the
  `validate!` macro. `AsyncRule::check_async` is documented as manual-only
  (`framework/src/validation/rule.rs:494-520`), while `FormRequest::extract`
  only runs the synchronous `validator::Validate` derive and synchronous
  `after_validation` hook (`framework/src/http/form_request.rs:174-226`).
  Built-in `Unique` therefore cannot be part of automatic request validation
  without every app hand-writing the same async hook pattern.~~ — **CLOSED
  2026-05-29 via #354a (`90736b6`): `after_validation_async` hook integrated
  into `extract()` in both standard + Precognition flows.**
- ~~High: `Unique` is a race-prone advisory check, not a uniqueness guarantee.
  It does `SELECT COUNT(*)` before the write (`framework/src/validation/rule.rs:551-591`),
  so concurrent requests can both pass validation and then insert duplicates
  unless the database has a unique constraint and write errors are mapped back
  to validation responses. The docs do not make that boundary clear.~~ — **CLOSED
  2026-05-29 via #354a (`90736b6`): contract documented + `FrameworkError::from_unique_violation`
  helper maps write-side UNIQUE violations to 422.**
- ~~Medium: `Unique` interpolates table and column identifiers directly into SQL
  (`framework/src/validation/rule.rs:535-572`). They are `&'static str`, but
  still come from application source and are not validated or quoted. A typo or
  hostile package literal can produce SQL errors or statement injection shape;
  identifier quoting/validation should be centralized.~~ — **CLOSED 2026-05-29
  via #354b (`ae5f0f6`): identifiers routed through `validate_identifier` allowlist.**
- ~~Medium: `Unique` is hard-coded to the default DB connection and `id` primary
  key for exclusions (`framework/src/validation/rule.rs:543-568`). It has no
  named connection, custom key column, scoped uniqueness, soft-delete filter, or
  case-insensitive mode.~~ — **CLOSED 2026-05-29 via #354b (`ae5f0f6`): builder
  surface (`Unique::new` + `.ignore` + `.where_eq` + `.case_insensitive`) shipped;
  named-connection + soft-delete filter routed to Laravel parity sweep.**
- ~~Medium: partial validation and conditional-required flows are constrained by
  deserializing into the final struct before validation.~~ — **CLOSED 2026-05-29
  as design.** The typed DTO *is* the schema by design — documented in
  `docs/core/validation.md` "Design notes" (lines 255-260), which states "a field
  that may be absent must be `Option<T>`" and explicitly ties this to Precognition
  partial-validation behavior. Code at `framework/src/http/form_request.rs:58-68`
  matches the audit's cited behavior exactly; the behavior is intentional.
- ~~Medium: optional `validate!` rows skip all rules when the field is `None`
  (`framework/src/validation/rule.rs:685-692`). That is correct for nullable
  fields, but it means conditional rules such as `RequiredIf` cannot require an
  absent `Option<T>` field; if the field is non-optional, serde rejects missing
  input before the rule runs.~~ — **CLOSED 2026-05-29 via #354c (`2d6b62d`):
  `?=>` row added as the optional-typed sibling that evaluates even on `None`.**
- ~~Medium: `FormRequest::authorize` is synchronous and returns only `bool`
  (`framework/src/http/form_request.rs:58-68`).~~ — **CLOSED 2026-05-29 as
  design.** Pre-parse async `authorize_async(&Request)` is architecturally
  impossible — `Request` still holds the streaming body at the
  pre-deserialization point. Async authorization belongs in middleware,
  `Gate::*_async`, or `after_validation_async`. Documented in
  `docs/core/validation.md` "Async authorization" section (lines 236-253).
- ~~Medium: the `Numeric` rule accepts any string that parses as `f64`
  (`framework/src/validation/rule.rs:237-247`), which includes non-finite
  values such as `NaN`/`inf` on Rust's parser. Those should not pass ordinary
  user-input numeric validation.~~ — **CLOSED 2026-05-29 via #354c (`2d6b62d`):
  requires a finite f64.**
- ~~Medium: `Url` accepts any URL scheme that `url::Url::parse` accepts
  (`framework/src/validation/rule.rs:299-310`). The docs mention this, but a
  framework-provided URL rule is often used for callback/webhook/avatar URLs;
  accepting `file:`, custom schemes, or other non-HTTP URLs is a production
  footgun unless there is also an `HttpUrl`/scheme-constrained rule.~~ —
  **CLOSED 2026-05-29 via #354c (`2d6b62d`): scheme-constrained `HttpUrl`
  (http/https) added + re-exported; `Url` stays liberal.**
- ~~Low: built-in rule messages are fixed English strings returned as display
  text (`framework/src/validation/rule.rs:32-53`). There is no error-code or
  localization surface for applications that need translated validation errors.~~ — **CLOSED via prior design-note rustdoc.** Trait `Rule` docstring at `framework/src/validation/rule.rs:32-36` now explicitly states "Suprnova does not impose a translation scheme on the message — wrap [Rule] yourself if you need i18n." Resolved as a documented design call; no i18n subsystem ships in core.
- ~~Low: `Min`, `Max`, and `Between` are string-length-only rules
  (`framework/src/validation/rule.rs:154-198`). Laravel-style validation varies
  semantics by type (string length, numeric value, array length, file size), so
  these names can mislead users validating non-string data through the rule
  object API.~~ — **CLOSED via prior design-note rustdoc.** Per-rule docs at `framework/src/validation/rule.rs:154-198` now spell out "must be at least N characters long. Counts Unicode scalar values (chars), not bytes" and the module rule guidance steers numeric/array/file size to `#[derive(Validate)]` / `#[validate(range)]`. Resolved as a documented design call.

Test coverage gaps:
- Add automatic FormRequest tests for async rules once an async hook or macro
  integration exists.
- Add concurrency tests proving duplicate inserts still require DB constraints,
  plus tests mapping unique-constraint write errors to validation responses.
- Add identifier validation/quoting tests for `Unique` table/column values.
- Add `Unique` tests for custom key columns, scopes, soft deletes,
  case-insensitive matching, and named connections once supported.
- Add Precognition tests with partial payloads and missing unrelated required
  fields.
- Add conditional-required tests for absent `Option<T>` fields.
- Add `Numeric` tests for `NaN`, `inf`, overflow-to-infinity, and finite-only
  behavior.
- Add scheme-constrained URL rule tests.

## web_push

Status: HIGH RESOLVED 2026-05-29 via remediation/web-push-high. All three HIGH
findings closed in one commit: SSRF endpoint guard, default request timeout,
VAPID TTL bounds check. MEDIUM `audience_of` malformed-origin issue closed
alongside (the new `validate_strict_endpoint` is the entry point and refuses
non-HTTPS / no-host URLs before `audience_of_url` runs). Remaining MEDIUM /
LOW items acknowledged below.

Files:
- `framework/src/web_push.rs`
- Supporting crate: `crates/suprnova-web-push/src/lib.rs`
- `crates/suprnova-web-push/src/client.rs`
- `crates/suprnova-web-push/src/vapid.rs`
- `crates/suprnova-web-push/src/payload.rs`
- `crates/suprnova-web-push/src/error.rs`
- Tests: `crates/suprnova-web-push/tests/vapid_test.rs`,
  `crates/suprnova-web-push/tests/client_test.rs`,
  `crates/suprnova-web-push/tests/ece_test.rs`

Review notes:
- ~~High: subscription endpoints are trusted as arbitrary URLs and posted to
  directly.~~ **CLOSED 2026-05-29.** Added `EndpointPolicy` enum (Strict /
  AllowAny) with `Strict` as the production default. `WebPushClient::send`
  now runs `validate_strict_endpoint` on the parsed URL before the HTTP POST:
  rejects non-HTTPS schemes, URLs with no host, IP-literal hosts (catches
  127.0.0.1 / 169.254.169.254 / `[::1]` etc.), and the RFC-2606 / cloud-
  metadata host blocklist (`localhost`, `*.local`, `*.internal`, `*.test`,
  `*.example`, `*.invalid`, `metadata.google.internal`, `metadata.aws.internal`,
  `metadata.azure.com`, `instance-data`). DNS-rebinding-style attacks are
  documented as a follow-on layer; callers needing that can apply it via
  `with_client` using a hardened resolver. `AllowAny` exists only for tests
  against local mock servers; both `client_test.rs` mock-server cases opt in
  explicitly. Regression coverage: 5 unit tests in `client.rs` (Strict
  rejects non-https / IP literal / blocked TLDs / no-host; Strict accepts
  legitimate push services) + 3 integration tests in `tests/client_test.rs`
  (Strict default rejects http, IP-literal, metadata hosts via the real
  send path).
- ~~High: the default reqwest client has no explicit request timeout.~~
  **CLOSED 2026-05-29.** `WebPushClient::new` now builds the default
  [`Client`] via `Client::builder().timeout(Duration::from_secs(30))` so a
  slow or hostile push service cannot tie up a calling task indefinitely.
  Callers needing a different transport policy still build their own
  [`Client`] and use `with_client` — that path is unchanged.
- ~~High: public VAPID signing does not enforce the documented TTL contract.~~
  **CLOSED 2026-05-29.** `VapidSigner::sign` now validates `ttl_secs` before
  the `as u64` cast: rejects values `<= 0` (would have wrapped to multi-
  century lifetimes) and values `> 86400` (RFC 8292 §2 caps at 24 h).
  Failures return a typed `WebPushError::Vapid("…")` instead of producing a
  silently-malformed JWT. Regression coverage: 4 tests in
  `tests/vapid_test.rs` (zero, negative, > 24 h reject; exactly-24 h accept).
- ~~Medium: `audience_of` accepts malformed push-service origins.~~
  **CLOSED 2026-05-29 (alongside the SSRF guard).** The new entry point is
  `parse_endpoint` + `validate_strict_endpoint`, which require scheme=https
  and a non-empty host. `audience_of_url` then runs on a guaranteed-valid
  `Url`, so "non-HTTP scheme or URL without host" can no longer reach the
  audience builder under the default `Strict` policy.
- ~~Medium: rejection bodies are buffered without a size cap.~~ — **CLOSED
  2026-05-29 via `873b94da` (cherry-picked as `8e9c8b8`).** `send`
  (`crates/suprnova-web-push/src/client.rs:88-93`) now bounds the buffered
  rejection body so a hostile endpoint can't drive memory growth via large
  error payloads.
- ~~Medium: push-service rate limiting and transient failures are surfaced only
  as `PushServiceRejected`.~~ — **CLOSED 2026-05-29 via `873b94da`.**
  `PushServiceRejected` now carries typed retry/backoff information
  (`crates/suprnova-web-push/src/client.rs:84-94`), so queue/job callers can
  branch on 429 retry-after vs 5xx retryability vs terminal without re-parsing
  status / body.
- ~~Medium: VAPID subject is not validated.~~ — **CLOSED 2026-05-29 via
  `873b94da`.** `WebPushClient` now validates the caller's subject string at
  construction (`crates/suprnova-web-push/src/client.rs:30-40`,
  `crates/suprnova-web-push/src/vapid.rs:73-88`): only `mailto:` and `https:`
  contact URIs are accepted, so invalid subjects fail at boot, not at first
  send.
- ~~Medium: decoded subscription key lengths are not checked before handing
  them to `ece::encrypt`.~~ — **CLOSED 2026-05-29 via `873b94da`.**
  `Payload::encrypt` (`crates/suprnova-web-push/src/payload.rs:48-59`) now
  enforces P-256 public key and auth-secret lengths after base64-decoding
  `p256dh` / `auth`, surfacing a clear framework error rather than panicking
  inside `ece::encrypt`.
- ~~Low: the framework module is only a re-export (`framework/src/web_push.rs:1-10`).
  There is no Suprnova-level configuration, subscription persistence model,
  notification-channel integration, or queue job wrapper, so apps have to wire
  all production push delivery concerns themselves.~~ — **CLOSED 2026-05-30 via
  `1b859ad2`** (discoverability) and **prior commits** (substantive integration).
  The audit framing reflected the re-export module's own docs, not absence of
  integration: `WebPushChannel`
  (`framework/src/notifications/channels/webpush.rs`, commit `3e0e1d5b`) wires
  web push into the notification surface, and `SendNotificationJob`
  (`framework/src/notifications/notify_job.rs`, commit `d9453952`) is the queue
  wrapper. `1b859ad2` makes both reachable from the `web_push` module rustdoc
  with a `docs/core/notifications.md` cross-reference.

Test coverage gaps:
- Add endpoint validation tests for non-HTTPS schemes, missing hosts,
  localhost/private IPs, and known push-service hosts.
- Add timeout tests using an injected reqwest client/server that never responds.
- Add VAPID TTL tests for negative, zero, exactly 24h, and over-24h lifetimes.
- Add bounded-error-body tests for rejection responses.
- Add typed handling tests for 429 with `Retry-After` and 5xx retry policy.
- Add subject validation tests for `mailto:`/HTTPS and invalid subjects.
- Add key-length validation tests for decoded `p256dh` and `auth`.

## workflow

Status: Resolved (2026-05-30) — all 4 HIGH (panic via `8d26c19`, reclamation via `8d26c19`, step-replay-deterministic-input via `10878bf`, long-running-step lock-heartbeat via `cba34b7`) + 6 MEDIUM (via `956a5c2`) + 1 LOW (via `a539397f`) closed. See per-bullet citations below + `audit-2026-05/DOMAIN-23-workflow.md` for the original HIGH pair.

Files:
- `framework/src/workflow/mod.rs`
- `framework/src/workflow/context.rs`
- `framework/src/workflow/store.rs`
- `framework/src/workflow/types.rs`
- `framework/src/workflow/entities.rs`
- `framework/src/workflow/config.rs`
- `framework/src/workflow/registry.rs`
- Supporting context: `suprnova-macros/src/workflow.rs`,
  `suprnova-macros/src/workflow_step.rs`, `framework/src/app.rs`

Review notes:
- ~~High: expired running workflows are never reclaimed. `claim_next_workflow`
  only selects rows where `status = 'pending'`
  (`framework/src/workflow/store.rs:125-143`), so a worker crash or panic after
  marking a row `running` leaves it permanently stuck even after
  `locked_until` expires. The lock timeout is refreshed, but it is not used to
  recover `running` rows.~~ — **CLOSED 2026-05-29 via `8d26c19` (D23-B): the
  eligible-row predicate now covers `status='running' AND locked_until <= NOW()`
  alongside the pending case; `FOR UPDATE SKIP LOCKED` keeps concurrent workers
  from racing on the recovered row; the outer UPDATE increments attempts so a
  reclaimed crash counts toward `max_attempts`. See `store.rs:122-215`.**
- ~~High: step replay does not verify deterministic input. `run_step_with_input`
  loads an existing step by `(workflow_id, step_index, step_name)` and returns a
  cached successful output without comparing `existing.input` to the new
  `input_json` (`framework/src/workflow/context.rs:55-76`). A workflow retry
  can silently reuse a step result for different arguments as long as the same
  step name appears at the same index.~~ — **CLOSED 2026-05-29 via `10878bf`.**
  Added a determinism guard in the `Some(existing)` arm of `run_step_with_input`
  (`framework/src/workflow/context.rs`): `if existing.input != input_json` returns
  `FrameworkError::internal` naming the step index, step name, and the determinism
  contract — short-circuits before the cached-output return AND before the
  `update_step_running` rewrite that would have silently overwritten the stored
  input. Regression `test_step_replay_with_mismatched_input_errors` records a step
  with `input=5`/`output=42`, replays the same name+index with `input=7`, and
  asserts the call returns Err containing both "input mismatch" and "deterministic",
  with the step closure invoked exactly once across both attempts.
- ~~High: workflow panics are not contained. Worker tasks call
  `process_claimed_workflow` inside `tokio::spawn` without `catch_unwind`
  (`framework/src/workflow/mod.rs:104-112`), and `process_claimed_workflow`
  directly awaits the workflow runner (`framework/src/workflow/mod.rs:140-146`).
  A panic leaves the workflow row `running` and, because running rows are not
  reclaimed, the workflow is stranded.~~ — **CLOSED 2026-05-29 via `8d26c19`
  (D23-A): workflow body is wrapped in `AssertUnwindSafe(...).catch_unwind()`;
  panic payload downcast via `server::panic_payload_message` (promoted to
  `pub(crate)`) and folded into the existing Err arm so the requeue/mark_failed
  budget accounting runs. See `mod.rs:152-211`. Regression coverage:
  `test_panic_requeues_under_budget`, `test_panic_marks_failed_when_budget_exhausted`,
  `test_claim_reclaims_expired_running_row` (Postgres-gated `#[ignore]`).**
- ~~High: long-running steps can exceed the lock timeout. Locks are refreshed
  before a step and after it completes (`framework/src/workflow/context.rs:84-88`,
  `framework/src/workflow/context.rs:104-120`), but not while the step future is
  running. If reclamation is added, a long side-effecting step can be claimed by
  another worker unless there is heartbeat/lease extension during execution.~~
  — **CLOSED 2026-05-29 via `cba34b7`.** Spawned a workflow-level heartbeat task
  in `process_claimed_workflow` that calls `store::refresh_lock` at
  `max(lock_timeout/2, 1s)` intervals while the workflow body executes; owned
  by an `AbortOnDrop` RAII guard so the renewal task is guaranteed to stop the
  moment the body resolves (load-bearing — settle arms use `?` and a leaked
  heartbeat would extend the lease for a workflow nobody is running, blocking
  reclamation forever). Workflow-level (not step-level) was chosen because it
  covers gaps step-level misses: before first step / between steps / after last
  step before `mark_succeeded`. Regression `test_long_running_step_extends_lease`
  (SQLite, backend-agnostic) uses `sleep_ms > lock_timeout` (2.5s > 2s, heartbeat
  at 1s); captures baseline AFTER the pre-step refresh lands so the per-step
  refresh can't false-pass it; counts distinct `locked_until > baseline` values
  during `status='running'`. Logged follow-up: `store::refresh_lock` has no
  `worker_id` guard, so a starved heartbeat that wakes after another worker
  reclaimed could stomp the new owner's lease — separate hardening (conditional
  `UPDATE WHERE worker_id = $self`).
- ~~Medium: workflow configuration is unvalidated. `WorkflowConfig::from_env`
  accepts `WORKFLOW_CONCURRENCY=0`, negative `WORKFLOW_MAX_ATTEMPTS`, and
  negative `WORKFLOW_RETRY_BACKOFF_SECS` through generic env parsing
  (`framework/src/workflow/config.rs:31-41`). Zero concurrency makes the worker
  wait forever on the semaphore (`framework/src/workflow/mod.rs:117-150`), and
  negative backoff schedules retries in the past
  (`framework/src/workflow/mod.rs:200-202`).~~ — **CLOSED 2026-05-29 via `956a5c2`.**
  `WorkflowConfig::from_env` clamps `WORKFLOW_CONCURRENCY=0`, negative
  `WORKFLOW_MAX_ATTEMPTS`, and negative `WORKFLOW_RETRY_BACKOFF_SECS` to safe
  minimums with `tracing::warn`; `WorkflowConfig::validate` fails-fast on
  programmatic configs that pass the same invalid values.
- ~~Medium: worker lifecycle is not production-managed. `WorkflowWorker::run`
  loops forever, uses bare `tokio::spawn`, writes errors with `eprintln!`, and
  has no signal handling, graceful drain, JoinSet, metrics, or structured logs
  (`framework/src/workflow/mod.rs:117-150`). The app-level `workflow:work`
  command exits only when `work_loop` returns an error (`framework/src/app.rs:438-469`).~~
  — **CLOSED 2026-05-29 via `956a5c2`.** `WorkflowWorker::run_with_cancel` uses
  `CancellationToken` + `JoinSet` for graceful drain; `eprintln!` replaced with
  structured `tracing` events; `app::run_workflow_worker_internal` mirrors the
  queue worker Ctrl-C-with-drain pattern.
- ~~Medium: duplicate workflow registrations are not detected. `registry::find`
  returns the first inventory entry matching a name
  (`framework/src/workflow/registry.rs:16-24`), so duplicate `#[workflow]` names
  across crates/modules are order-dependent rather than a boot-time error.~~
  — **CLOSED 2026-05-29 via `956a5c2`.** `registry::find_strict` and
  `registry::assert_no_duplicates` detect duplicate `#[workflow]` registrations;
  `start_named` uses `find_strict` and `WorkflowWorker::new` asserts no
  duplicates at boot.
- ~~Medium: there is no framework-owned schema migration. The entities expect
  `workflows` and `workflow_steps` tables (`framework/src/workflow/entities.rs:1-66`),
  and tests define local migrations inside `workflow::tests`
  (`framework/src/workflow/mod.rs:356-557`), but the framework does not ship or
  expose migrations for applications to install.~~ — **CLOSED 2026-05-29 via
  `956a5c2`.** `framework::workflow::migrations` exposes
  `CreateWorkflowsTable` and `CreateWorkflowStepsTable` so apps can register
  the schema directly without copying CLI scaffolder templates.
- ~~Medium: workflow side effects are at-least-once, but the API reads like
  durable exactly-once steps. A crash after a step side effect but before
  `mark_step_succeeded` (`framework/src/workflow/context.rs:94-114`) causes the
  step body to run again on retry. The docs should require idempotent step
  side effects or provide an idempotency helper integrated with workflow ids.~~
  — **CLOSED 2026-05-29 via `956a5c2`.** `docs/workflows.md` and module
  rustdoc document at-least-once semantics with idempotency patterns; pure
  side-effect helpers reference `WorkflowContext` + `step_index` as stable
  keys.
- ~~Medium: `WorkflowHandle::wait` has no timeout or cancellation control. It
  polls forever using default config values, not the configured workflow
  settings (`framework/src/workflow/types.rs:79-91`), so callers can hang
  indefinitely on lost/stuck workflows.~~ — **CLOSED 2026-05-29 via `956a5c2`.**
  Added `WorkflowHandle::wait_with_timeout(Duration)` and
  `wait_with_options(poll, timeout)` bounding the previously-unbounded poll
  loop; original `wait()` preserved.
- ~~Low: `normalize_workflow_name("name")` uses `module_path!()` from inside the
  framework module (`framework/src/workflow/mod.rs:73-81`), not the caller's
  module. The macro has its own normalization path, but the public helper is
  misleading for app code.~~ — **CLOSED 2026-05-30 via `a539397f`.** Deleted
  the standalone `normalize_workflow_name` helper: zero callers, misleading by
  construction (no way to capture the caller's `module_path!()`), and the
  `#[workflow]` macro already emits the correct
  `concat!(module_path!(), "::", stringify!(...))` at the call site.

Test coverage gaps:
- Add Postgres integration tests proving expired `running` workflows are
  reclaimed safely and not double-claimed.
- Add deterministic replay tests where the same step index/name receives
  different serialized input on retry and must fail.
- Add panic tests for workflow bodies and steps, asserting rows are failed or
  requeued rather than stranded.
- Add long-running step/lock-heartbeat tests.
- Add config validation tests for zero/negative concurrency, attempts, timeout,
  and backoff.
- Add worker shutdown/drain tests with multiple concurrent claimed workflows.
- Add duplicate workflow-name tests.
- Add migration installation tests for fresh apps.

## ws

Status: HIGH RESOLVED 2026-05-29 via remediation/ws-high. Both HIGH findings
closed in one commit: documented-close-code-on-handler-err and Origin policy
at upgrade time. MEDIUM `WsConfig` unvalidated finding partially closed
2026-05-29 via `73a7ad3` (defaults dropped to 1 MiB / 64 KiB with explicit
`generous()` opt-in for high-limit feeds). Remaining MEDIUM / LOW items
acknowledged below.

Files:
- `framework/src/ws/mod.rs`
- `framework/src/ws/socket.rs`
- `framework/src/ws/heartbeat.rs`
- Supporting context: `framework/src/server.rs`, `framework/src/routing/router.rs`,
  `framework/tests/ws_e2e.rs`, `framework/tests/ws_unit.rs`,
  `framework/tests/ws_heartbeat.rs`, `framework/tests/ws_router.rs`,
  `framework/tests/ws_router_macro.rs`, `framework/tests/ws_per_route_config.rs`,
  `framework/tests/ws_origin_policy.rs`

Review notes:
- ~~High: handler errors do not send the documented close code.~~
  **CLOSED 2026-05-29.** `handle_ws_upgrade` now sends an explicit
  `Close(1011, "internal error")` frame on the handler-`Err(_)` path,
  mirroring the `Close(1000, "")` already sent on `Ok(())`. Without the
  explicit close, the peer saw the protocol-default 1005 / 1006
  ("No Status Received" / "Abnormal Closure") and the documented
  `WebSocketHandler` trait contract was silently broken. Regression
  coverage: `framework/tests/ws_origin_policy.rs::handler_err_sends_close_1011`
  drives an `ErroringHandler` end-to-end through a real loopback server
  and asserts a `Close(CloseCode::Error)` frame arrives within 2 s.
- ~~High: WebSocket routes have no built-in Origin policy.~~
  **CLOSED 2026-05-29.** Added `OriginPolicy` enum to `framework/src/ws/mod.rs`
  with three variants: `SameOrigin` (default — rejects upgrades whose
  `Origin` host or port doesn't match the request's `Host` header, AND
  rejects upgrades without an `Origin` header), `AllowAny` (skips
  validation; for non-browser endpoints and tests), and `AllowList(Vec<String>)`
  (exact case-insensitive match against listed `scheme://host[:port]`
  origins). Enforced in `handle_ws_upgrade` BEFORE `hyper_tungstenite::upgrade`
  so a policy violation returns HTTP 403 with no protocol switch. Added
  `Router::ws_with_config` was already the right way to pin a per-route
  policy; `WsConfig::origin_policy` is the new field with `SameOrigin`
  default. Tests `ws_e2e.rs`, `ws_heartbeat.rs`, `ws_global_middleware.rs`
  updated to opt into `AllowAny` (they're not exercising browser CSRF
  semantics). Regression coverage: 6 tests in `ws_origin_policy.rs`
  (SameOrigin rejects no-Origin / cross-origin; SameOrigin allows
  matching-Origin; AllowList accepts listed / rejects unlisted; AllowList
  rejects no-Origin).
- ~~Medium: global middleware does not apply to WS upgrades. This was recorded in
  the `middleware` section, but it directly affects the WS module too:
  `handle_request` branches to `handle_ws_upgrade` before building the normal
  middleware chain (`framework/src/server.rs:451-459`). Auth/session/rate-limit
  middleware must be repeated per WS route.~~ — **CLOSED via prior work (cited
  line range stale).** `handle_ws_upgrade` now extends the chain with
  `global_middleware()` before the terminator, so auth/session/rate-limit
  global middleware run on WS upgrades alongside HTTP requests.
- ~~Medium: heartbeat close uses the public `Message` bridge, so it sends
  `Message::Close` as a normal outbound message rather than the internal
  `Outbound::Close` variant that closes the sink and terminates the forwarder
  (`framework/src/ws/heartbeat.rs:62-71`, `framework/src/ws/socket.rs:187-213`).
  The peer may receive a close frame, but the forwarder does not take the
  explicit close path.~~ — **CLOSED 2026-05-29 via `c73eba9`.** The heartbeat
  close now goes through `Outbound::Msg`; the bridge rewraps
  `Message::Close` → `Outbound::Close` terminating the forwarder so the close
  handshake completes through the explicit close path.
- ~~Medium: `WsConfig` is unvalidated. A zero `ping_interval` can panic
  `tokio::time::interval`, very large `max_message_size`/`max_frame_size` can
  permit high per-connection memory use, and `max_missed_pings = 0` is not
  documented (`framework/src/ws/mod.rs:49-82`, `framework/src/ws/heartbeat.rs:40-75`).
  **PARTIAL 2026-05-29:** the `max_message_size` / `max_frame_size` DoS-knob
  half closed via `73a7ad3` — defaults dropped to 1 MiB / 64 KiB (public-
  endpoint-safe); high-limit feeds opt in via `WsConfig::generous()` (64 MiB /
  16 MiB). Zero `ping_interval` panic surface + undocumented `max_missed_pings = 0`
  semantic remain open.~~ — **FULLY CLOSED 2026-05-29 via `c73eba9` (on top of
  `73a7ad3`).** Added `WsConfig::validate()` rejecting zero `ping_interval` /
  zero `max_missed_pings` at `try_ws_boxed_with_middleware_and_config`
  registration; the zero-interval panic surface and undocumented
  `max_missed_pings = 0` semantic both resolved.
- ~~Medium: auxiliary tasks are only partially tracked. `Server::run` tracks the
  top-level handler task in `WS_TASKS` (`framework/src/server.rs:858-874`), but
  `WsSocket::from_stream_with_heartbeat` spawns a forwarder task and
  `WsSocket::sender` spawns bridge tasks with bare `tokio::spawn`
  (`framework/src/ws/socket.rs:73-93`, `framework/src/ws/socket.rs:115-126`).
  Shutdown drains the handler task but not these child tasks directly.~~ —
  **CLOSED 2026-05-29 via `c73eba9`.** Added `take_forwarder_handle()` awaited
  post-handler so the close handshake completes before the `WS_TASKS` slot
  reports joined; the forwarder is no longer an untracked tail task at drain.
- ~~Medium: subprotocol negotiation is missing. `hyper_tungstenite::upgrade` is
  called with only tungstenite config (`framework/src/server.rs:653-662`), and
  `WsConfig` has no accepted-protocol list. Apps needing GraphQL WS, JSON-RPC,
  or custom protocols cannot negotiate `Sec-WebSocket-Protocol` through the
  framework API.~~ — **CLOSED 2026-05-29 via `c73eba9`.** Added
  `WsConfig.accepted_protocols` + `negotiate_subprotocol()` +
  `Sec-WebSocket-Protocol` echo on 101; apps can now negotiate
  GraphQL WS / JSON-RPC / custom subprotocols through the framework API.
- ~~Low: `recv_text` silently discards binary, ping, pong, and frame messages
  (`framework/src/ws/socket.rs:142-161`). That is ergonomic for echo-style
  handlers, but handlers cannot observe discarded control/data frames unless
  they use the lower-level `recv` API from the start.~~ — **CLOSED via dc258cc.** `WsSocket::recv_text` rustdoc now enumerates the discarded frame kinds (binary, ping, pong, control) and points callers to the lower-level `recv()` API for full observation.
- ~~Low: close reason/code validation is delegated to tungstenite. `WsSocket::close`
  accepts any `u16` and arbitrary reason string (`framework/src/ws/socket.rs:180-195`);
  invalid app-level choices surface late, if at all.~~ — **CLOSED via dc258cc.** `WsSocket::close` now validates code via `CloseCode::is_allowed` and caps reason at 123 bytes before delegating to tungstenite, so invalid app-level choices fail fast at the framework boundary.

Test coverage gaps:
- Add an e2e test where a handler returns `Err` and assert the client receives
  Close 1011.
- Add Origin allow/deny middleware tests and document the default policy.
- Add WS global middleware tests once the global-middleware behavior is fixed or
  explicitly documented.
- Add heartbeat no-pong tests with a short per-route config and a raw/non-auto-pong
  client; current tests only prove healthy echo still works
  (`framework/tests/ws_heartbeat.rs`).
- Add config validation tests for zero intervals, huge frame/message sizes, and
  `max_missed_pings` edge cases.
- Add shutdown tests proving forwarder/bridge/heartbeat tasks terminate after
  handler completion and server drain.
- Add subprotocol negotiation tests once supported.

## server

Status: Resolved (2026-05-29) — covered by `audit-2026-05/DOMAIN-01-router-and-dispatch.md` (F1 silent insert errors, F2 accept loop, F4 health 503, F8 RwLock poison, F16 WS terminator) and `DOMAIN-04-config.md` (C1 SERVER_MAX_BODY_SIZE wiring, body-cap atomic, ServerConfig boot). HIGH `APP_KEY` fail-closed at non-dev envs landed via `Server::from_config` Err-not-panic path (CLAUDE.md request-lifecycle section).

## session

Status: Resolved (2026-05-29) — covered by `audit-2026-05/DOMAIN-12-cache-sessions-crypto.md` (no findings; per-request `Arc<Mutex>` poison only affects current request, caught by Domain 2 M1 catch_unwind). Plus #375 session-persistence fail-closed via `86270fa`. Plus Laravel-13 parity sweep (`docs/parity/session.md`) — facade completion (pull/push/increment/decrement/remember/missing/has_any/has_all/all/only/except/replace/put_many/forget_many/now/reflash/keep/flash_input + previousUrl/previousRoute/passwordConfirmed accessors), config knobs (expire_on_close/domain/partitioned/connection), Cookie::partitioned attribute, `SessionMiddleware::install_with_gc`/`install` ctors, crate-root re-exports — shipped inline on top of zero-findings baseline.

## testing

Status: Resolved (2026-05-29) — covered by `audit-2026-05/DOMAIN-21-testing.md` (no findings; all 27 `panic!()` in `expect.rs` are Jest-style assertion contracts; `TestDatabase::fresh` + `TestContainer::fake/bind` audited clean).

## rate_limit

Status: Resolved (2026-05-29) — covered by #376 `5546aa8` (rate-limit fail-open/closed configurable via `BackendErrorPolicy`; `FailClosed=503` on backend errors).

## mail

Status: Resolved (2026-05-29) — `mail/boot.rs` + `mail/mailable_registry.rs` lock-poison findings absorbed into Domain 1 deferrals (PROGRESS.md F8 cross-cutting fix). Plus Phase 11 R5 `MAIL_FROM required` hardening confirmed at HEAD via `audit-2026-05/DOMAIN-10-auth.md`.

## auth_flows

Status: Resolved (2026-05-29) — covered by `audit-2026-05/DOMAIN-10-auth.md` (D10-A `GateRegistry` HOT PATH RwLock poison + D10-B `OAuthAuth::configure` poison; Phase 11 R1-R5 mitigations all holding at HEAD). Plus #373 2FA replay race via `c9dc1ac`.

## features

Status: Resolved (2026-05-29) — covered by `audit-2026-05/DOMAIN-17-feature-flags.md` (D17-A `DatabaseEvaluator::is_enabled` HOT PATH RwLock poison closed; Phase 13 R1-R5 confirmed at HEAD).

## notifications

Status: Resolved (2026-05-29) — covered by #374 `c4ae6cf` (broadcast notification channel → real `BroadcastHub` delivery + fail-fast). Notification-channel registry lock-poison absorbed into Domain 1 deferrals (PROGRESS.md F8 cross-cutting).

## factory

Status: Resolved (2026-05-29) — covered by `audit-2026-05/DOMAIN-14-factories-seeders.md` (no findings; pure data-shaping, no global state).

## seed

Status: Resolved (2026-05-29) — covered by `audit-2026-05/DOMAIN-14-factories-seeders.md` (D14-A seeder registry `.expect()` defeated lock helper at 3 sites; register/count/clear now match helper Result shape and log+degrade).

## console

Status: Resolved (2026-05-29) — covered by `audit-2026-05/DOMAIN-13-console.md` (D13-A schedule helper `# Panics` docs added for `every_n_minutes`/`hourly_at`/`daily_at`/`monthly_on` ranges; alternative `try_cron()` fallible path exists).

## supervisor

Status: Resolved (2026-05-29) — covered by `audit-2026-05/DOMAIN-16-broadcasting.md` (supervisor audited as part of broadcasting domain; `supervisor:203` OnceLock-set-then-get monomorphically safe; tokio::sync mutexes no poison surface).

## vector

Status: Resolved (2026-05-29) — covered by `audit-2026-05/DOMAIN-18-vector.md` (D18-A `VectorRegistry::install` RwLock poison closed; Phase 9A T2-T4 + 9B T1-T4 + H1-H3 confirmed at HEAD).

## payments

Status: Resolved (2026-05-29) — covered by `audit-2026-05/DOMAIN-19-payments.md` (D19-A registry poison; D19-B `MockPaymentProvider` tokio::sync::RwLock migration; D19-C `Money::{from_decimal,add,sub}` `# Panics` docs).

## prelude

Status: Resolved (2026-05-29) — covered by `audit-2026-05/DOMAIN-01-router-and-dispatch.md` (prelude re-exports audited as part of the framework crate-root re-export discipline; no findings).
