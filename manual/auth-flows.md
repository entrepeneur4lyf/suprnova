# Auth Flows

`suprnova::auth_flows` is the lifecycle layer on top of [session
authentication](authentication.md). Where `auth::*` answers "who is this
request", `auth_flows::*` answers everything around that question — proving
the email address is real, recovering it when the password is lost,
defending it against credential stuffing, and protecting it with a second
factor. Five flows ship under one namespace:

- `EmailVerification` — generate, check, and consume torii-backed
  verification tokens; `send_link` dispatches the verification mail
  through the [`Mail`](mail.md) facade.
- `PasswordReset` — anti-enumeration `request`, `verify_token`,
  `complete`, plus `send_link`. `complete` rotates the password,
  revokes every session and remember-me row for the user, and sends a
  `PasswordChangedMail` security notification.
- `BruteForce` + `LoginThrottleMiddleware` — torii-backed lockout
  state plus an HTTP middleware that short-circuits with `429 Too
  Many Requests` before the login handler is invoked.
- `TwoFactor` — TOTP enrollment, confirmation, verification, recovery
  codes, secret rotation, the full challenge flow that gates a
  password login on the second factor, and replay protection at the
  30-second timestep granularity.
- `remember_me` — re-export of `crate::auth::remember` (DB-row +
  bcrypt + single-use rotation persistent cookies) for namespace
  cohesion.

Two route-gate middleware ship in the same namespace:

- `EnsureEmailVerifiedMiddleware` — composes after `AuthMiddleware`
  to gate routes on `email_verified_at`.
- `TwoFactorChallengeMiddleware` — composes in front of
  `AuthMiddleware` to bounce a session with a pending 2FA challenge
  to the challenge form rather than the login page.

Every transactional message is delivered through the [`Mail`](mail.md)
facade. Torii's optional `mailer` feature is intentionally disabled in
`framework/Cargo.toml`: running a second mail stack inside torii would
split telemetry, double the transport configuration surface, and force
apps to wire two "from" addresses.

### Why Suprnova diverges

Torii owns the durable security state — verification tokens, reset
tokens, the per-account lockout counter, and the session pool. Suprnova
owns the cross-cutting concerns — outbound mail, event dispatch, the
2FA TOTP table, remember-me cookies, and the HTTP middleware. The two
halves meet at the facades in this module, so application code only
touches `suprnova::auth_flows::*`. Laravel folds equivalent surface into
Fortify; we keep the split because the storage half is upstream
infrastructure and the lifecycle half is framework idiom — keeping them
in separate crates lets each evolve on its own cadence.

## Failure semantics across flows

Every facade follows one ordering rule: the durable state change
commits first, then notification side effects fire. A listener panic, a
transient mail-transport failure, or a dispatcher error after the
mutation cannot roll the mutation back.

- `EmailVerification::verify` stamps `email_verified_at` before firing
  `EmailVerified`.
- `PasswordReset::complete` rotates the password inside torii's
  transaction first, then revokes every session and remember-me row for
  the user (logged on failure, not surfaced), then dispatches
  `PasswordChangedMail` fire-and-forget, then fires
  `PasswordResetCompleted`.
- `BruteForce::unlock_account` commits the unlock before firing
  `AccountUnlocked`.
- `TwoFactor::confirm` stamps `confirmed_at` before firing
  `TwoFactorEnrolled`; `TwoFactor::disable` deletes the row before
  firing `TwoFactorDisabled`; `TwoFactor::complete_challenge`
  promotes pending → authed before dispatching the standard
  `auth::Login` + `auth::Authenticated` pair followed by
  `TwoFactorChallenged`.

A listener that needs durability should buffer its work (queue a
job from the listener body); the facade itself never retries.

## Bootstrapping

Two pieces of bootstrap are required before any flow works:

1. Initialise torii — call `init_torii(ToriiConfig::from_sea_orm(conn))`
   in `bootstrap.rs::register()`, after `DB::init`.
2. Register the framework-owned 2FA migrations — list **both**
   `two_factor::migration::Migration` and
   `two_factor::migration_replay::Migration` in your app's
   `Migrator::migrations()` so `suprnova migrate` provisions
   `two_factor_credentials` and the replay-protection column.

### Wiring torii

```rust
use suprnova::torii_integration::{init_torii, ToriiConfig};
use suprnova::DB;

pub async fn register() -> Result<(), suprnova::FrameworkError> {
    DB::init().await?;

    let conn = DB::connection()?.inner().clone();
    init_torii(ToriiConfig::from_sea_orm(conn)).await?;

    Ok(())
}
```

`init_torii` is idempotent. The `OnceLock` guard means the second call
is a no-op, so test harnesses that re-enter `register()` per fixture
do not double-migrate. For tests, swap in
`ToriiConfig::sqlite_in_memory()` — it spins up a shared-cache
in-memory database that survives across runtimes:

```rust
let config = ToriiConfig::sqlite_in_memory()
    .await?
    .apply_migrations(true);
init_torii(config).await?;
```

### Registering the 2FA migrations

The framework ships the schema; your app opts in by listing both
migrations in its own migrator:

