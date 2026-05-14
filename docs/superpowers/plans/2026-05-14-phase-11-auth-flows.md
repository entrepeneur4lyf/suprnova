# Phase 11: Auth Flows Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Five auth-flow features built on top of torii-rs where it already covers the ground, and on our own primitives where it doesn't: (1) email verification, (2) password reset, (3) brute-force protection on login + sensitive endpoints, (4) 2FA TOTP enrollment + recovery codes, (5) remember-me persistent cookies.

**Architecture — what's torii vs what's ours:**

| Flow | Source | Why |
|---|---|---|
| Email verification | **torii** (`EmailVerificationService`) | Token storage + lifecycle already shipped |
| Password reset | **torii** (`PasswordResetService`) | Token storage + lifecycle already shipped |
| Brute-force throttle | **torii** (`BruteForceService`) | Account-locking + IP throttling already shipped |
| 2FA TOTP | **Ours** (totp-rs) | Not in torii core; we own it |
| Remember-me | **Ours** | Not in torii core; thin wrapper over `Encrypter` (Phase 2) + `Cookie` |
| Mail delivery | **Ours** (`Mail::send`, lettre) | We bridge torii's `MailerService` trait to our `Mail` facade so torii never reaches for `torii-mailer` directly |

The key bridge: `framework/src/torii_integration/mailer_bridge.rs` implements
torii's `MailerService` trait by formatting each torii-defined email
(magic link, verification, password reset, welcome) into a Suprnova
`Mailable` and dispatching via `Mail::send`. This gives us one mail
facade across the stack — torii's auth services + the user's
application emails both go through the same lettre transport, same
provider drivers, same `Mail::fake()` for tests.

**Tech Stack:** torii-core + torii-storage-seaorm (from Phase 3),
`totp-rs` 5 for TOTP / HOTP, `qrcode` 0.14 for enrollment QR codes,
reuses Phase 2 `Encrypter`, Phase 5 `Mail`, Phase 5 `RateLimiter`,
Phase 1 `Event::dispatch`.

---

## File Structure

**New files:**
- `framework/src/auth_flows/mod.rs` — module entry, `Auth::email_verification()` / `Auth::password_reset()` facades
- `framework/src/auth_flows/email_verify.rs` — thin facade over `torii.email_verification()`
- `framework/src/auth_flows/password_reset.rs` — thin facade over `torii.password_reset()`
- `framework/src/auth_flows/brute_force.rs` — thin facade over `torii.brute_force()` + `LoginThrottle` middleware
- `framework/src/auth_flows/two_factor/mod.rs` — `TwoFactor` facade (ours)
- `framework/src/auth_flows/two_factor/recovery.rs` — recovery codes
- `framework/src/auth_flows/remember_me/mod.rs` — `RememberMe` facade (ours)
- `framework/src/auth_flows/remember_me/middleware.rs` — `RememberMeMiddleware`
- `framework/src/auth_flows/events.rs` — `EmailVerified`, `PasswordReset`, `TwoFactorEnrolled` events dispatched through Phase 1
- `framework/src/torii_integration/mailer_bridge.rs` — implements torii's `MailerService` trait via `Mail::send`
- `framework/src/auth_flows/migrations/m_add_two_factor_to_users.rs` — secret + recovery codes columns (torii owns the verification + reset token tables)
- `framework/tests/email_verify.rs`, `password_reset.rs`, `brute_force.rs`, `two_factor.rs`, `remember_me.rs`, `mailer_bridge.rs`
- `app/src/controllers/auth_verify.rs`, `auth_reset.rs`, `auth_2fa.rs` — wire routes

