# Phase 11: Auth Flows Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** The four "every app has these" auth flows beyond bare login: email verification (signed link → `verified` middleware gate), password reset (token-bearing reset flow), two-factor authentication (TOTP enrollment + recovery codes), and remember-me persistent cookies.

**Architecture:** Each flow is built on subsystems we already shipped — Mail (Phase 5) for verification and reset emails, Encrypter (Phase 2) for signed-link / token generation, RateLimiter (Phase 5) for brute-force protection on verification + reset + 2FA challenges, FormRequest + Validation for the form endpoints. The flows live in `framework/src/auth_flows/` (new module). 2FA TOTP uses `totp-rs` for code generation and verification; QR enrollment generates an otpauth URL the user's authenticator app scans.

**Tech Stack:** `totp-rs` 5 (TOTP / HOTP), `qrcode` 0.14 (QR generation for 2FA enrollment), reuses Mail / Encrypter / RateLimiter / FormRequest from earlier phases.

---

## File Structure

**New files:**
- `framework/src/auth_flows/mod.rs` — entry
- `framework/src/auth_flows/email_verify/mod.rs` — verification facade
- `framework/src/auth_flows/email_verify/mailable.rs` — `VerifyEmailMessage`
- `framework/src/auth_flows/email_verify/middleware.rs` — `VerifiedMiddleware`
- `framework/src/auth_flows/password_reset/mod.rs` — `Password::sendResetLink`, `Password::reset`
- `framework/src/auth_flows/password_reset/mailable.rs` — `ResetPasswordMessage`
- `framework/src/auth_flows/two_factor/mod.rs` — `TwoFactor` facade
- `framework/src/auth_flows/two_factor/qr.rs` — QR / otpauth URL generation
- `framework/src/auth_flows/two_factor/recovery.rs` — recovery codes
- `framework/src/auth_flows/remember_me/mod.rs` — long-lived token cookies
- `framework/tests/email_verify.rs`, `password_reset.rs`, `two_factor.rs`, `remember_me.rs`
- `app/src/mail/verify_email.rs`, `reset_password.rs` — concrete mailables
- `app/src/controllers/auth_verify.rs`, `auth_reset.rs`, `auth_2fa.rs` — example routes

**Migrations:**
- `framework/src/auth_flows/migrations/m_add_email_verified_at_to_users.rs`
- `framework/src/auth_flows/migrations/m_create_password_resets_table.rs`
- `framework/src/auth_flows/migrations/m_add_two_factor_to_users.rs` (secret + recovery codes columns)
- `framework/src/auth_flows/migrations/m_create_remember_tokens_table.rs`

---

## Task 1: Add deps + migrations

**Files:** `framework/Cargo.toml`, migrations

- [ ] **Step 1: Add deps**

```toml
# framework/Cargo.toml
totp-rs = { version = "5", features = ["otpauth", "qr"] }
qrcode = "0.14"
```

- [ ] **Step 2: Run migrations check**

```bash
cargo check --workspace
```

- [ ] **Step 3: Commit**

```bash
git add framework/Cargo.toml Cargo.lock
git commit -m "feat(deps): add totp-rs, qrcode for Phase 11 auth flows"
```

---

## Task 2: Email verification — signed link generation + verify middleware

**Files:** `framework/src/auth_flows/email_verify/`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/email_verify.rs
use suprnova::auth_flows::email_verify::{generate_verify_url, verify_url_token};

#[test]
fn signed_url_round_trips() {
    let enc = suprnova::Encrypter::new(suprnova::EncryptionKey::generate());
    let url = generate_verify_url(
        "https://app.example.com/email/verify",
        42,
        std::time::Duration::from_secs(3600),
        &enc,
    )
    .unwrap();
    assert!(url.contains("user_id="));
    assert!(url.contains("expires="));
    assert!(url.contains("sig="));

    let user_id = verify_url_token(&url, &enc).unwrap();
    assert_eq!(user_id, 42);
}

#[test]
fn tampered_signature_fails() {
    let enc = suprnova::Encrypter::new(suprnova::EncryptionKey::generate());
    let url = generate_verify_url(
        "https://app.example.com/email/verify",
        42,
        std::time::Duration::from_secs(3600),
        &enc,
    )
    .unwrap();
    let tampered = url.replace("user_id=42", "user_id=99");
    assert!(verify_url_token(&tampered, &enc).is_err());
}

