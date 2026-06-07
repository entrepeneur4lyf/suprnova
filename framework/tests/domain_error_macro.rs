//! Regression tests for `#[domain_error]` across all three struct shapes.
//!
//! Bug history: the macro previously emitted `#vis struct #name #fields`
//! verbatim. For named-field structs that's correct — the `{ ... }` block
//! terminates the declaration. For unit and tuple structs the result was
//! invalid syntax (`pub struct Foo` or `pub struct Foo(T)` with no trailing
//! `;`), so the unit-struct form documented in `manual/errors.md` failed to
//! compile and copy-paste users hit a confusing "expected `where`, `{`, `(`,
//! or `;` after struct name" error pointing at their own code.
//!
//! All three forms must compile and produce the documented surface:
//! - `Debug + Clone + std::error::Error + std::fmt::Display`
//! - `suprnova::HttpError` with the chosen `status_code()` and the
//!   `error_message()` derived from `Display`
//! - `From<Self> for FrameworkError` so `?` bridges via `FrameworkError`

use suprnova::{FrameworkError, HttpError, domain_error};

#[domain_error(status = 404, message = "User not found")]
pub struct UserNotFound;

// Regression for the format-string trap: the message is rendered
// through `f.write_str`, so unescaped braces in the literal must NOT
// be interpreted as format positional arguments. The whole test file
// failed to compile before the fix.
#[domain_error(status = 404, message = "User {id} not found")]
pub struct UserNotFoundWithBraces;

#[domain_error(status = 422, message = "Bad input")]
pub struct InvalidInput(pub String);

#[domain_error(status = 402, message = "Insufficient funds")]
pub struct InsufficientFunds {
    pub available: u64,
    pub requested: u64,
}

#[test]
fn unit_struct_form_compiles_and_renders() {
    let err = UserNotFound;
    assert_eq!(err.status_code(), 404);
    assert_eq!(err.error_message(), "User not found");
    let display = format!("{err}");
    assert_eq!(display, "User not found");
    // Clone + Debug both implemented
    let _cloned = err.clone();
    let _ = format!("{err:?}");
}

#[test]
fn tuple_struct_form_compiles_and_renders() {
    let err = InvalidInput("email is empty".to_string());
    assert_eq!(err.status_code(), 422);
    assert_eq!(err.error_message(), "Bad input");
    let _cloned = err.clone();
}

#[test]
fn named_struct_form_compiles_and_renders() {
    let err = InsufficientFunds {
        available: 10,
        requested: 100,
    };
    assert_eq!(err.status_code(), 402);
    assert_eq!(err.error_message(), "Insufficient funds");
    let _cloned = err.clone();
}

#[test]
fn message_with_braces_renders_verbatim() {
    let err = UserNotFoundWithBraces;
    assert_eq!(err.status_code(), 404);
    // The literal `{id}` is part of the message, not a format spec.
    assert_eq!(err.error_message(), "User {id} not found");
    assert_eq!(format!("{err}"), "User {id} not found");
}

#[test]
fn from_self_into_framework_error_carries_status_and_message() {
    fn bridge<E: Into<FrameworkError>>(e: E) -> FrameworkError {
        e.into()
    }

    let fw_unit: FrameworkError = bridge(UserNotFound);
    assert_eq!(fw_unit.status_code(), 404);

    let fw_tuple: FrameworkError = bridge(InvalidInput("x".into()));
    assert_eq!(fw_tuple.status_code(), 422);

    let fw_named: FrameworkError = bridge(InsufficientFunds {
        available: 0,
        requested: 1,
    });
    assert_eq!(fw_named.status_code(), 402);
}