```rust
use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            // ... your own migrations ...

            // Creates `two_factor_credentials`.
            Box::new(suprnova::auth_flows::two_factor::migration::Migration),
            // Adds `last_used_timestep` for TOTP replay protection.
            Box::new(suprnova::auth_flows::two_factor::migration_replay::Migration),
        ]
    }
}
```

Both are idempotent against an already-applied database (the v1 uses
`CREATE TABLE IF NOT EXISTS`; the v2 is a column add). Re-running
`suprnova migrate` against a production database that already has the
schema is a no-op.

### Environment

The transactional mailables read two environment variables at send
time:

| Var | Default | Used for |
|---|---|---|
| `APP_NAME` | `"Suprnova"` | Subject branding and the `otpauth://` issuer label that authenticator apps display. |
| `MAIL_FROM` | none — **errors when unset** | Envelope `From` on every outgoing message. Set to a verified sender domain. |

`MAIL_FROM` deliberately has no default. Defaulting to a placeholder
like `noreply@example.com` would silently break DMARC / SPF in
production and ship from a domain the operator doesn't control, so the
facade fails closed instead. `EmailVerification::send_link` and
`PasswordReset::send_link` surface the error as `Err`;
`PasswordReset::complete` logs via `tracing::warn!` and continues
(the password change has already committed, so the notification path
cannot roll it back).

Apps additionally set `APP_URL` so controllers can derive the base URL
used in `send_link` calls; the framework facade itself takes the base
URL as a parameter.

The mail driver is configured separately via `MAIL_DRIVER` — see the
[Mail](mail.md) docs.

## Email Verification

`EmailVerification` is a facade over torii's verification service. Four
operations cover the lifecycle:

```rust
use suprnova::auth_flows::EmailVerification;

// Mint a token for a user.
let token = EmailVerification::generate_token(&user.id).await?;
let token_str = token
    .token()
    .expect("plaintext on a freshly-issued token")
    .to_string();

// Optional landing-page check — non-consuming, so a page refresh
// does not burn the token.
let valid: bool = EmailVerification::check(&token_str).await?;

// The click-through handler consumes the token and stamps the user.
let verified_user = EmailVerification::verify(&token_str).await?;
```

`verify` fires `EmailVerified` on success — listeners are the right
place to unlock additional functionality (welcome email, default
follows, "complete your profile" CTA) without coupling them to the
verification handler.

### End-to-end with `send_link`

Most callers do not mint the token themselves — `send_link` mints it
and dispatches the verification email:

```rust
use std::collections::HashMap;
use suprnova::auth_flows::EmailVerification;
use suprnova::torii_integration::find_user_by_email_lookup_only;
use suprnova::{FrameworkError, HttpResponse, Request, Response};

pub async fn resend(req: Request) -> Response {
    resend_inner(req).await.map_err(HttpResponse::from)
}

async fn resend_inner(req: Request) -> Result<HttpResponse, FrameworkError> {
    let raw = req.query().unwrap_or("");
    let params: HashMap<String, String> =
        url::form_urlencoded::parse(raw.as_bytes()).into_owned().collect();
    let email = params
        .get("email")
        .ok_or_else(|| FrameworkError::bad_request("missing email"))?;

    // Anti-enumeration: only dispatch when the user exists; respond
    // identically in both branches. `lookup_only` never creates a row,
    // so a probing caller cannot mint accounts here either.
    if let Some(user) = find_user_by_email_lookup_only(email).await? {
        let base = format!(
            "{}/auth/verify",
            std::env::var("APP_URL").unwrap_or_else(|_| "http://localhost:8000".into()),
        );
        EmailVerification::send_link(&user, &base).await?;
    }

    Ok(HttpResponse::text(
        "If this email is on file, a verification link has been sent.",
    ))
}
```

`send_link` builds the URL as `{base_url}?token={plaintext_token}`. A
trailing slash on `base_url` is trimmed before the query string is
appended, so `https://app.example.com/verify/` and
`https://app.example.com/verify` both produce a clean URL.

The click-through handler pulls the token from the query string and
calls `verify`:

```rust
async fn verify_inner(req: Request) -> Result<HttpResponse, FrameworkError> {
    let raw = req.query().unwrap_or("");
    let params: HashMap<String, String> =
        url::form_urlencoded::parse(raw.as_bytes()).into_owned().collect();
    let token = params
        .get("token")
        .ok_or_else(|| FrameworkError::bad_request("missing token"))?;

    EmailVerification::verify(token).await?;

    Ok(HttpResponse::new().status(302).header("Location", "/"))
}
```

The handler does not need to look up the user — `verify` returns the
freshly-stamped `User`, the `EmailVerified` event carries the user id,
and the next request is signed in as usual through session middleware.

### Verified-only routes: `EnsureEmailVerifiedMiddleware`

`EnsureEmailVerifiedMiddleware` gates routes on the authenticated
user's `email_verified_at`. Compose it after `AuthMiddleware` and the
chain blocks any request whose user has not yet completed the verify
step.

The choice between **403 JSON** and **302 HTML redirect** is made at
route-registration time via the constructor — there is no
request-content sniffing, matching the pattern set by
`AuthMiddleware::new` / `AuthMiddleware::redirect_to`:

