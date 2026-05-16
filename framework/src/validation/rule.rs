//! Rule objects — composable validators that work alongside (and
//! independently of) `#[derive(Validate)]`.
//!
//! The base trait [`Rule`] covers pure synchronous checks on a single
//! string value. Built-in rules in [`rules`]:
//!
//! - [`rules::Required`] — value must be present and non-whitespace.
//! - [`rules::Email`] — value must be a valid email per the
//!   [`validator`] crate's [`ValidateEmail`](validator::ValidateEmail)
//!   semantics.
//! - [`rules::Min`] / [`rules::Max`] — value length (in `char`s) must
//!   be within bounds.

/// A synchronous validator over a single string value.
///
/// `Err(msg)` carries a human-readable message describing why the
/// value failed. Suprnova does not impose a translation scheme on the
/// message — wrap [`Rule`] yourself if you need i18n.
pub trait Rule {
    /// Check `value`. Return `Ok(())` if it passes, `Err(message)` if
    /// it fails.
    fn passes(&self, value: &str) -> Result<(), String>;
}

/// Built-in synchronous rules.
pub mod rules {
    use super::Rule;
    use validator::ValidateEmail;

    /// Laravel `required` — value must be present and non-whitespace.
    pub struct Required;
    impl Rule for Required {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.trim().is_empty() {
                Err("required".into())
            } else {
                Ok(())
            }
        }
    }

    /// Laravel `email` — defers to [`validator::ValidateEmail`] so
    /// semantics match `#[validate(email)]` on derived types.
    pub struct Email;
    impl Rule for Email {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.validate_email() {
                Ok(())
            } else {
                Err("must be a valid email".into())
            }
        }
    }

    /// Laravel `min:N` — value must be at least `N` characters long.
    ///
    /// Counts Unicode scalar values (`char`s), not bytes, so multi-byte
    /// characters count as a single character.
    pub struct Min(pub usize);
    impl Rule for Min {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.chars().count() >= self.0 {
                Ok(())
            } else {
                Err(format!("must be at least {} characters", self.0))
            }
        }
    }

    /// Laravel `max:N` — value must be at most `N` characters long.
    ///
    /// Counts Unicode scalar values (`char`s), not bytes.
    pub struct Max(pub usize);
    impl Rule for Max {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.chars().count() <= self.0 {
                Ok(())
            } else {
                Err(format!("must be at most {} characters", self.0))
            }
        }
    }
}
