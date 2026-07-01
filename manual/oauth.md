# OAuth, Apple & Magic-Link Login

Suprnova ships three torii-backed login methods behind the `Auth` facade:
**generic OAuth** (GitHub, Google, or any OIDC/OAuth2 provider), **Sign in with
Apple**, and **passwordless magic links**. They share one prerequisite
(`init_torii` plus the ceremony migration) and the same facade shape —
`Auth::oauth(provider)` / `Auth::magic_link()` — and none of them ship routes:
you add a thin controller (start + callback) and the framework does the CSRF
state, PKCE, token exchange, identity verification, user upsert, and session
minting.

The whole surface lives in `framework/src/torii_integration/`. There is **no**
framework env-var contract for any of it — every credential is passed
programmatically (pull your own from the environment); this chapter's examples
use `std::env::var(...)` purely to show where your secrets go.

## Prerequisites

1. **Initialise torii once at boot** — this backs the user upsert and session
   creation:

   ```rust
   use suprnova::{init_torii, ToriiConfig};

   // in bootstrap::register(), after DB::init()
   init_torii(ToriiConfig::from_sea_orm(db_conn)).await?;
   ```

2. **Run the ceremony migration.** OAuth and Apple stash a short-lived
   (10-minute) CSRF-`state` + PKCE ceremony in the `auth_ceremony_tokens` table.
   Register the migration `m20251209_000000_create_auth_ceremony_tokens_table`
   in your `Migrator` (the starter kits already include it). Optionally schedule
   `suprnova::torii_integration::ceremony::prune_expired()` to GC stale rows.

3. **`SessionMiddleware` on the OAuth *start* route.** `begin()` writes the
   `state` into the session; a sessionless call fails with a 500.

Magic links need only step 1.

## Generic OAuth (GitHub, Google, custom)

### Configure a provider

Register each provider once at startup. The registry is process-global and
idempotent, so re-registering the same provider just replaces the config:

```rust
use suprnova::Auth;
use suprnova::torii_integration::oauth::OAuthProviderConfig;

Auth::oauth("github").configure(OAuthProviderConfig {
    client_id: std::env::var("GITHUB_CLIENT_ID")?,
    client_secret: std::env::var("GITHUB_CLIENT_SECRET")?,
    redirect_url: "https://app.example.com/auth/oauth/github/callback".into(),
    scopes: vec!["user:email".into()],
    endpoints_override: None,   // None → the built-in well-known table
    apple_key_pair: None,       // Apple-only; leave None for GitHub/Google
    apple_team_id: None,        // Apple-only
});
```

The well-known authorize/token/userinfo endpoints are built in for `github`,
`google`, and `apple`. For any other provider — or a self-hosted / test server —
supply them yourself:

```rust
use suprnova::torii_integration::oauth::EndpointOverrides;

Auth::oauth("gitlab").configure(OAuthProviderConfig {
    client_id: /* … */,
    client_secret: /* … */,
    redirect_url: /* … */,
    scopes: vec!["read_user".into()],
    endpoints_override: Some(EndpointOverrides {
        authorize: "https://gitlab.com/oauth/authorize".into(),
        token: "https://gitlab.com/oauth/token".into(),
        userinfo: "https://gitlab.com/api/v4/user".into(),
        emails: None,   // GitHub-style /emails fallback for a private primary
    }),
    apple_key_pair: None,
    apple_team_id: None,
});
```

### Start the flow (authorize URL)

```rust
// GET /auth/oauth/github/start  (route MUST carry SessionMiddleware)
let kickoff = Auth::oauth("github").begin().await?;
// kickoff.authorization_url — redirect the browser here
// kickoff.state             — CSRF state, already stored in the session for you
```

`begin()` mints the CSRF `state` (UUID v4) and an RFC 7636 PKCE
verifier/S256 challenge, records the ceremony (10-minute TTL), and returns the
provider authorize URL. Redirect the user to `authorization_url`.

### Complete the flow — `verify` vs `complete`

On the callback you have two entry points (split in 0.5.4). Pick by whether your
`users` table **is** torii's schema:

| Method | Returns | Side effects | Use when |
|---|---|---|---|
| `verify_oauth_identity(code, state)` | `OAuthIdentity { provider, subject, email, name }` | **None** — verifies the ceremony, exchanges the code, fetches userinfo, extracts a verified email + stable `subject`. No user, no session. | Your app owns its `users` table and you want to look up / create the user yourself. |
| `complete(code, state)` | `(User, Session)` | Upserts the user into torii (`get_or_create_user`) and mints a session. | Your `users` table is torii's schema. |

```rust
// Custom users table:
let id = Auth::oauth("github").verify_oauth_identity(&code, &state).await?;
// id.subject is the stable provider id; id.email is verified-or-None.
let user = my_users::upsert(id.provider, id.subject, id.email, id.name).await?;

// …or, torii-backed:
let (user, session) = Auth::oauth("github").complete(&code, &state).await?;
```

A `verify`-returned `email` is always a *verified* address (OIDC `email_verified`,
GitHub treated as verified, or the `/emails` fallback); an unverified or absent
email comes back as `None` and repeat logins resolve by `subject`.

### Routes you add

The framework provides no OAuth routes — wire two thin handlers (mirror the shape
of the existing `auth_verify` / `auth_reset` controllers in the starter kit):

```rust
// start — redirects to the provider
get!("/auth/oauth/{provider}/start", controllers::oauth::start),
// callback — GitHub/Google use GET ?code&state
get!("/auth/oauth/{provider}/callback", controllers::oauth::callback),
```