```rust
use suprnova::{AuthMiddleware, EnsureEmailVerifiedMiddleware, group, get};

// API surface — 403 with a JSON body.
group!("/api")
    .middleware(AuthMiddleware::new())
    .middleware(EnsureEmailVerifiedMiddleware::new())
    .routes([
        get!("/me", profile::show),
    ]);

// Web surface — 302 (or 409 + X-Inertia-Location for Inertia visits).
group!("/dashboard")
    .middleware(AuthMiddleware::redirect_to("/login"))
    .middleware(EnsureEmailVerifiedMiddleware::redirect_to("/email/verify"))
    .routes([
        get!("/", dashboard::index),
    ]);
```

If no user is authenticated, the middleware falls into the same response
branch as "authed but not verified" — matching Laravel's
`! $request->user() || ! hasVerifiedEmail()` shape. Compose
`AuthMiddleware` first when you want a separate `401` for unauthed
requests.

For in-handler branching (e.g. conditionally rendering a "please
verify" CTA without redirecting), read the flag from the torii `User`:

```rust
if let Some(user_id) = suprnova::Auth::id()
    && let Some(user) = suprnova::torii_integration::find_user_by_id(&user_id).await?
{
    let verified: bool = user.is_email_verified();
    // branch on it
}
```

## Password Reset

`PasswordReset` mirrors the same shape — `request`, `verify_token`,
`complete`, plus the `send_link` convenience:

```rust
use suprnova::auth_flows::PasswordReset;

// Mint a reset token. Ok(None) when the email is unknown.
let Some((user, token)) = PasswordReset::request(&email).await? else {
    return Ok(generic_response);
};

// Optional landing-page check.
let valid: bool = PasswordReset::verify_token(&token).await?;

// The click-through handler: consume the token + apply the new password.
let user = PasswordReset::complete(&token, &new_password).await?;
```

### Anti-enumeration

The module is structured so the response shape never leaks whether an
email address has an account:

- `request` returns `Ok(None)` when the email is not on file — same
  return type, same shape, no `Err`.
- `send_link` always returns `Ok(())`. When the email is absent no
  mail is dispatched, but the absence is not surfaced through the
  return type either. Callers that need to distinguish (e.g. for
  internal accounting) should call `request` directly and watch the
  `PasswordResetLinkSent` event.
- The dogfood controller pairs `send_link` with a fixed 200 response
  body, so a probing caller cannot distinguish through status code,
  response body, or response timing.

### `complete` side effects

`complete` runs four steps in order:

1. Rotate the password hash inside torii's transaction (the only step
   that can fail the call).
2. Revoke every session row for the user via
   `crate::session::destroy_all_for_user` (best-effort: failures
   `tracing::warn!`).
3. Revoke every remember-me row via `crate::auth::remember::revoke_all_for_user`
   (best-effort).
4. Dispatch `PasswordChangedMail` fire-and-forget, then fire
   `PasswordResetCompleted`.

A stolen session and a captured remember-me cookie must not outlive
the credential they depended on. The revocations happen on every
successful reset, not just on user-initiated ones, so a security-team
forced reset also kicks out an active attacker.

## Brute-Force Protection

The brute-force layer has two parts: the `BruteForce` facade that
records and queries lockout state, and the `LoginThrottleMiddleware`
that short-circuits at the HTTP layer before the handler is invoked.

### The `BruteForce` facade

Call `record_failed_attempt` from the failed-auth branch of your login
handler, and `reset_attempts` from the success branch:

```rust
use suprnova::auth_flows::BruteForce;

// In the failed-auth path:
let status = BruteForce::record_failed_attempt(&email, Some(&peer_ip)).await?;
if status.is_locked {
    // Optionally surface a custom response. The middleware will do
    // this for you on the *next* request — see below.
}

// In the success path:
BruteForce::reset_attempts(&email).await?;
```

`record_failed_attempt` returns the updated `LockoutStatus`
(`is_locked`, `failed_attempts`, and `locked_until` when locked). Pass
the optional `ip` for audit logs; pass `None` if your transport doesn't
surface a client IP cleanly.

Two additional operations:

```rust
// Read-only — safe on emails with no history.
let status = BruteForce::get_lockout_status(&email).await?;
let locked: bool = BruteForce::is_locked(&email).await?;

// Admin / forced unlock. Fires `AccountUnlocked` only on a real state
// transition (no-op unlock on an already-unlocked account does not fire).
let was_locked: bool = BruteForce::unlock_account(&email).await?;
```

`unlock_account` returns `true` when the account had been locked at the
time of the call, `false` otherwise. The `AccountUnlocked` event fires
only on `true` — a `false` return is the no-op it is, not an audit
event.

### `LoginThrottleMiddleware`

The middleware reads the lockout state for whichever email a request is
targeting and short-circuits with `429 Too Many Requests` when the
account is locked. The login handler is never invoked, so a locked
account does not even get to attempt a credentials check:

```rust
use suprnova::auth_flows::LoginThrottleMiddleware;
use suprnova::Router;

// The email extractor is a sync closure over `&Request`. Reading
// JSON/form body is async and consumes `Request`, so the closure
// cannot read the body — pull from a header, query string, or
// route param instead.
let throttle = LoginThrottleMiddleware::new(|req| {
    req.header("X-Login-Email").map(str::to_string)
});

let router = Router::new()
    .post("/login", login_handler)
    .middleware(throttle);
```