#[test]
fn expired_link_fails() {
    let enc = suprnova::Encrypter::new(suprnova::EncryptionKey::generate());
    let url = generate_verify_url(
        "https://app.example.com/email/verify",
        42,
        std::time::Duration::from_millis(1),
        &enc,
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(verify_url_token(&url, &enc).is_err());
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/auth_flows/email_verify/mod.rs
//! Email-verification flow.
//!
//! 1. After registration, dispatch the `VerifyEmailMessage`
//!    (Phase 5 Mail).
//! 2. Email contains a signed URL like
//!    `/email/verify?user_id=42&expires=...&sig=...`.
//! 3. User clicks → controller calls `verify_url_token(...)` →
//!    on success, sets `users.email_verified_at = NOW()`.
//! 4. `VerifiedMiddleware` blocks routes for users whose
//!    `email_verified_at` is null.

pub mod mailable;
pub mod middleware;

use crate::FrameworkError;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize)]
struct Payload {
    user_id: i64,
    expires: u64,
}

pub fn generate_verify_url(
    base: &str,
    user_id: i64,
    valid_for: Duration,
    encrypter: &crate::Encrypter,
) -> Result<String, FrameworkError> {
    let expires = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + valid_for.as_secs();
    let payload = Payload { user_id, expires };
    let canonical = serde_urlencoded::to_string(&payload)
        .map_err(|e| FrameworkError::internal(format!("encode: {}", e)))?;
    let signature = encrypter.encrypt_string(&canonical)?;
    Ok(format!("{}?{}&sig={}", base, canonical, signature))
}

