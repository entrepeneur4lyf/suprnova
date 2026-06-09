//! `MustVerifyEmail` — a model trait letting the email-verification and
//! password-reset flows read a user's email/name + verification timestamp
//! independent of the storage backend. Laravel's `MustVerifyEmail`.

use crate::auth::Authenticatable;
use chrono::{DateTime, Utc};

/// Model trait the email-verification and password-reset flows use to read a
/// user's email, display name, and verification timestamp without coupling to
/// any particular storage backend. The Suprnova analogue of Laravel's
/// `MustVerifyEmail` contract.
pub trait MustVerifyEmail: Authenticatable {
    /// The user's email address (the verification/reset target).
    fn email(&self) -> &str;
    /// When the email was verified, if ever.
    fn email_verified_at(&self) -> Option<DateTime<Utc>>;
    /// Set/clear the verification timestamp (used by the provider to mark
    /// verified, and to re-trigger verification on email change).
    fn set_email_verified_at(&mut self, v: Option<DateTime<Utc>>);
    /// Overwrite the stored password hash. The value arrives ALREADY HASHED
    /// (the password-reset flow hashes the new plaintext before calling the
    /// provider) — store it verbatim. Mirrors [`set_email_verified_at`] as the
    /// second mutable field the auth flows write through this trait, so a
    /// generic [`UserProvider`](crate::auth::UserProvider) can persist a reset
    /// password without coupling to any concrete model's field layout.
    fn set_password_hash(&mut self, hash: &str);
    /// True once verified.
    fn is_email_verified(&self) -> bool {
        self.email_verified_at().is_some()
    }
    /// Display name for the email greeting, if any.
    fn name(&self) -> Option<&str> {
        None
    }
}

/// Lightweight user carrier the `UserProvider` returns to the auth-flow
/// facades, so they get email/name without trait-object gymnastics on
/// `Authenticatable`.
#[derive(Debug, Clone)]
pub struct AuthFlowUser {
    /// The user's stable identifier (Laravel's `getAuthIdentifier`), carried as
    /// a `String` end-to-end like the rest of the auth surface.
    pub id: String,
    /// The user's email address — the verification/reset target.
    pub email: String,
    /// Optional display name for the email greeting.
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;

    struct SampleUser {
        id: String,
        email: String,
        password: String,
        verified_at: Option<DateTime<Utc>>,
    }

    impl Authenticatable for SampleUser {
        fn get_auth_identifier(&self) -> String {
            self.id.clone()
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn into_arc_any(
            self: std::sync::Arc<Self>,
        ) -> std::sync::Arc<dyn Any + Send + Sync> {
            self
        }
    }

    impl MustVerifyEmail for SampleUser {
        fn email(&self) -> &str {
            &self.email
        }

        fn email_verified_at(&self) -> Option<DateTime<Utc>> {
            self.verified_at
        }

        fn set_email_verified_at(&mut self, v: Option<DateTime<Utc>>) {
            self.verified_at = v;
        }

        fn set_password_hash(&mut self, hash: &str) {
            self.password = hash.to_string();
        }
    }

    #[test]
    fn is_email_verified_default_tracks_timestamp() {
        let mut user = SampleUser {
            id: "1".to_string(),
            email: "user@example.com".to_string(),
            password: "old-hash".to_string(),
            verified_at: None,
        };
        assert!(!user.is_email_verified());
        assert_eq!(user.name(), None);

        let now = Utc::now();
        user.set_email_verified_at(Some(now));
        assert!(user.is_email_verified());
        assert_eq!(user.email_verified_at(), Some(now));
        assert_eq!(user.email(), "user@example.com");

        user.set_password_hash("new-hash");
        assert_eq!(user.password, "new-hash");
    }
}