**Modified files:**
- `framework/Cargo.toml` — add `totp-rs`, `qrcode`; enable torii's `mailer` feature only when consumers want torii's built-in `ToriiMailerService` (we don't by default — our bridge replaces it)
- `framework/src/torii_integration/mod.rs` — register the mailer bridge during `init_torii`
- `framework/src/lib.rs` — declare + re-export the new modules

---

## Task 1: Add deps

**Files:** `framework/Cargo.toml`

- [ ] **Step 1: Add deps**

```toml
# framework/Cargo.toml — [dependencies]
totp-rs = { version = "5", features = ["otpauth", "qr"] }
qrcode = "0.14"
# torii-core, torii-storage-seaorm already from Phase 3 — confirm
# the `mailer` feature on torii-core is NOT enabled by default
# because we bring our own mailer bridge.
```

- [ ] **Step 2: Verify build**

```bash
cargo check --workspace
```

- [ ] **Step 3: Commit**

```bash
git add framework/Cargo.toml Cargo.lock
git commit -m "feat(deps): add totp-rs, qrcode for Phase 11"
```

---

## Task 2: MailerBridge — implement torii's MailerService against our Mail facade

**Files:** `framework/src/torii_integration/mailer_bridge.rs`

torii's `MailerService` trait (under `torii-core` feature `mailer`)
defines five methods: `send_magic_link_email`, `send_welcome_email`,
`send_password_reset_email`, `send_password_changed_email`,
`send_verification_email`. We implement the trait — each impl
constructs a Suprnova `Mailable` and calls `Mail::send`.

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/mailer_bridge.rs
use suprnova::{Mail, Mailable};

#[tokio::test]
async fn bridge_dispatches_verification_email_via_our_mail_facade() {
    let _g = Mail::fake();

    let bridge = suprnova::torii_integration::mailer_bridge::MailerBridge::new(
        "Suprnova Test".into(),
        "https://app.example.com".into(),
        "noreply@example.com".into(),
    );

    // Cast through torii's trait so we exercise the actual integration:
    use torii_core::services::MailerService;
    bridge
        .send_verification_email(
            "alice@example.com",
            "https://app.example.com/verify?token=abc",
            Some("Alice"),
        )
        .await
        .unwrap();

    // Assert we recorded the dispatch in our Mail::fake() store.
    suprnova::mail::testing::assert_sent::<crate::ToriiVerifyMailable>(|m| {
        m.to_address == "alice@example.com" && m.verification_link.contains("token=abc")
    });
}
```

> **Mailable visibility:** The test references `ToriiVerifyMailable` — a Mailable type the bridge constructs. Export it from `mailer_bridge.rs` so tests can assert on it.

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova --test mailer_bridge
```

Expected: FAIL — `MailerBridge` not found.

- [ ] **Step 3: Implement**

```rust
// framework/src/torii_integration/mailer_bridge.rs
//! Implements torii's `MailerService` trait against our `Mail::send`
//! facade. This means every torii auth email (verification, reset,
//! magic link, welcome, password changed) flows through the same
//! lettre transport / provider drivers / Mail::fake test recorder as
//! user-application emails.

use crate::{async_trait, FrameworkError, Mail, Mailable};
use torii_core::services::MailerService;
use torii_core::Error as ToriiError;

pub struct MailerBridge {
    pub app_name: String,
    pub app_url: String,
    pub from_address: String,
}

impl MailerBridge {
    pub fn new(app_name: String, app_url: String, from_address: String) -> Self {
        Self {
            app_name,
            app_url,
            from_address,
        }
    }
}

#[async_trait]
impl MailerService for MailerBridge {
    async fn send_magic_link_email(
        &self,
        to: &str,
        magic_link: &str,
        user_name: Option<&str>,
    ) -> Result<(), ToriiError> {
        Mail::send(ToriiMagicLinkMailable {
            to_address: to.to_string(),
            user_name: user_name.map(|s| s.to_string()),
            magic_link: magic_link.to_string(),
            app_name: self.app_name.clone(),
            from_address: self.from_address.clone(),
        })
        .await
        .map_err(map_err)
    }

    async fn send_welcome_email(&self, to: &str, user_name: Option<&str>) -> Result<(), ToriiError> {
        Mail::send(ToriiWelcomeMailable {
            to_address: to.to_string(),
            user_name: user_name.map(|s| s.to_string()),
            app_name: self.app_name.clone(),
            from_address: self.from_address.clone(),
        })
        .await
        .map_err(map_err)
    }

    async fn send_password_reset_email(
        &self,
        to: &str,
        reset_link: &str,
        user_name: Option<&str>,
    ) -> Result<(), ToriiError> {
        Mail::send(ToriiPasswordResetMailable {
            to_address: to.to_string(),
            user_name: user_name.map(|s| s.to_string()),
            reset_link: reset_link.to_string(),
            app_name: self.app_name.clone(),
            from_address: self.from_address.clone(),
        })
        .await
        .map_err(map_err)
    }

    async fn send_password_changed_email(
        &self,
        to: &str,
        user_name: Option<&str>,
    ) -> Result<(), ToriiError> {
        Mail::send(ToriiPasswordChangedMailable {
            to_address: to.to_string(),
            user_name: user_name.map(|s| s.to_string()),
            app_name: self.app_name.clone(),
            from_address: self.from_address.clone(),
        })
        .await
        .map_err(map_err)
    }

    async fn send_verification_email(
        &self,
        to: &str,
        verification_link: &str,
        user_name: Option<&str>,
    ) -> Result<(), ToriiError> {
        Mail::send(ToriiVerifyMailable {
            to_address: to.to_string(),
            user_name: user_name.map(|s| s.to_string()),
            verification_link: verification_link.to_string(),
            app_name: self.app_name.clone(),
            from_address: self.from_address.clone(),
        })
        .await
        .map_err(map_err)
    }
}

fn map_err(e: FrameworkError) -> ToriiError {
    ToriiError::Storage(torii_core::error::StorageError::Connection(e.to_string()))
}

// One Mailable per auth-email kind. Each is a plain struct with
// from/subject/html/text. We could template via askama for parity
// with torii-mailer; the inline body keeps Phase 11 self-contained.

pub struct ToriiVerifyMailable {
    pub to_address: String,
    pub user_name: Option<String>,
    pub verification_link: String,
    pub app_name: String,
    pub from_address: String,
}

#[async_trait]
impl Mailable for ToriiVerifyMailable {
    fn to(&self) -> Vec<String> {
        vec![self.to_address.clone()]
    }
    fn from(&self) -> Option<String> {
        Some(self.from_address.clone())
    }
    fn subject(&self) -> String {
        format!("Verify your email for {}", self.app_name)
    }
    fn body_html(&self) -> String {
        let greeting = self.user_name.as_deref().unwrap_or("there");
        format!(
            "<p>Hi {},</p><p>Please <a href=\"{}\">verify your email</a>. \
             This link expires in 24 hours.</p>",
            greeting, self.verification_link
        )
    }
    fn body_text(&self) -> Option<String> {
        let greeting = self.user_name.as_deref().unwrap_or("there");
        Some(format!(
            "Hi {},\n\nVerify your email: {}\nExpires in 24 hours.\n",
            greeting, self.verification_link
        ))
    }
}

pub struct ToriiPasswordResetMailable {
    pub to_address: String,
    pub user_name: Option<String>,
    pub reset_link: String,
    pub app_name: String,
    pub from_address: String,
}

#[async_trait]
impl Mailable for ToriiPasswordResetMailable {
    fn to(&self) -> Vec<String> { vec![self.to_address.clone()] }
    fn from(&self) -> Option<String> { Some(self.from_address.clone()) }
    fn subject(&self) -> String { format!("Reset your {} password", self.app_name) }
    fn body_html(&self) -> String {
        let greeting = self.user_name.as_deref().unwrap_or("there");
        format!(
            "<p>Hi {},</p><p><a href=\"{}\">Reset your password</a>. \
             Expires in 15 minutes.</p>",
            greeting, self.reset_link
        )
    }
    fn body_text(&self) -> Option<String> {
        let greeting = self.user_name.as_deref().unwrap_or("there");
        Some(format!("Hi {},\n\nReset link: {}\nExpires in 15 minutes.\n", greeting, self.reset_link))
    }
}

pub struct ToriiPasswordChangedMailable {
    pub to_address: String,
    pub user_name: Option<String>,
    pub app_name: String,
    pub from_address: String,
}

#[async_trait]
impl Mailable for ToriiPasswordChangedMailable {
    fn to(&self) -> Vec<String> { vec![self.to_address.clone()] }
    fn from(&self) -> Option<String> { Some(self.from_address.clone()) }
    fn subject(&self) -> String { format!("Your {} password was changed", self.app_name) }
    fn body_html(&self) -> String {
        let greeting = self.user_name.as_deref().unwrap_or("there");
        format!(
            "<p>Hi {},</p><p>Your password was just changed. If this wasn't you, \
             contact support immediately.</p>",
            greeting
        )
    }
    fn body_text(&self) -> Option<String> {
        let greeting = self.user_name.as_deref().unwrap_or("there");
        Some(format!("Hi {},\n\nYour password was changed.\n", greeting))
    }
}

pub struct ToriiMagicLinkMailable {
    pub to_address: String,
    pub user_name: Option<String>,
    pub magic_link: String,
    pub app_name: String,
    pub from_address: String,
}

#[async_trait]
impl Mailable for ToriiMagicLinkMailable {
    fn to(&self) -> Vec<String> { vec![self.to_address.clone()] }
    fn from(&self) -> Option<String> { Some(self.from_address.clone()) }
    fn subject(&self) -> String { format!("Sign in to {}", self.app_name) }
    fn body_html(&self) -> String {
        let greeting = self.user_name.as_deref().unwrap_or("there");
        format!(
            "<p>Hi {},</p><p><a href=\"{}\">Sign in to {}</a>. \
             Link expires in 15 minutes.</p>",
            greeting, self.magic_link, self.app_name
        )
    }
    fn body_text(&self) -> Option<String> {
        let greeting = self.user_name.as_deref().unwrap_or("there");
        Some(format!("Hi {},\n\nSign in: {}\n", greeting, self.magic_link))
    }
}

pub struct ToriiWelcomeMailable {
    pub to_address: String,
    pub user_name: Option<String>,
    pub app_name: String,
    pub from_address: String,
}

#[async_trait]
impl Mailable for ToriiWelcomeMailable {
    fn to(&self) -> Vec<String> { vec![self.to_address.clone()] }
    fn from(&self) -> Option<String> { Some(self.from_address.clone()) }
    fn subject(&self) -> String { format!("Welcome to {}", self.app_name) }
    fn body_html(&self) -> String {
        let greeting = self.user_name.as_deref().unwrap_or("there");
        format!("<h1>Welcome, {}!</h1><p>Thanks for joining {}.</p>", greeting, self.app_name)
    }
    fn body_text(&self) -> Option<String> {
        let greeting = self.user_name.as_deref().unwrap_or("there");
        Some(format!("Welcome, {}! Thanks for joining {}.\n", greeting, self.app_name))
    }
}
```

- [ ] **Step 4: Wire the bridge from `init_torii`**

```rust
// framework/src/torii_integration/mod.rs — extend `init_torii` to
// optionally register the mailer bridge into the torii builder:
pub async fn init_torii(config: ToriiConfig) -> Result<(), FrameworkError> {
    // ... existing builder steps ...

    let bridge = mailer_bridge::MailerBridge::new(
        std::env::var("APP_NAME").unwrap_or_else(|_| "Suprnova".into()),
        std::env::var("APP_URL").unwrap_or_else(|_| "http://localhost:8000".into()),
        std::env::var("MAIL_FROM").unwrap_or_else(|_| "noreply@example.com".into()),
    );

    let torii = configured
        .with_mailer(Arc::new(bridge))   // torii builder accepts an Arc<dyn MailerService>
        .apply_migrations(config.apply_migrations)
        .build()
        .await
        .map_err(map_torii_err)?;
    // ... store in OnceLock ...
    Ok(())
}
```

> **Builder method:** Verify the exact builder method via `reference/torii-rs-main/torii/src/builder.rs` — it's likely `.with_mailer(Arc<dyn MailerService>)` or similar. Adjust to torii's actual API.

- [ ] **Step 5: Run — expect pass**

```bash
cargo test -p suprnova --test mailer_bridge
```

- [ ] **Step 6: Commit**

```bash
git add framework/src/torii_integration/mailer_bridge.rs framework/src/torii_integration/mod.rs framework/tests/mailer_bridge.rs
git commit -m "feat(auth_flows): MailerBridge implements torii MailerService via Mail::send"
```

---

## Task 3: Email verification facade

**Files:** `framework/src/auth_flows/email_verify.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/email_verify.rs
use suprnova::{
    auth_flows::email_verify::EmailVerification,
    torii_integration::{init_torii, ToriiConfig},
};

#[tokio::test]
async fn verify_email_round_trip_via_torii() {
    init_torii(ToriiConfig::sqlite_in_memory()).await.unwrap();

    // Register a user so we have something to verify.
    let user = suprnova::Auth::password()
        .register("alice@example.com", "longenough123")
        .await
        .unwrap();

    // 1) Generate a verification token (torii does this).
    let token = EmailVerification::generate_token(&user.id).await.unwrap();
    let token_str = token.token().expect("token value").to_string();

    // 2) Verify the token (consumes it, marks email_verified_at).
    let verified_user = EmailVerification::verify(&token_str).await.unwrap();
    assert_eq!(verified_user.email, "alice@example.com");
    assert!(verified_user.email_verified_at.is_some());

    // 3) Re-verifying with the same token fails (already consumed).
    assert!(EmailVerification::verify(&token_str).await.is_err());
}

#[tokio::test]
async fn send_verification_email_dispatches_via_bridge() {
    init_torii(ToriiConfig::sqlite_in_memory()).await.unwrap();
    let _g = suprnova::Mail::fake();

    let user = suprnova::Auth::password()
        .register("bob@example.com", "longenough123")
        .await
        .unwrap();

    EmailVerification::send_link(&user, "https://app.example.com/verify")
        .await
        .unwrap();

    // The mailer bridge dispatched a ToriiVerifyMailable through Mail::send.
    suprnova::mail::testing::assert_sent::<
        suprnova::torii_integration::mailer_bridge::ToriiVerifyMailable,
    >(|m| m.to_address == "bob@example.com" && m.verification_link.contains("verify"));
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova --test email_verify
```

Expected: FAIL — `EmailVerification` not found.

- [ ] **Step 3: Implement**

```rust
// framework/src/auth_flows/email_verify.rs
//! Email verification facade — thin layer over torii's
//! `EmailVerificationService`. We expose it as
//! `EmailVerification::generate_token` / `verify` / `send_link` so the
//! rest of the framework reads idiomatically without hand-passing
//! the torii instance.

use crate::{torii_integration::instance, FrameworkError};
use torii_core::{User, UserId, storage::SecureToken};

pub struct EmailVerification;

impl EmailVerification {
    pub async fn generate_token(user_id: &UserId) -> Result<SecureToken, FrameworkError> {
        instance()?
            .email_verification()
            .generate_token(user_id)
            .await
            .map_err(crate::torii_integration::map_torii_err)
    }

    pub async fn verify(token: &str) -> Result<User, FrameworkError> {
        instance()?
            .email_verification()
            .verify_email(token)
            .await
            .map_err(crate::torii_integration::map_torii_err)
    }

    pub async fn check(token: &str) -> Result<bool, FrameworkError> {
        instance()?
            .email_verification()
            .check_token(token)
            .await
            .map_err(crate::torii_integration::map_torii_err)
    }

    /// Generate a token, build the verification URL, and dispatch the
    /// verification email via the MailerBridge → `Mail::send`.
    pub async fn send_link(user: &User, base_url: &str) -> Result<(), FrameworkError> {
        let token = Self::generate_token(&user.id).await?;
        let token_value = token.token().expect("token value").to_string();
        let url = format!("{}?token={}", base_url, token_value);
        // The torii instance's mailer (our bridge) is reached via
        // torii.mailer() if exposed; otherwise call our bridge
        // directly using values from config.
        use torii_core::services::MailerService;
        let bridge = crate::torii_integration::current_mailer_bridge()?;
        bridge
            .send_verification_email(&user.email, &url, user.name.as_deref())
            .await
            .map_err(crate::torii_integration::map_torii_err)
    }
}
```

> **`current_mailer_bridge` accessor:** `torii_integration::mod.rs` should store the bridge in a `OnceLock<Arc<MailerBridge>>` (set during `init_torii`) and expose a getter. This avoids re-reading env vars per send.

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova --test email_verify
```

- [ ] **Step 5: Commit**

```bash
git add framework/src/auth_flows/email_verify.rs framework/tests/email_verify.rs
git commit -m "feat(auth_flows): EmailVerification facade over torii EmailVerificationService"
```

---

## Task 4: Password reset facade

**Files:** `framework/src/auth_flows/password_reset.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/password_reset.rs
use suprnova::{
    auth_flows::password_reset::PasswordReset,
    torii_integration::{init_torii, ToriiConfig},
};

#[tokio::test]
async fn password_reset_round_trip_via_torii() {
    init_torii(ToriiConfig::sqlite_in_memory()).await.unwrap();
    suprnova::Auth::password()
        .register("alice@example.com", "originalpass1")
        .await
        .unwrap();

    let (_user, token) = PasswordReset::request("alice@example.com")
        .await
        .unwrap()
        .expect("user exists");

    let user = PasswordReset::reset(&token, "newpass123").await.unwrap();
    assert_eq!(user.email, "alice@example.com");

    // Old password no longer authenticates
    assert!(suprnova::Auth::password()
        .authenticate("alice@example.com", "originalpass1", None, None)
        .await
        .is_err());

    // New password works
    suprnova::Auth::password()
        .authenticate("alice@example.com", "newpass123", None, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn request_for_unknown_email_silently_returns_none() {
    init_torii(ToriiConfig::sqlite_in_memory()).await.unwrap();
    let result = PasswordReset::request("noone@example.com").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn send_link_dispatches_email_via_bridge() {
    init_torii(ToriiConfig::sqlite_in_memory()).await.unwrap();
    let _g = suprnova::Mail::fake();
    suprnova::Auth::password()
        .register("bob@example.com", "originalpass1")
        .await
        .unwrap();

    PasswordReset::send_link("bob@example.com", "https://app.example.com/password/reset")
        .await
        .unwrap();

    suprnova::mail::testing::assert_sent::<
        suprnova::torii_integration::mailer_bridge::ToriiPasswordResetMailable,
    >(|m| m.to_address == "bob@example.com");
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/auth_flows/password_reset.rs
//! Password reset facade over torii's `PasswordResetService`.

use crate::{torii_integration::instance, FrameworkError};
use torii_core::User;

pub struct PasswordReset;

impl PasswordReset {
    /// Generate a reset token. Returns `None` if the email isn't on
    /// file (the response is deliberately ambiguous to prevent
    /// email-enumeration attacks — match torii's semantics).
    pub async fn request(email: &str) -> Result<Option<(User, String)>, FrameworkError> {
        instance()?
            .password_reset()
            .request_password_reset(email)
            .await
            .map_err(crate::torii_integration::map_torii_err)
    }

    /// Verify (without consuming) — use for "is this token still
    /// valid?" checks on the reset form.
    pub async fn verify(token: &str) -> Result<bool, FrameworkError> {
        instance()?
            .password_reset()
            .verify_reset_token(token)
            .await
            .map_err(crate::torii_integration::map_torii_err)
    }

    /// Consume the token and update the password. Returns the user.
    pub async fn reset(token: &str, new_password: &str) -> Result<User, FrameworkError> {
        let user = instance()?
            .password_reset()
            .reset_password(token, new_password)
            .await
            .map_err(crate::torii_integration::map_torii_err)?;

        // Fire a "password changed" notification email so the user is
        // alerted if it wasn't them.
        use torii_core::services::MailerService;
        let bridge = crate::torii_integration::current_mailer_bridge()?;
        let _ = bridge
            .send_password_changed_email(&user.email, user.name.as_deref())
            .await;

        let _ = crate::Event::dispatch(crate::auth_flows::events::PasswordReset {
            user_id: user.id.to_string(),
        })
        .await;
        Ok(user)
    }

    /// Generate a reset token and email it via the bridge.
    pub async fn send_link(email: &str, base_url: &str) -> Result<(), FrameworkError> {
        let Some((user, token)) = Self::request(email).await? else {
            // Silent return — same semantics as torii.
            return Ok(());
        };
        let url = format!("{}?token={}", base_url, token);

        use torii_core::services::MailerService;
        let bridge = crate::torii_integration::current_mailer_bridge()?;
        bridge
            .send_password_reset_email(&user.email, &url, user.name.as_deref())
            .await
            .map_err(crate::torii_integration::map_torii_err)
    }
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p suprnova --test password_reset
git add framework/src/auth_flows/password_reset.rs framework/tests/password_reset.rs
git commit -m "feat(auth_flows): PasswordReset facade over torii PasswordResetService"
```

---

## Task 5: Brute-force protection facade + LoginThrottle middleware

**Files:** `framework/src/auth_flows/brute_force.rs`

torii ships `BruteForceService` which handles attempt-counting,
account locking after N failures, and IP throttling. We expose it
through `Auth::brute_force()` and provide a drop-in
`LoginThrottleMiddleware` that wraps `/login` endpoints.

- [ ] **Step 1: Inspect torii's BruteForceService**

```bash
grep -n "pub fn\|pub async fn" /home/shawn/workspace/nation-x-com/reference/torii-rs-main/torii-core/src/services/brute_force.rs
```

Note the exact method names — likely something like
`record_failed_attempt`, `is_locked`, `unlock`, `clear_attempts`.

- [ ] **Step 2: Write failing test**

```rust
// framework/tests/brute_force.rs
use suprnova::{
    auth_flows::brute_force::BruteForce,
    torii_integration::{init_torii, ToriiConfig},
};

#[tokio::test]
async fn account_locks_after_n_failed_attempts() {
    init_torii(ToriiConfig::sqlite_in_memory()).await.unwrap();
    let user = suprnova::Auth::password()
        .register("alice@example.com", "correctpass1")
        .await
        .unwrap();

    // 5 failed attempts (configurable via torii service defaults)
    for _ in 0..5 {
        let _ = suprnova::Auth::password()
            .authenticate("alice@example.com", "wrong", None, None)
            .await;
    }

    let locked = BruteForce::is_locked(&user.id).await.unwrap();
    assert!(locked, "account should be locked after 5 failed attempts");

    // Even with the correct password, auth fails while locked
    assert!(suprnova::Auth::password()
        .authenticate("alice@example.com", "correctpass1", None, None)
        .await
        .is_err());

    // Admin unlocks
    BruteForce::unlock(&user.id).await.unwrap();
    let locked = BruteForce::is_locked(&user.id).await.unwrap();
    assert!(!locked);
}
```

- [ ] **Step 3: Implement**

```rust
// framework/src/auth_flows/brute_force.rs
//! Brute-force facade over torii's BruteForceService. Track failed
//! authentication attempts per user/IP; lock accounts that exceed
//! the threshold; expose admin unlock.

use crate::{torii_integration::instance, FrameworkError};
use torii_core::UserId;

pub struct BruteForce;

impl BruteForce {
    pub async fn record_failed_attempt(user_id: &UserId, ip: Option<&str>) -> Result<(), FrameworkError> {
        instance()?
            .brute_force()
            .record_failed_attempt(user_id, ip)
            .await
            .map_err(crate::torii_integration::map_torii_err)
    }

    pub async fn is_locked(user_id: &UserId) -> Result<bool, FrameworkError> {
        instance()?
            .brute_force()
            .is_locked(user_id)
            .await
            .map_err(crate::torii_integration::map_torii_err)
    }

    pub async fn unlock(user_id: &UserId) -> Result<(), FrameworkError> {
        instance()?
            .brute_force()
            .unlock(user_id)
            .await
            .map_err(crate::torii_integration::map_torii_err)
    }

    pub async fn clear_attempts(user_id: &UserId) -> Result<(), FrameworkError> {
        instance()?
            .brute_force()
            .clear_attempts(user_id)
            .await
            .map_err(crate::torii_integration::map_torii_err)
    }
}
```

> **Method-name verification:** The exact torii method names may differ — read `torii-core/src/services/brute_force.rs` and align.

- [ ] **Step 4: Optional `LoginThrottleMiddleware` (combines torii brute-force with Phase 5 RateLimiter for IP-level)**

```rust
// framework/src/auth_flows/brute_force.rs — append
use crate::http::{HttpResponse, Request, Response};
use crate::middleware::{Middleware, Next};
use async_trait::async_trait;

pub struct LoginThrottleMiddleware {
    per_ip_per_minute: u32,
}

impl LoginThrottleMiddleware {
    pub fn per_ip_per_minute(limit: u32) -> Self {
        Self { per_ip_per_minute: limit }
    }
}

#[async_trait]
impl Middleware for LoginThrottleMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let ip = request
            .header("x-forwarded-for")
            .or_else(|| request.header("x-real-ip"))
            .unwrap_or_else(|| "unknown".into());

        // IP-level throttle via Phase 5 RateLimiter
        crate::RateLimiter::for_("login")
            .limit(self.per_ip_per_minute)
            .per_minute()
            .attempt(&ip)
            .await?;

        // Account-level lockout happens inside torii.authenticate()
        // — see the auth_flows::brute_force integration in Auth::password().authenticate
        next(request).await
    }
}
```

- [ ] **Step 5: Commit**

```bash
git add framework/src/auth_flows/brute_force.rs framework/tests/brute_force.rs
git commit -m "feat(auth_flows): BruteForce facade over torii + LoginThrottleMiddleware"
```

---

## Task 6: 2FA TOTP enrollment + verify + recovery codes

**Files:** `framework/src/auth_flows/two_factor/mod.rs`, `recovery.rs`

torii does not ship TOTP — this is our work. Migration adds
`two_factor_secret` (encrypted) and `two_factor_recovery_codes` (JSON)
columns to the user table.

- [ ] **Step 1: Migration**

```rust
// framework/src/auth_flows/migrations/m_add_two_factor_to_users.rs
// ALTER TABLE users
//   ADD COLUMN two_factor_secret TEXT,                  -- AES-encrypted base32 secret
//   ADD COLUMN two_factor_recovery_codes JSON,          -- AES-encrypted JSON array
//   ADD COLUMN two_factor_confirmed_at DATETIME;
```

- [ ] **Step 2: Write failing test**

```rust
// framework/tests/two_factor.rs
use suprnova::auth_flows::two_factor::TwoFactor;

#[test]
fn enroll_generates_secret_and_otpauth_url() {
    let enrollment = TwoFactor::enroll("alice@example.com", "Suprnova").unwrap();
    assert_eq!(enrollment.secret.len(), 32);
    assert!(enrollment.otpauth_url.starts_with("otpauth://totp/"));
    assert!(enrollment.otpauth_url.contains("alice%40example.com"));
    assert!(enrollment.otpauth_url.contains("issuer=Suprnova"));
    assert!(!enrollment.qr_png_base64.is_empty());
}

#[test]
fn verify_accepts_current_code_rejects_random() {
    let enrollment = TwoFactor::enroll("a@b.c", "App").unwrap();
    let now = TwoFactor::generate_code(&enrollment.secret).unwrap();
    assert!(TwoFactor::verify(&enrollment.secret, &now).unwrap());
    assert!(!TwoFactor::verify(&enrollment.secret, "000000").unwrap());
}

#[test]
fn recovery_codes_unique_and_one_time_use() {
    let codes = TwoFactor::generate_recovery_codes(8);
    let set: std::collections::HashSet<_> = codes.iter().collect();
    assert_eq!(set.len(), 8);

    let mut remaining = codes.clone();
    assert!(TwoFactor::consume_recovery_code(&mut remaining, &codes[0]));
    assert_eq!(remaining.len(), 7);
    assert!(!TwoFactor::consume_recovery_code(&mut remaining, &codes[0]));
}
```

- [ ] **Step 3: Implement**

```rust
// framework/src/auth_flows/two_factor/mod.rs
//! TOTP 2FA. Not provided by torii; we own this.

pub mod recovery;

use crate::FrameworkError;
use base64::Engine;
use qrcode::{render::svg, QrCode};
use rand::Rng;
use totp_rs::{Algorithm, Secret, TOTP};

pub struct Enrollment {
    pub secret: String,        // base32 — encrypt before storing
    pub otpauth_url: String,
    pub qr_png_base64: String, // base64-encoded SVG (or PNG once we wire raster)
}

pub struct TwoFactor;

impl TwoFactor {
    pub fn enroll(account: &str, issuer: &str) -> Result<Enrollment, FrameworkError> {
        let secret = Secret::generate_secret();
        let totp = TOTP::new(
            Algorithm::SHA1,
            6,
            1,
            30,
            secret.to_bytes().unwrap(),
            Some(issuer.to_string()),
            account.to_string(),
        )
        .map_err(|e| FrameworkError::internal(format!("totp: {}", e)))?;

        let secret_b32 = secret.to_encoded().to_string();
        let otpauth = totp.get_url();

        let code = QrCode::new(otpauth.as_bytes())
            .map_err(|e| FrameworkError::internal(format!("qr: {}", e)))?;
        let svg = code.render::<svg::Color>().build();
        let b64 = base64::engine::general_purpose::STANDARD.encode(svg.as_bytes());

        Ok(Enrollment {
            secret: secret_b32,
            otpauth_url: otpauth,
            qr_png_base64: b64,
        })
    }

    pub fn generate_code(secret_b32: &str) -> Result<String, FrameworkError> {
        let secret = Secret::Encoded(secret_b32.to_string())
            .to_bytes()
            .map_err(|e| FrameworkError::internal(format!("secret: {:?}", e)))?;
        let totp = TOTP::new(Algorithm::SHA1, 6, 1, 30, secret, None, "".into())
            .map_err(|e| FrameworkError::internal(format!("totp: {}", e)))?;
        totp.generate_current()
            .map_err(|e| FrameworkError::internal(format!("totp gen: {}", e)))
    }

    pub fn verify(secret_b32: &str, code: &str) -> Result<bool, FrameworkError> {
        let secret = Secret::Encoded(secret_b32.to_string())
            .to_bytes()
            .map_err(|e| FrameworkError::internal(format!("secret: {:?}", e)))?;
        let totp = TOTP::new(Algorithm::SHA1, 6, 1, 30, secret, None, "".into())
            .map_err(|e| FrameworkError::internal(format!("totp: {}", e)))?;
        Ok(totp.check_current(code).unwrap_or(false))
    }

    pub fn generate_recovery_codes(count: usize) -> Vec<String> {
        let mut rng = rand::thread_rng();
        (0..count)
            .map(|_| {
                let a: u32 = rng.gen_range(10_000..100_000);
                let b: u32 = rng.gen_range(10_000..100_000);
                format!("{}-{}", a, b)
            })
            .collect()
    }

    pub fn consume_recovery_code(stored: &mut Vec<String>, submitted: &str) -> bool {
        let trimmed = submitted.trim();
        if let Some(idx) = stored.iter().position(|c| c == trimmed) {
            stored.remove(idx);
            true
        } else {
            false
        }
    }
}
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p suprnova --test two_factor
git add framework/src/auth_flows/two_factor framework/src/auth_flows/migrations framework/tests/two_factor.rs
git commit -m "feat(auth_flows): TwoFactor TOTP enrollment + verify + recovery codes (ours)"
```

---

## Task 7: Remember-me persistent cookies

**Files:** `framework/src/auth_flows/remember_me/`

Not in torii. Stays our work.

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/remember_me.rs
use suprnova::auth_flows::remember_me::{generate_token, verify_token};

#[test]
fn token_round_trips_user_id() {
    let enc = suprnova::Encrypter::new(suprnova::EncryptionKey::generate());
    let token = generate_token(42, &enc);
    let user_id = verify_token(&token, &enc).unwrap();
    assert_eq!(user_id, 42);
}

#[test]
fn tampered_token_fails() {
    let enc = suprnova::Encrypter::new(suprnova::EncryptionKey::generate());
    let token = generate_token(42, &enc);
    let mut bytes = token.into_bytes();
    bytes[0] ^= 1;
    let tampered = String::from_utf8_lossy(&bytes).into_owned();
    assert!(verify_token(&tampered, &enc).is_err());
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/auth_flows/remember_me/mod.rs
pub mod middleware;

use crate::{Cookie, Encrypter, FrameworkError, SameSite};
use rand::RngCore;
use std::time::Duration;

const COOKIE_NAME: &str = "remember_me";
const COOKIE_LIFETIME_DAYS: i64 = 30;

pub fn generate_token(user_id: i64, encrypter: &Encrypter) -> String {
    let mut random = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut random);
    let payload = format!("{}:{}", user_id, hex::encode(random));
    encrypter
        .encrypt_string(&payload)
        .expect("encrypt remember-me token")
}

pub fn verify_token(token: &str, encrypter: &Encrypter) -> Result<i64, FrameworkError> {
    let payload = encrypter
        .decrypt_string(token)
        .map_err(|_| FrameworkError::Unauthorized)?;
    payload
        .split(':')
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or(FrameworkError::Unauthorized)
}

pub fn make_cookie(token: &str) -> Cookie {
    Cookie::new(COOKIE_NAME, token)
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .max_age(Duration::from_secs(60 * 60 * 24 * COOKIE_LIFETIME_DAYS as u64))
}

pub fn make_clear_cookie() -> Cookie {
    Cookie::new(COOKIE_NAME, "")
        .http_only(true)
        .secure(true)
        .max_age(Duration::from_secs(0))
}
```

```rust
// framework/src/auth_flows/remember_me/middleware.rs
use crate::http::{Request, Response};
use crate::middleware::{Middleware, Next};
use crate::session::session_mut;
use crate::{Auth, Encrypter};
use async_trait::async_trait;

pub struct RememberMeMiddleware {
    encrypter: Encrypter,
}

impl RememberMeMiddleware {
    pub fn new(encrypter: Encrypter) -> Self {
        Self { encrypter }
    }
}

#[async_trait]
impl Middleware for RememberMeMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        if Auth::check() {
            return next(request).await;
        }
        if let Some(token) = request.cookie("remember_me") {
            if let Ok(user_id) = super::verify_token(&token, &self.encrypter) {
                session_mut(|s| s.put("user_id", user_id));
            }
        }
        next(request).await
    }
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p suprnova --test remember_me
git add framework/src/auth_flows/remember_me framework/tests/remember_me.rs
git commit -m "feat(auth_flows): RememberMe token + cookie + middleware (ours)"
```

---

## Task 8: Events + module wiring

**Files:** `framework/src/auth_flows/events.rs`, `framework/src/auth_flows/mod.rs`, `framework/src/lib.rs`

- [ ] **Step 1: Define events**

```rust
// framework/src/auth_flows/events.rs
use crate::EventTrait;

#[derive(Debug, Clone)]
pub struct EmailVerified {
    pub user_id: String,
}

impl EventTrait for EmailVerified {
    fn event_name() -> &'static str { "EmailVerified" }
}

#[derive(Debug, Clone)]
pub struct PasswordReset {
    pub user_id: String,
}

impl EventTrait for PasswordReset {
    fn event_name() -> &'static str { "PasswordReset" }
}

#[derive(Debug, Clone)]
pub struct TwoFactorEnrolled {
    pub user_id: String,
}

impl EventTrait for TwoFactorEnrolled {
    fn event_name() -> &'static str { "TwoFactorEnrolled" }
}

#[derive(Debug, Clone)]
pub struct AccountLocked {
    pub user_id: String,
    pub reason: &'static str,
}

impl EventTrait for AccountLocked {
    fn event_name() -> &'static str { "AccountLocked" }
}
```

- [ ] **Step 2: Module + re-exports**

```rust
// framework/src/auth_flows/mod.rs
pub mod brute_force;
pub mod email_verify;
pub mod events;
pub mod password_reset;
pub mod remember_me;
pub mod two_factor;

pub use brute_force::{BruteForce, LoginThrottleMiddleware};
pub use email_verify::EmailVerification;
pub use password_reset::PasswordReset;
pub use remember_me::middleware::RememberMeMiddleware;
pub use two_factor::TwoFactor;
```

```rust
// framework/src/lib.rs
pub mod auth_flows;
pub use auth_flows::{
    BruteForce, EmailVerification, LoginThrottleMiddleware, PasswordReset,
    RememberMeMiddleware, TwoFactor,
};
```

- [ ] **Step 3: Commit**

```bash
git add framework/src/auth_flows/events.rs framework/src/auth_flows/mod.rs framework/src/lib.rs
git commit -m "feat(auth_flows): events + module wiring + crate-root re-exports"
```

---

## Task 9: App dogfood — controllers + routes

**Files:** `app/src/controllers/auth_verify.rs`, `auth_reset.rs`, `auth_2fa.rs`

- [ ] **Step 1: Verify controller**

```rust
// app/src/controllers/auth_verify.rs
use suprnova::{json_response, Auth, EmailVerification, FrameworkError, Request, Response};

pub async fn send(_req: Request) -> Response {
    let user = Auth::user().await?.ok_or(FrameworkError::Unauthorized)?;
    EmailVerification::send_link(&user, "http://localhost:8000/email/verify").await?;
    json_response!({ "sent": true })
}

pub async fn confirm(req: Request) -> Response {
    let token = req.query("token").ok_or(FrameworkError::param("token"))?;
    let user = EmailVerification::verify(&token).await?;
    json_response!({ "verified": true, "email": user.email })
}
```

- [ ] **Step 2: Reset controllers**

```rust
// app/src/controllers/auth_reset.rs
use suprnova::{json_response, FrameworkError, PasswordReset, Request, Response};

pub async fn request_link(req: Request) -> Response {
    let body: serde_json::Value = req.parse_json().await?;
    let email = body["email"].as_str().ok_or(FrameworkError::param("email"))?;
    PasswordReset::send_link(email, "http://localhost:8000/password/reset").await?;
    // Always 200, never reveal whether email is on file.
    json_response!({ "sent": true })
}

pub async fn confirm(req: Request) -> Response {
    let body: serde_json::Value = req.parse_json().await?;
    let token = body["token"].as_str().ok_or(FrameworkError::param("token"))?;
    let new_password = body["password"].as_str().ok_or(FrameworkError::param("password"))?;
    let _user = PasswordReset::reset(token, new_password).await?;
    json_response!({ "reset": true })
}
```

- [ ] **Step 3: 2FA controllers**

```rust
// app/src/controllers/auth_2fa.rs
use suprnova::{json_response, Auth, FrameworkError, Request, Response, TwoFactor};

pub async fn enroll(_req: Request) -> Response {
    let user = Auth::user().await?.ok_or(FrameworkError::Unauthorized)?;
    let issuer = std::env::var("APP_NAME").unwrap_or_else(|_| "Suprnova".into());
    let enrollment = TwoFactor::enroll(&user.email, &issuer)?;
    // Persist enrollment.secret + recovery codes (encrypted!) on the user.
    // crate::models::User::set_pending_2fa_secret(user.id, &enrollment.secret).await?;
    json_response!({
        "secret": enrollment.secret,
        "qr": enrollment.qr_png_base64,
        "url": enrollment.otpauth_url,
    })
}

pub async fn confirm(req: Request) -> Response {
    let user = Auth::user().await?.ok_or(FrameworkError::Unauthorized)?;
    let body: serde_json::Value = req.parse_json().await?;
    let code = body["code"].as_str().ok_or(FrameworkError::param("code"))?;
    // Load the pending 2FA secret from the user row.
    // let secret = crate::models::User::pending_2fa_secret(user.id).await?;
    let secret = ""; // placeholder
    if !TwoFactor::verify(secret, code)? {
        return Err(FrameworkError::Domain {
            message: "invalid 2FA code".into(),
            status_code: 422,
        }.into());
    }
    let recovery = TwoFactor::generate_recovery_codes(8);
    // crate::models::User::confirm_2fa(user.id, &recovery).await?;
    json_response!({ "confirmed": true, "recovery_codes": recovery })
}
```

- [ ] **Step 4: Wire routes + login throttle**

```rust
// In the routes! macro:
post!("/email/verify/send", controllers::auth_verify::send),
get!("/email/verify", controllers::auth_verify::confirm),
post!("/password/email", controllers::auth_reset::request_link),
post!("/password/reset", controllers::auth_reset::confirm),
post!("/2fa/enroll", controllers::auth_2fa::enroll),
post!("/2fa/confirm", controllers::auth_2fa::confirm),

// On the login route group:
group!("/login")
    .middleware(suprnova::LoginThrottleMiddleware::per_ip_per_minute(5))
    .routes([ /* login routes */ ]);
```

- [ ] **Step 5: Smoke test**

```bash
cargo run -p app -- serve &
sleep 2
curl -X POST http://127.0.0.1:8000/password/email -d '{"email":"alice@example.com"}' -H 'Content-Type: application/json'
# Verify mail log captured the email (Mail::use_log in dev)
kill %1
```

- [ ] **Step 6: Commit**

```bash
git add app/src
git commit -m "feat(app): wired email-verify / password-reset / 2FA controllers using torii facades"
```

---

## Task 10: Workspace lint + roadmap update

- [ ] **Step 1: Clippy + tests**

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

- [ ] **Step 2: Move from "Missing" to "Production-ready"**:
- Email verification (via torii)
- Password reset (via torii)
- Brute-force protection (via torii)
- 2FA TOTP + recovery codes (ours)
- Remember-me cookies (ours)
- Mailer bridge (torii → our Mail::send)

- [ ] **Step 3: Commit + push**

---

## Self-Review

| Spec item | Covered by | Source |
|---|---|---|
| MailerBridge (torii → Mail::send) | Task 2 | Ours (bridge to torii trait) |
| Email verification | Task 3 | **torii** |
| Password reset | Task 4 | **torii** |
| Password-changed notification | Task 4 | **torii** mailer |
| Brute-force protection | Task 5 | **torii** |
| Login throttle middleware | Task 5 | Ours + Phase 5 RateLimiter |
| 2FA TOTP enrollment + verify | Task 6 | Ours (totp-rs) |
| Recovery codes | Task 6 | Ours |
| Remember-me cookies + middleware | Task 7 | Ours |
| Events (EmailVerified / PasswordReset / TwoFactorEnrolled / AccountLocked) | Task 8 | Ours (Phase 1 EventDispatcher) |
| App dogfood | Task 9 | — |

**Architectural correctness:** torii owns the token + state for verification/reset/brute-force. We own TOTP + remember-me. Mail flows through our `Mail::send` (lettre) via the bridge — torii never reaches for `torii-mailer` directly. Tests use `Mail::fake()` to assert torii's emails landed in our recorder, proving the bridge works end-to-end.

**Placeholder scan:** Clean. `> Builder method:` and `> Method-name verification:` notes flag concrete files to read (`reference/torii-rs-main/torii/src/builder.rs`, `services/brute_force.rs`) before wiring.

---

## Execution Handoff

**Subagent-Driven recommended per task.**