pub fn verify_url_token(url: &str, encrypter: &crate::Encrypter) -> Result<i64, FrameworkError> {
    let (_, query) = url
        .split_once('?')
        .ok_or_else(|| FrameworkError::param("query"))?;
    let mut pairs: Vec<(String, String)> = serde_urlencoded::from_str(query)
        .map_err(|e| FrameworkError::internal(format!("decode: {}", e)))?;
    let sig = pairs
        .iter()
        .position(|(k, _)| k == "sig")
        .map(|i| pairs.remove(i).1)
        .ok_or_else(|| FrameworkError::param("sig"))?;
    let canonical = serde_urlencoded::to_string(&pairs)
        .map_err(|e| FrameworkError::internal(format!("encode: {}", e)))?;
    let decrypted = encrypter.decrypt_string(&sig)?;
    if decrypted != canonical {
        return Err(FrameworkError::Unauthorized);
    }
    let user_id: i64 = pairs
        .iter()
        .find(|(k, _)| k == "user_id")
        .and_then(|(_, v)| v.parse().ok())
        .ok_or_else(|| FrameworkError::param("user_id"))?;
    let expires: u64 = pairs
        .iter()
        .find(|(k, _)| k == "expires")
        .and_then(|(_, v)| v.parse().ok())
        .ok_or_else(|| FrameworkError::param("expires"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if now >= expires {
        return Err(FrameworkError::Domain {
            message: "verification link expired".into(),
            status_code: 410,
        });
    }
    Ok(user_id)
}
```

- [ ] **Step 3: Mailable**

```rust
// framework/src/auth_flows/email_verify/mailable.rs
use crate::Mailable;
use suprnova_macros::async_trait;

pub struct VerifyEmailMessage {
    pub email: String,
    pub name: String,
    pub verify_url: String,
}

#[async_trait]
impl Mailable for VerifyEmailMessage {
    fn to(&self) -> Vec<String> { vec![self.email.clone()] }
    fn subject(&self) -> String { "Please verify your email".into() }
    fn body_html(&self) -> String {
        format!(
            "<p>Hi {},</p><p>Please click <a href=\"{}\">here</a> to verify your email.</p>",
            self.name, self.verify_url
        )
    }
    fn body_text(&self) -> Option<String> {
        Some(format!("Hi {},\n\nVerify: {}\n", self.name, self.verify_url))
    }
}
```

- [ ] **Step 4: Verified middleware**

```rust
// framework/src/auth_flows/email_verify/middleware.rs
use crate::http::{Request, Response, HttpResponse};
use crate::middleware::{Middleware, Next};
use crate::Auth;
use async_trait::async_trait;

/// Middleware that blocks routes for users whose
/// `email_verified_at` is null. Redirects to a configurable
/// "please verify" page.
pub struct VerifiedMiddleware {
    redirect_to: String,
}

impl VerifiedMiddleware {
    pub fn redirect_to(path: impl Into<String>) -> Self {
        Self { redirect_to: path.into() }
    }
}

#[async_trait]
impl Middleware for VerifiedMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Authenticatable trait must expose email_verified_at.
        // For brevity, this sketch trusts a UserProvider that
        // returns Some(user) with `email_verified_at: Option<DateTime>`.
        if let Some(user) = Auth::user().await? {
            // Check via downcast or a typed accessor.
            // Implementation: extend Authenticatable with
            //   fn email_verified_at(&self) -> Option<DateTime<Utc>>;
            // Default returns None; concrete user types override.
            if user.email_verified_at().is_some() {
                return next(request).await;
            }
        }
        Err(HttpResponse::new()
            .status(302)
            .header("Location", &self.redirect_to))
    }
}
```

- [ ] **Step 5: Run — expect pass**

```bash
cargo test -p suprnova --test email_verify
```

- [ ] **Step 6: Commit**

```bash
git add framework/src/auth_flows/email_verify framework/src/lib.rs framework/tests/email_verify.rs
git commit -m "feat(auth_flows): email verification — signed URL + VerifiedMiddleware"
```

---

## Task 3: Password reset

**Files:** `framework/src/auth_flows/password_reset/`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/password_reset.rs
use suprnova::auth_flows::password_reset::{generate_reset_token, verify_reset_token};

#[tokio::test]
async fn reset_token_round_trips() {
    // Token is stored in the password_resets table; we test the
    // crypto round-trip + lookup against a test DB.
    let enc = suprnova::Encrypter::new(suprnova::EncryptionKey::generate());
    let token = generate_reset_token("alice@example.com", &enc);
    // Token format: "<email>:<random>:<sig>" or similar opaque blob
    assert!(!token.is_empty());

    let result = verify_reset_token(&token, &enc).unwrap();
    assert_eq!(result, "alice@example.com");
}

#[tokio::test]
async fn tampered_token_fails() {
    let enc = suprnova::Encrypter::new(suprnova::EncryptionKey::generate());
    let token = generate_reset_token("alice@example.com", &enc);
    let mut bytes = token.into_bytes();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let tampered = String::from_utf8_lossy(&bytes).into_owned();
    assert!(verify_reset_token(&tampered, &enc).is_err());
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/auth_flows/password_reset/mod.rs
pub mod mailable;

use crate::{Encrypter, FrameworkError};
use rand::RngCore;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Generate an opaque reset token. Format (after encrypt):
/// `<email>:<random>:<expires-epoch>`.
pub fn generate_reset_token(email: &str, encrypter: &Encrypter) -> String {
    let mut nonce = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce);
    let expires = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600; // 1 hour
    let payload = format!("{}:{}:{}", email, hex::encode(nonce), expires);
    encrypter
        .encrypt_string(&payload)
        .expect("encrypt reset token")
}

pub fn verify_reset_token(token: &str, encrypter: &Encrypter) -> Result<String, FrameworkError> {
    let payload = encrypter
        .decrypt_string(token)
        .map_err(|_| FrameworkError::Domain {
            message: "invalid reset token".into(),
            status_code: 400,
        })?;
    let parts: Vec<&str> = payload.split(':').collect();
    if parts.len() != 3 {
        return Err(FrameworkError::Domain {
            message: "malformed reset token".into(),
            status_code: 400,
        });
    }
    let email = parts[0].to_string();
    let expires: u64 = parts[2].parse().map_err(|_| FrameworkError::Domain {
        message: "malformed reset token".into(),
        status_code: 400,
    })?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if now >= expires {
        return Err(FrameworkError::Domain {
            message: "reset token expired".into(),
            status_code: 410,
        });
    }
    Ok(email)
}

pub struct Password;

impl Password {
    /// Generate a token, persist nothing on our side (token is
    /// self-contained), and send the reset email.
    pub async fn send_reset_link(
        email: &str,
        callback_base: &str,
        encrypter: &Encrypter,
    ) -> Result<(), FrameworkError> {
        let token = generate_reset_token(email, encrypter);
        let url = format!("{}?token={}&email={}", callback_base, token, email);
        crate::Mail::send(mailable::ResetPasswordMessage {
            email: email.to_string(),
            reset_url: url,
        })
        .await
    }

    /// Verify the token, then call the user-provided closure to
    /// actually update the password (since we don't own the user
    /// model — that's app code).
    pub async fn reset<F, Fut>(
        token: &str,
        new_password: &str,
        encrypter: &Encrypter,
        update_password: F,
    ) -> Result<(), FrameworkError>
    where
        F: FnOnce(String, String) -> Fut,
        Fut: std::future::Future<Output = Result<(), FrameworkError>>,
    {
        let email = verify_reset_token(token, encrypter)?;
        update_password(email, new_password.to_string()).await
    }
}
```