Practical extraction surfaces:

- A header (`X-Login-Email`), set by a preceding pre-processor — the
  pattern used in the dogfood app.
- A query string parameter (`?email=…`).
- A route parameter (`/login/{email}`).

Returning `None` from the extractor is the explicit "I have nothing to
check" signal — the middleware passes the request through unchanged.
This makes the middleware safe to install on routes that occasionally
see anonymous traffic (e.g. the same `POST /login` endpoint that also
handles a no-email "request password reset" sub-action).

On lock the middleware returns:

- Status `429 Too Many Requests`.
- `Retry-After` header — seconds, computed from the lockout's
  `locked_until` via `LockoutStatus::retry_after_seconds`. Falls back
  to `900` (15 minutes — torii's default lockout period) if the
  timestamp is somehow absent.
- Body: `"Account locked due to too many failed login attempts. Try
  again later."`

### Fail-open on backend errors

If `get_lockout_status` returns an `Err` (transient database hiccup),
the middleware passes the request through. The downstream login
handler will then make the call itself and can decide whether to fail
closed or open. The middleware errs on the side of availability:
taking down the login endpoint whenever the auth database has a blip
is worse than letting the handler make the call directly.

### Layering with `RateLimitMiddleware`

`LoginThrottleMiddleware` is per-account — it gates a single email
when the threshold is crossed. For per-IP quotas, layer it with
[`RateLimitMiddleware`](rate-limiting.md). The two compose naturally:

```rust
let router = Router::new()
    .post("/login", login_handler)
    .middleware(LoginThrottleMiddleware::new(|req| { /* ... */ }))
    .middleware(RateLimitMiddleware::ip_based(20, std::time::Duration::from_secs(60)));
```

Together they cover the realistic shapes of credential stuffing:
distributed (one email × many IPs) is the rate limit's job; focused
(many attempts × one email) is the throttle middleware's job.

### Configuration

Torii's `BruteForceProtectionConfig` defaults to **5 failed attempts
before lockout** and a **15-minute lockout period**. These are what
`init_torii` wires up today; configuring per-app values requires
reaching into torii's own configuration surface and is not exposed
through Suprnova's `ToriiConfig` builder. The defaults are deliberately
conservative — pick "five mistypes locks me out for 15 minutes" before
deciding to relax them.

## Two-Factor (TOTP)

`TwoFactor` covers TOTP-based 2FA — the kind that pairs with any
standards-compliant authenticator app (Google Authenticator, 1Password,
Bitwarden, Authy). The flow is enrollment → confirmation → ongoing
verification, plus single-use recovery codes for when the user loses
their device, plus the challenge flow that stitches everything into the
login lifecycle.

### The `TwoFactorUser` trait

The framework cannot reach into your application's user storage, so
callers implement a small trait to bridge from their user model to the
2FA facade:

```rust
use suprnova::auth_flows::TwoFactorUser;

pub trait TwoFactorUser: Send + Sync {
    fn user_id(&self) -> &str;
    fn email(&self) -> &str;
}
```

`user_id` is the opaque storage key — typically
`torii::UserId.as_str()`, but any stable per-user identifier works.
The 2FA table indexes on it; there is no FK to your user table.

`email` is folded into the `otpauth://` URL's `account_name` segment so
the authenticator app renders the row with a human-readable label
(e.g. "MyCorp (alice@example.com)").

A common pattern is a small newtype that wraps your user model:

```rust
use suprnova::auth_flows::TwoFactorUser;
use suprnova::torii_integration::User as ToriiUser;

struct AppUser2FA<'a> { user: &'a ToriiUser }

impl<'a> TwoFactorUser for AppUser2FA<'a> {
    fn user_id(&self) -> &str { self.user.id.as_str() }
    fn email(&self)   -> &str { &self.user.email }
}
```

### Storage

2FA state lives in the framework-owned `two_factor_credentials` table.
Secrets and recovery codes are encrypted at rest with
`crate::crypto::Crypt::encrypt_string`, which requires a process-global
`EncryptionKey`. Apps opt into the schema by listing both migrations
in their `Migrator::migrations()` — see [Bootstrapping](#bootstrapping).

### Enroll, confirm, verify

```rust
use suprnova::auth_flows::{TwoFactor, EnrollmentResponse};

// 1. Enrollment: generate a fresh secret + 10 recovery codes, persist
//    them encrypted, return everything needed to render the QR code.
let response: EnrollmentResponse = TwoFactor::enroll(&user_2fa).await?;
// response.otpauth_url    — `otpauth://totp/...` deep link
// response.qr_code_svg    — <svg> wrapping a base64 PNG, embed inline
// response.recovery_codes — Vec<String>, 10 plaintext codes — show ONCE

// 2. Confirm: the user opens the authenticator app and types in the
//    6-digit code. `confirm` validates it and stamps `confirmed_at`.
TwoFactor::confirm(&user_2fa, &user_typed_code).await?;
// fires `TwoFactorEnrolled`

// 3. On subsequent logins, gate the session on `verify`:
let ok: bool = TwoFactor::verify(&user_2fa, &code_from_login_form).await?;
if !ok {
    return Err(suprnova::FrameworkError::domain("invalid 2FA code", 401));
}
```

`enroll` returns plaintext recovery codes **exactly once**. There is
no API to retrieve them later — the encrypted column is one-way from
this point on. Show them on the enrollment success page, encourage the
user to save them, and don't store the plaintext anywhere else.

`enroll` refuses to overwrite a **confirmed** enrollment — it returns a
`409` to push the caller toward `re_enroll`, which requires proof of
possession. Re-enrolling on an unconfirmed (pending) row is allowed:
the prior enrollment never became authoritative.

### Replay protection

`verify` writes the current TOTP timestep to `last_used_timestep` on
success. Subsequent verifies where `current_timestep <=
last_used_timestep` are rejected even when the code itself is
structurally valid, defeating a stolen-code replay inside the 30-second
window.

The timestep claim is atomic. The stamp lands via a conditional
`UPDATE … WHERE last_used_timestep IS NULL OR last_used_timestep <
:current`, and the verify only succeeds when the statement affects
exactly one row. Two concurrent verifies in the same timestep cannot
both win: the first flips the column, the second's predicate no
longer matches, and the second is treated as a replay. A plain
read-modify-write would be a TOCTOU race — both verifies read the
pre-stamp row, both validate the same code, both stamp, both succeed.
Concurrent racers are also counted as failed attempts so the
brute-force counter records them.

### Recovery codes

```rust
let consumed: bool = TwoFactor::consume_recovery_code(&user_2fa, &code).await?;
```

Single-use: a matching code is removed from the row before the call
returns, so a second attempt against the same code returns `false`.
Codes are 12 decimal digits in `NNNNNN-NNNNNN` shape (~40 bits of
entropy each, matching Laravel Fortify's format).

`consume_recovery_code` only accepts codes when 2FA is fully confirmed
— it short-circuits to `Ok(false)` while `confirmed_at` is NULL.
Without this gate, an attacker who triggered enrollment on a victim
account (or any flow that creates the row without confirming) could
authenticate using only a fresh recovery code, bypassing TOTP entirely.
The contract is symmetric with `verify`'s "confirmed enrollment only"
guard.

### Rotating recovery codes and secrets

When a user exhausts their recovery codes, or wants to rotate them
after a suspected compromise:

```rust
let fresh: Vec<String> = TwoFactor::regenerate_recovery_codes(&user_2fa, &proof).await?;
```

`proof` must validate as either a current TOTP code or an unused
recovery code. Without the proof check, a session-hijacked attacker
could silently blow away the legitimate user's recovery codes
(denial-of-service against account recovery). The fresh codes replace
the persisted set; the existing secret and `confirmed_at` are
preserved, so the user's authenticator app keeps working without
re-pairing. Errors:

- `400` — no confirmed enrollment exists; call `enroll`/`confirm` first.
- `401` — `proof` validates as neither a TOTP code nor an unused
  recovery code.
- `429` — the account is locked by brute-force throttling.

To rotate the **secret** (re-pair to a new device) without disabling
2FA first:

```rust
let response = TwoFactor::re_enroll(&user_2fa, &proof).await?;
```

Same proof model as `regenerate_recovery_codes`. The row is rewritten
with a fresh secret + 10 fresh recovery codes; `confirmed_at` resets to
NULL so the user must `confirm` with a code from the new authenticator
before 2FA is active again.

### Disable

```rust
TwoFactor::disable(&user_2fa).await?;
// fires `TwoFactorDisabled` only if a row was removed
```

Idempotent: a disable on a user who never enrolled is not an error.
The `TwoFactorDisabled` event fires only on a real state transition,
so audit listeners see one entry per actual disable rather than one
per click on a no-op button.

### Challenge flow (gating login on the second factor)

The enroll / confirm / verify primitives are the building blocks; the
**challenge flow** stitches them into the login lifecycle so a user
with 2FA enabled cannot reach protected pages on password alone.

The flow:

1. Password login resolves a user.
2. If `TwoFactor::is_enabled_by_id(&user_id)` returns `true`, the login
   handler calls `TwoFactor::start_challenge(user_id, remember)` —
   that stashes the user-id as **pending** in the session, clears the
   fully-authenticated slot, revokes any remember-me cookie issued by
   `Auth::attempt`, and remembers whether the user opted into
   remember-me so the cookie can be re-issued after the challenge
   completes. `Auth::id()` returns `None` from this point until the
   challenge completes.
3. The handler redirects to a `/two-factor-challenge` route that shows
   the code form.
4. The challenge POST handler calls
   `TwoFactor::complete_challenge(code)` — verifies the code (TOTP
   **or** an unused recovery code, matching Fortify's challenge
   controller), promotes pending → authed, rotates the session id
   (defeating session fixation) and the CSRF token, re-issues the
   remember-me cookie when the user opted in, and dispatches the
   standard `auth::Login` + `auth::Authenticated` lifecycle events
   plus the 2FA-specific `TwoFactorChallenged`.

```rust
use suprnova::auth_flows::TwoFactor;
use suprnova::{Auth, Authenticatable, Credentials, redirect};

pub async fn login(form: LoginRequest) -> Response {
    match Auth::attempt(&Credentials::password(&form.email, &form.password), form.remember).await? {
        Some(user) => {
            let user_id = user.get_auth_identifier();
            if TwoFactor::is_enabled_by_id(&user_id).await? {
                // Demote to "pending": auth slot cleared, pending set,
                // remember-me cookie revoked. Pass through the form's
                // remember flag so `complete_challenge` can re-issue
                // the cookie on success.
                TwoFactor::start_challenge(user_id, form.remember).await?;
                redirect!("/two-factor-challenge").into()
            } else {
                redirect!("/dashboard").into()
            }
        }
        None => Err(invalid_credentials().into()),
    }
}

pub async fn complete(form: TwoFactorChallengeRequest) -> Response {
    let _user = TwoFactor::complete_challenge(&form.code).await?;
    // Session id + CSRF have rotated; remember-me has been re-issued
    // if the original login form set it. Listeners that hook
    // `auth::Login` / `auth::Authenticated` saw a normal login.
    redirect!("/dashboard").into()
}
```

`complete_challenge` rotates the session id and CSRF token as part of
the promotion to authed. That closes the classic session-fixation
attack where an attacker plants a known session id on a victim before
they log in — after the rotation, the planted id is dead and only the
freshly-generated id carries the authenticated state. The contract
matches `Auth::login_id` / `Auth::login_using_id`, so 2FA logins are
indistinguishable from no-2FA logins in terms of session state and
listener observability.

Gate every protected route group with `TwoFactorChallengeMiddleware`
**before** `AuthMiddleware` so a pending session is bounced to the
challenge page rather than the login page:

```rust
use suprnova::{AuthMiddleware, TwoFactorChallengeMiddleware, group, get};

group!("/dashboard")
    .middleware(TwoFactorChallengeMiddleware::redirect_to("/two-factor-challenge"))
    .middleware(AuthMiddleware::redirect_to("/login"))
    .routes([
        get!("/", dashboard::index),
    ]);
```

The challenge page itself (the GET that renders the form, the POST
that calls `complete_challenge`) must NOT install
`TwoFactorChallengeMiddleware` — it is the destination. The POST
handler typically also checks `TwoFactor::pending_user_id().is_some()`
up front so a stale link does not reach the verify logic with an
empty session.

`TwoFactor::cancel_challenge()` clears both pending slots without
authenticating anyone — wire it to a "back to login" link on the
challenge page.

**Recovery code fallback.** `complete_challenge(code)` tries the TOTP
path first and falls back to consuming a recovery code, so a user who
lost their authenticator can still get in. Each recovery code is
single-use.

**Brute-force linkage.** Failed challenge codes feed the per-account
brute-force counter through `BruteForce::record_failed_attempt`, the
same way bare `TwoFactor::verify` does. An attacker grinding the
challenge form will trip `AccountLocked` after the configured
threshold. A single bad submission counts as **one** failed attempt
even though `complete_challenge` tries both the TOTP and recovery-code
paths internally — the silent-validation cores skip the brute-force
counter so the outer layer records the canonical attempt exactly once.

**Lockout gate.** `complete_challenge` checks `BruteForce::is_locked`
up front and returns `429 Too Many Requests` if the account is
already locked — even when the submitted code is correct. Without
this in-method gate an attacker who tripped the lockout could still
get in by submitting the right code on the next request: the
brute-force counter is keyed on the user's email but `verify` itself
doesn't consult it. The password path's `LoginThrottleMiddleware`
enforces the same constraint at the route layer; composing it in
front of the challenge POST route is fine — both gates are
idempotent.

**Failure event.** `complete_challenge` dispatches
`TwoFactorChallengeFailed { user_id }` on a bad code (or a locked
account), distinct from the password path's `auth::Failed`. Listeners
watching for "user tried 2FA and failed" subscribe to the new event;
listeners watching for "password didn't authenticate" stay on
`auth::Failed`. The two surfaces are kept separate so a 2FA mistype
does not look like a password failure to audit pipelines.

### Why Suprnova diverges

The 2FA `user_id` is intentionally a `String`. If it were typed as
`i64`, `Uuid`, or `torii::UserId`, the 2FA table would be permanently
tied to whatever shape the framework picked first — apps that store
users with a different shape (UUIDs vs auto-increment integers, or
apps that do not use torii at all but want the 2FA module) would be
locked out. A stringy `user_id` lets each app pick whatever stable
per-user identifier it likes; the trade-off is one `.to_string()` at
the call site. Laravel's Fortify ties the equivalent column to
Eloquent's `User::id` — Suprnova decouples it so `TwoFactor` is a
reusable lifecycle primitive, not a User-shaped accessory.

## Remember-me

`suprnova::auth_flows::remember_me` re-exports `suprnova::auth::remember`
— the persistent-cookie module that already shipped alongside session
auth. The re-export is purely organisational: everything auth-flow-shaped
lives under `auth_flows::*`, even when the implementation predates this
namespace.

The design that ships:

- **DB-row + bcrypt hash** — each issued token has a row in the
  `remember_tokens` table storing only the bcrypt hash, never the
  plaintext. A database dump cannot yield re-authenticating credentials.
- **Single-use rotation** — a successful verification DELETEs the
  matched row and issues a fresh one. A captured cookie cannot be
  re-used; if attacker and victim race to use it, the loser sees the
  row gone and fails to authenticate.
- **Revocation** — `revoke_all_for_user` wipes every row for a user
  in one DELETE. `Auth::logout` chains this so a real logout actually
  clears persistent state, and `PasswordReset::complete` does the
  same so a password reset invalidates every existing persistent
  cookie.
- **Pruning** — `prune_expired` cleans up expired rows on a schedule.

In practice the framework's session middleware does the heavy lifting;
the typical app does not call the `remember_me` module directly. The
[Authentication](authentication.md) doc covers the user-facing surface
— the `remember` flag on `Auth::login`, the cookie name, and the
lifetime knobs.

## Events

Nine events fire across the flows, one per security-state transition:

| Event | Fired by | Carries |
|---|---|---|
| `EmailVerified` | `EmailVerification::verify` on success | `user_id: String` |
| `PasswordResetLinkSent` | `PasswordReset::request` on success — anti-enumeration silent for absent emails | `user_id: String`, `email: String` |
| `PasswordResetCompleted` | `PasswordReset::complete` on success | `user_id: String` |
| `AccountLocked` | `BruteForce::record_failed_attempt` on the unlocked → locked transition | `email: String`, `failed_attempts: u32` |
| `AccountUnlocked` | `BruteForce::unlock_account` when an actual unlock occurred | `email: String` |
| `TwoFactorEnrolled` | `TwoFactor::confirm` on success | `user_id: String` |
| `TwoFactorChallenged` | `TwoFactor::complete_challenge` promoted pending → authed | `user_id: String` |
| `TwoFactorChallengeFailed` | `TwoFactor::complete_challenge` rejected a bad code or refused a locked account | `user_id: String` |
| `TwoFactorDisabled` | `TwoFactor::disable` when a row was actually removed | `user_id: String` |

Every event is `Debug + Clone + 'static`, carries no sensitive data
(no plaintext tokens, no IPs), and uses stringy identifiers so
listeners can serialize them across task boundaries without leaking
type information from the user-storage backend.

### Listening

Subscribe via the standard event API — same surface as every other
in-process event:

```rust
use std::sync::Arc;
use suprnova::async_trait;
use suprnova::auth_flows::events::AccountLocked;
use suprnova::{EventFacade, FrameworkError, Listener};

pub struct PageOpsOnLockout;

#[async_trait]
impl Listener<AccountLocked> for PageOpsOnLockout {
    async fn handle(&self, event: &AccountLocked) -> Result<(), FrameworkError> {
        tracing::warn!(
            email = %event.email,
            failed_attempts = event.failed_attempts,
            "account locked — paging ops",
        );
        // ... Slack notification, audit table append, etc.
        Ok(())
    }
}

// In bootstrap.rs:
EventFacade::listen::<AccountLocked, _>(Arc::new(PageOpsOnLockout)).await;
```

Listeners run on Tokio's runtime and are dispatched in registration
order. See the [Events](events.md) chapter for the full surface.

## Testing

Three fakes cover the auth-flows surface, and they compose.

### `Mail::fake()`

Installs a process-local capture transport. Every send during the
guard's lifetime lands in an in-memory buffer instead of going out:

```rust
use suprnova::mail::Mail;

#[tokio::test]
async fn send_link_dispatches_email() {
    let fake = Mail::fake();
    // ... drive the flow ...
    EmailVerification::send_link(&user, "https://app.example.com/verify")
        .await
        .unwrap();
    fake.assert_sent(|m| {
        m.to.iter().any(|a| a.email == "alice@example.com")
            && m.subject.contains("Verify")
    });
    fake.assert_sent_count(1);
}
```

`MailFake` exposes `assert_sent`, `assert_not_sent`,
`assert_sent_count`, plus the raw `captured()` and `count()`
accessors. When the guard drops, the previously-bound transport is
restored — tests that interleave fakes with explicit transport
binding do not leak state.

### `EventFacade::fake()`

The same shape, but for events:

```rust
use suprnova::auth_flows::events::EmailVerified;
use suprnova::events::testing::assert_dispatched;
use suprnova::EventFacade;

#[tokio::test]
async fn verify_fires_email_verified_event() {
    let _guard = EventFacade::fake();
    // ... drive the flow ...
    EmailVerification::verify(&token).await.unwrap();
    assert_dispatched::<EmailVerified>(|e| !e.user_id.is_empty());
}
```

The fake records dispatched events without invoking listeners, so a
listener that talks to an external service will not fire during the
test. The companion `assert_not_dispatched::<E>(pred)` asserts the
negative; `dispatched_count::<E>(pred)` returns the raw count for
finer-grained assertions.

### `ToriiConfig::sqlite_in_memory()` for integration tests

Each test (or each test file) can spin up a fresh torii on an in-memory
SQLite database. The example test files in `framework/tests/` use a
shared runtime + `once_cell::sync::Lazy<()>` pattern to amortise the
cost across tests, plus `#[serial]` to keep the process-global mail
transport stable between tests that interleave `Mail::fake()`:

```rust
use once_cell::sync::Lazy;
use serial_test::serial;
use tokio::runtime::Runtime;
use suprnova::torii_integration::{init_torii, ToriiConfig};

static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

static SETUP: Lazy<()> = Lazy::new(|| {
    RT.block_on(async {
        let config = ToriiConfig::sqlite_in_memory()
            .await
            .expect("sqlite in-memory connection")
            .apply_migrations(true);
        init_torii(config).await.expect("init_torii");
    });
});

#[test]
#[serial]
fn my_test() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        // ... use Mail::fake() / EventFacade::fake() here ...
    });
}
```

Canonical examples — copy from these when writing your own:

- `framework/tests/email_verify.rs` — verify token round-trip,
  `send_link` trailing-slash trimming, `Mail::fake()` assertions on
  subject/HTML.
- `framework/tests/password_reset.rs` — reset round-trip with
  new-password authentication, anti-enumeration on unknown emails,
  `complete` rejects reused tokens.
- `framework/tests/brute_force.rs` — full lockout lifecycle,
  `AccountLocked` fires once per transition, `unlock_account`
  returns `was_locked`.
- `framework/tests/two_factor.rs` — full enroll → confirm → verify
  with a real TOTP code computed from the otpauth URL,
  recovery-code single-use, re-enrollment overwrites the secret,
  replay rejection across two concurrent verifies.
- `framework/tests/two_factor_challenge_flow.rs` — the end-to-end
  challenge flow with session rotation, remember-me re-issue, and
  event dispatch.
- `framework/tests/email_verified_middleware.rs` and
  `two_factor_challenge_middleware.rs` — middleware response shapes
  (403 JSON vs 302 vs 409 + X-Inertia-Location).

## Reference

| Symbol | Purpose |
|---|---|
| `suprnova::auth_flows::EmailVerification` | `generate_token`, `check`, `verify`, `send_link`. |
| `suprnova::auth_flows::EnsureEmailVerifiedMiddleware` | `new()` for 403 JSON, `redirect_to(path)` for 302 / 409 + X-Inertia-Location. |
| `suprnova::auth_flows::PasswordReset` | `request`, `request_with_expiration`, `verify_token`, `complete`, `send_link`. |
| `suprnova::auth_flows::BruteForce` | `record_failed_attempt`, `reset_attempts`, `get_lockout_status`, `is_locked`, `unlock_account`. |
| `suprnova::auth_flows::LoginThrottleMiddleware` | HTTP middleware that 429s pre-handler when the targeted account is locked. |
| `suprnova::auth_flows::TwoFactor` | `enroll`, `re_enroll`, `confirm`, `verify`, `consume_recovery_code`, `regenerate_recovery_codes`, `is_enabled`, `is_enabled_by_id`, `start_challenge`, `pending_user_id`, `cancel_challenge`, `complete_challenge`, `disable`. |
| `suprnova::auth_flows::TwoFactorUser` | Trait bridging the app's user model to the 2FA facade. |
| `suprnova::auth_flows::EnrollmentResponse` | Return value of `TwoFactor::enroll` — `otpauth_url`, `qr_code_svg`, `recovery_codes`. |
| `suprnova::auth_flows::TwoFactorChallengeMiddleware` | `new()` for 403 JSON, `redirect_to(path)` for 302 / 409 + X-Inertia-Location. Compose in front of `AuthMiddleware`. |
| `suprnova::auth_flows::two_factor::migration::Migration` | SeaORM migration for `two_factor_credentials`. List in your `Migrator::migrations()`. |
| `suprnova::auth_flows::two_factor::migration_replay::Migration` | Column add for `last_used_timestep` (TOTP replay protection). List after the create-table migration. |
| `suprnova::auth_flows::remember_me` | Re-export of `suprnova::auth::remember`. |
| `suprnova::auth_flows::events::*` | Nine events — see [Events](#events). |
| `suprnova::auth_flows::EmailVerificationMail` | Transactional Mailable. Subject `"Verify your email for {APP_NAME}"`. |
| `suprnova::auth_flows::PasswordResetMail` | Transactional Mailable. Subject `"Reset your {APP_NAME} password"`. |
| `suprnova::auth_flows::PasswordChangedMail` | Security-notification Mailable. Subject `"Your {APP_NAME} password was changed"`. |
| `suprnova::torii_integration::ToriiConfig` | Torii bootstrap config. `from_sea_orm(conn)` for production, `sqlite_in_memory()` for tests. |
| `suprnova::torii_integration::init_torii` | Idempotent global init. Call once from `bootstrap.rs::register()`. |

## Next

- [Authentication](authentication.md) — guards, providers, the
  `Auth` facade, `AuthMiddleware`.
- [Mail](mail.md) — the transport layer the `send_link` calls
  dispatch through.
- [Events](events.md) — registering listeners for the nine
  auth-flow events.
- [Rate Limiting](rate-limiting.md) — pair
  `RateLimitMiddleware::ip_based` with `LoginThrottleMiddleware` for
  layered defence.
- [Session](session.md) — what `start_challenge` /
  `complete_challenge` touch when they rotate the session id.