Put the `/start` route (at least) behind `SessionMiddleware`.

## Sign in with Apple

Apple is the same facade — `Auth::oauth("apple")` — with a few Apple-specific
rules baked in:

- **The callback is a `POST`.** Apple uses `response_mode=form_post`, so the
  redirect delivers `code` + `state` in a form body, not query params. Register
  the Apple callback as a `post!` route and read the fields from the form.
- **No PKCE.** Apple rejects `code_challenge`, so the authorize URL omits it
  (the client secret is a signed JWT instead).
- **`client_secret` is unused** — leave it `String::new()`. Suprnova mints the
  short-lived JWT client secret from your `.p8` key on each token exchange.
- **ID tokens are verified against Apple's JWKS (RS256)** since 0.5.6, not
  trusted structurally.

### Supply your Apple key — `AppleKeyPair`

`AppleKeyPair` is the one Apple type re-exported for apps (so you need no direct
`apple` dependency). Build it from your `.p8` signing key:

```rust
use suprnova::torii_integration::oauth::AppleKeyPair;

let key = AppleKeyPair::from_file(
    &std::env::var("APPLE_KEY_ID")?,   // Apple *Key ID* (not the Team ID)
    &std::env::var("APPLE_P8_PATH")?,  // path to AuthKey_XXXXXX.p8
)?;
// or: AppleKeyPair::from_base64(key_id, b64)  /  from_pem_bytes(key_id, bytes)
```

### Configure Apple

```rust
use suprnova::torii_integration::oauth::OAuthProviderConfig;

Auth::oauth("apple").configure(OAuthProviderConfig {
    client_id: std::env::var("APPLE_CLIENT_ID")?,  // your Services ID
    client_secret: String::new(),                  // unused — minted from the key
    redirect_url: "https://app.example.com/auth/apple/callback".into(),
    scopes: vec!["email".into(), "name".into()],
    endpoints_override: None,
    apple_key_pair: Some(key),
    apple_team_id: Some(std::env::var("APPLE_TEAM_ID")?),  // 10-char Team ID
});
```

### Complete the Apple flow

Same split as generic OAuth. `complete` upserts + sessions; the verify path
returns an `AppleIdentity` for a custom users table:

```rust
// POST /auth/apple/callback  — read code + state from the FORM body
let (user, session) = Auth::oauth("apple").complete(&code, &state).await?;

// …or custom users table:
let id = Auth::oauth("apple").verify_apple_identity(&code, &state).await?;
// id: AppleIdentity { provider, subject, email, email_verified, is_private_email }
```

`AppleIdentity.email` is `Some(_)` only when Apple asserts it verified; an
unverified email is refused (401) before the identity is built. `is_private_email`
is set when the user chose Apple's private-relay address — persist the `subject`
as the stable key, since the relay address is the only email you'll get.

## Magic-Link Login

Passwordless email login, torii-backed, via `Auth::magic_link()`. The framework
issues and verifies the token; **you** email the link (it never sends mail
itself), which composes cleanly with the [Mail](mail.md) chapter.

```rust
use suprnova::Auth;

// POST /auth/magic  — request a link
let token = Auth::magic_link()
    .send("alice@example.com", "https://app.example.com/auth/magic")
    .await?;
// Build the link and email it yourself:
Mail::to("alice@example.com")
    .send(MagicLink { url: format!("https://app.example.com/auth/magic?token={token}") })
    .await?;

// GET /auth/magic?token=…  — consume it (single-use; a second call fails)
let (user, session) = Auth::magic_link().consume(&token).await?;
```

The user is auto-created on first use. `send` returns the **plaintext** token so
you control the URL shape and delivery.

> **Note — `TokenPurpose::MagicLink`.** The `auth_flows`
> `TokenPurpose` enum has a `MagicLink` variant (added in 0.5.5), but it is a
> *reserved discriminator* for the generic `TokenStore` — no built-in flow
> consumes it. The working, supported magic-link path is `Auth::magic_link()`
> above. Only reach for `TokenPurpose::MagicLink` if you are hand-rolling your
> own flow on the `auth_flow_tokens` table.

## A note on configuration

None of these methods read framework environment variables — provider IDs,
secrets, redirect URLs, and Apple keys are all passed to `configure(...)`
programmatically. Load them however you like (`std::env::var`, a typed config
struct, a secret manager) and register providers once during `bootstrap`. This
keeps multi-tenant / per-deploy provider setups first-class instead of forcing a
fixed env-var naming scheme.

## Reference

- Facade entry points: `Auth::oauth(provider)`, `Auth::magic_link()`
  (`suprnova::Auth`)
- Config: `suprnova::torii_integration::oauth::{OAuthProviderConfig, EndpointOverrides, AppleKeyPair}`
- OAuth results: `OAuthKickoff { authorization_url, state }`,
  `OAuthIdentity { provider, subject, email, name }`,
  `AppleIdentity { provider, subject, email, email_verified, is_private_email }`
- Bootstrap: `suprnova::{init_torii, ToriiConfig}`
- Ceremony store: `auth_ceremony_tokens` table +
  `suprnova::torii_integration::ceremony::prune_expired()`

## Next

- [Authentication](authentication.md) — guards, providers, and the
  `Authenticatable` user model these flows create sessions for
- [Auth Flows](auth-flows.md) — email verification, password reset, and 2FA
- [Mail](mail.md) — sending the magic-link email (and the `MAIL_FROM` /
  `MAIL_FROM_NAME` sender config)
- [Sessions](session.md) — what the returned `Session` is and how it's persisted