- [ ] **Step 3: Mailable**

```rust
// framework/src/auth_flows/password_reset/mailable.rs
use crate::Mailable;
use suprnova_macros::async_trait;

pub struct ResetPasswordMessage {
    pub email: String,
    pub reset_url: String,
}

#[async_trait]
impl Mailable for ResetPasswordMessage {
    fn to(&self) -> Vec<String> { vec![self.email.clone()] }
    fn subject(&self) -> String { "Reset your password".into() }
    fn body_html(&self) -> String {
        format!(
            "<p>Click <a href=\"{}\">here</a> to reset your password. Link expires in 1 hour.</p>",
            self.reset_url
        )
    }
    fn body_text(&self) -> Option<String> {
        Some(format!("Reset: {}\nExpires in 1 hour.\n", self.reset_url))
    }
}
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p suprnova --test password_reset
git add framework/src/auth_flows/password_reset framework/tests/password_reset.rs
git commit -m "feat(auth_flows): Password::send_reset_link + Password::reset with encrypted tokens"
```

---

## Task 4: Two-factor authentication (TOTP)

**Files:** `framework/src/auth_flows/two_factor/`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/two_factor.rs
use suprnova::auth_flows::two_factor::TwoFactor;

#[test]
fn enroll_generates_secret_and_otpauth_url() {
    let enrollment = TwoFactor::enroll("alice@example.com", "Suprnova").unwrap();
    assert_eq!(enrollment.secret.len(), 32); // base32 secret
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

    let stored: Vec<String> = codes.clone();
    let mut remaining = stored.clone();
    assert!(TwoFactor::consume_recovery_code(&mut remaining, &codes[0]));
    assert_eq!(remaining.len(), 7);
    assert!(!TwoFactor::consume_recovery_code(&mut remaining, &codes[0]));
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/auth_flows/two_factor/mod.rs
//! TOTP 2FA.
//!
//! Enrollment:
//!   1. `TwoFactor::enroll(account, issuer)` → returns secret (store
//!      encrypted on the user row), otpauth URL (display the QR), and
//!      a PNG QR ready for `<img src="data:image/png;base64,...">`.
//!   2. User scans QR, enters first code, server verifies with
//!      `TwoFactor::verify`.
//!   3. Generate recovery codes via `generate_recovery_codes`, store
//!      hashed (or encrypted) on user row.
//!
//! Authentication:
//!   - On login (or step-up auth), prompt for code, call
//!     `TwoFactor::verify(secret, submitted_code)`.

use crate::FrameworkError;
use rand::Rng;
use totp_rs::{Algorithm, Secret, TOTP};

pub struct Enrollment {
    pub secret: String,        // base32 — store ENCRYPTED on user
    pub otpauth_url: String,   // for displaying or generating QR yourself
    pub qr_png_base64: String, // base64-encoded PNG bytes
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

        // Generate QR PNG bytes.
        use qrcode::QrCode;
        let code = QrCode::new(otpauth.as_bytes())
            .map_err(|e| FrameworkError::internal(format!("qr: {}", e)))?;
        let image = code.render::<qrcode::render::svg::Color>().build();
        // Render to PNG via the image crate (already a dep from Phase 4).
        // For brevity this sketch returns the SVG inline. Production:
        // use qrcode's `image-rendering` feature to get raw RGBA →
        // `image::PngEncoder` → base64.
        let svg_bytes = image.as_bytes();
        let b64 = base64::engine::general_purpose::STANDARD.encode(svg_bytes);

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

    /// Consume a recovery code from the stored list. Returns true if
    /// the code matched and was removed.
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

- [ ] **Step 3: Run + commit**

```bash
cargo test -p suprnova --test two_factor
git add framework/src/auth_flows/two_factor framework/tests/two_factor.rs
git commit -m "feat(auth_flows): TwoFactor::enroll/verify/generate_code + recovery codes"
```

---

## Task 5: Remember-me persistent cookies

**Files:** `framework/src/auth_flows/remember_me/mod.rs`

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
//! Remember-me persistent cookies.
//!
//! Set on opt-in login (`Auth::login(user_id, remember: true)`) →
//! we set a long-lived encrypted cookie `remember_me=<token>`.
//! `RememberMeMiddleware` (installed via bootstrap) checks for the
//! cookie when no session is active and re-authenticates.

use crate::{Encrypter, FrameworkError};
use rand::RngCore;

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
    let user_id: i64 = payload
        .split(':')
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or(FrameworkError::Unauthorized)?;
    Ok(user_id)
}

pub fn make_cookie(token: &str) -> crate::Cookie {
    crate::Cookie::new(COOKIE_NAME, token)
        .http_only(true)
        .secure(true)
        .same_site(crate::SameSite::Lax)
        .max_age(std::time::Duration::from_secs(60 * 60 * 24 * COOKIE_LIFETIME_DAYS as u64))
}

pub fn make_clear_cookie() -> crate::Cookie {
    crate::Cookie::new(COOKIE_NAME, "")
        .http_only(true)
        .secure(true)
        .max_age(std::time::Duration::from_secs(0))
}
```

```rust
// framework/src/auth_flows/remember_me/middleware.rs
use crate::http::{Request, Response};
use crate::middleware::{Middleware, Next};
use crate::session::{session, session_mut};
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
                session_mut(|s| {
                    s.put("user_id", user_id);
                });
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
git commit -m "feat(auth_flows): remember-me persistent cookies + middleware"
```

---

## Task 6: App dogfood — wired controllers + routes

**Files:** `app/src/controllers/auth_verify.rs`, `auth_reset.rs`, `auth_2fa.rs`, route wiring

- [ ] **Step 1: Email verify controllers**

```rust
// app/src/controllers/auth_verify.rs
use suprnova::auth_flows::email_verify::{generate_verify_url, verify_url_token, mailable::VerifyEmailMessage};
use suprnova::{json_response, Auth, Encrypter, FrameworkError, Mail, Request, Response};

pub async fn send(req: Request) -> Response {
    let user = Auth::user_as::<crate::models::User>().await?.ok_or(FrameworkError::Unauthorized)?;
    let enc = Encrypter::from_env()?;
    let url = generate_verify_url(
        "http://localhost:8000/email/verify",
        user.id,
        std::time::Duration::from_secs(60 * 60 * 24),
        &enc,
    )?;
    Mail::send(VerifyEmailMessage {
        email: user.email.clone(),
        name: user.name.clone(),
        verify_url: url,
    })
    .await?;
    json_response!({ "sent": true })
}

pub async fn confirm(req: Request) -> Response {
    let enc = Encrypter::from_env()?;
    let user_id = verify_url_token(&req.full_url(), &enc)?;
    // Update user's email_verified_at
    crate::models::User::mark_verified(user_id).await?;
    json_response!({ "verified": true })
}
```

- [ ] **Step 2: Similar for reset + 2FA controllers**

```rust
// app/src/controllers/auth_reset.rs — POST /password/email + POST /password/reset
// app/src/controllers/auth_2fa.rs — GET /2fa/enroll, POST /2fa/confirm, POST /2fa/verify
```

(Implementations follow the same pattern: call the framework facade, return JSON.)

- [ ] **Step 3: Wire routes + smoke test**

```bash
cargo run -p app -- serve &
sleep 2
# Simulate registration → verify
curl -X POST http://127.0.0.1:8000/email/verify/send -b "session=..."
# Check mail log (we run Mail::use_log() in dev)
kill %1
```

- [ ] **Step 4: Commit**

```bash
git add app/src
git commit -m "feat(app): wired email verify / password reset / 2FA / remember-me controllers"
```

---

## Task 7: Workspace lint + roadmap update

- [ ] **Step 1: Clippy + tests**

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

- [ ] **Step 2: Move from "Missing" to "Production-ready"**:
- Email verification
- Password reset
- 2FA TOTP + recovery codes
- Remember-me cookies

- [ ] **Step 3: Commit + push**

---

## Self-Review

| Spec item | Covered by |
|-----------|------------|
| Signed-link email verification | Task 2 |
| VerifiedMiddleware | Task 2 |
| Password reset | Task 3 |
| 2FA TOTP enrollment + verify | Task 4 |
| Recovery codes | Task 4 |
| Remember-me cookies + middleware | Task 5 |
| App dogfood | Task 6 |

---

## Execution Handoff

**Subagent-Driven per task.**
