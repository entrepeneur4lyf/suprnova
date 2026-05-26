//! Domain 5 audit M-D5-1 runtime regression.
//!
//! The macro-emitted `From<inner::Model> for UserStruct` and
//! `From<UserStruct> for inner::Model` impls panic on cast failure —
//! that design hasn't changed because making them fallible would
//! require breaking the row-materialisation API surface on
//! `Builder::get`, `Model::all`, every eager-load arm, etc. (Domain 2's
//! middleware safety net translates the panic to a 500 with the
//! message in the tracing log.)
//!
//! M-D5-1 ensures the panic message includes the offending field name
//! and the original `FrameworkError`, so an operator can locate which
//! column failed and why directly from the trace — no column-by-column
//! bisection needed. These tests exercise the runtime panic path by
//! constructing an inner `Model` value directly (no DB required) and
//! catching the panic the generated From impl raises.
//!
//! The token-shape regression lives in
//! `suprnova-macros/src/model/casts.rs::tests`; this file ensures the
//! emitted code actually behaves the way the token-shape tests claim it
//! does at runtime.

use std::panic::{AssertUnwindSafe, catch_unwind};

use suprnova::eloquent::casts::Cast;
use suprnova::{FrameworkError, model};

/// A test-only cast that fails in BOTH directions. Used to assert the
/// runtime panic shape on `From<inner::Model>` (read path) and
/// `From<UserStruct>` (write path).
pub struct AlwaysFails;

impl Cast for AlwaysFails {
    type Runtime = String;
    type Storage = String;

    fn to_storage(_value: &Self::Runtime) -> Result<Self::Storage, FrameworkError> {
        Err(FrameworkError::internal("to_storage exploded"))
    }

    fn from_storage(_stored: &Self::Storage) -> Result<Self::Runtime, FrameworkError> {
        Err(FrameworkError::internal("from_storage exploded"))
    }
}

#[model(
    table = "cast_panic_canary",
    timestamps = false,
    fillable = ["payload"],
    casts = { payload = AlwaysFails }
)]
pub struct CastPanicCanary {
    pub id: i64,
    pub payload: String,
}

/// Extract a panic payload as a `String` regardless of whether the
/// panic emitted via `panic!(format!(...))` (String payload) or
/// `panic!("literal")` (&'static str payload). The macro emits via
/// `panic!("...{}...", #field_name, __cast_err)` which produces a
/// String payload, but the runtime payload type is technically opaque
/// — covering both branches keeps the assertion robust.
fn panic_message_of(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else {
        "<unrecognised panic payload type>".to_string()
    }
}

#[test]
fn from_storage_panic_includes_field_name_and_source_error() {
    // Build the inner SeaORM Model directly with an arbitrary storage
    // value. The cast's `from_storage` will return Err when the
    // generated `From<cast_panic_canary::Model> for CastPanicCanary`
    // tries to inflate the field — the panic that translates that Err
    // is the path we're asserting against.
    let inner = cast_panic_canary::Model {
        id: 1,
        payload: "any-stored-value".to_string(),
    };

    let result = catch_unwind(AssertUnwindSafe(|| CastPanicCanary::from(inner)));
    let payload = result.err().expect(
        "From<cast_panic_canary::Model> for CastPanicCanary must panic when the cast fails",
    );
    let msg = panic_message_of(payload);

    assert!(
        msg.contains("payload"),
        "panic must name the offending field; got: {msg}",
    );
    assert!(
        msg.contains("from_storage exploded"),
        "panic must surface the source FrameworkError; got: {msg}",
    );
    assert!(
        msg.contains("from_storage"),
        "panic must identify the direction (from_storage); got: {msg}",
    );
}

#[test]
fn to_storage_panic_includes_field_name_and_source_error() {
    // Construct the user struct directly. The macro auto-injects
    // `__eager` and `__pivot` runtime-scratch fields on the user
    // struct — `..Default::default()` fills them with the empty cache
    // and `None` respectively. The generated
    // `From<CastPanicCanary> for cast_panic_canary::Model` calls
    // `<AlwaysFails as Cast>::to_storage(&s.payload)`, which Errs —
    // the From impl is infallible by signature so the Err translates
    // to a panic carrying the new diagnostic.
    let user = CastPanicCanary {
        id: 1,
        payload: "any-runtime-value".to_string(),
        ..Default::default()
    };

    let result = catch_unwind(AssertUnwindSafe(|| cast_panic_canary::Model::from(user)));
    let payload = result.err().expect(
        "From<CastPanicCanary> for cast_panic_canary::Model must panic when the cast fails",
    );
    let msg = panic_message_of(payload);

    assert!(
        msg.contains("payload"),
        "panic must name the offending field; got: {msg}",
    );
    assert!(
        msg.contains("to_storage exploded"),
        "panic must surface the source FrameworkError; got: {msg}",
    );
    assert!(
        msg.contains("to_storage"),
        "panic must identify the direction (to_storage); got: {msg}",
    );
}

#[test]
fn pre_audit_panic_message_no_longer_present() {
    // Smoke-test guard against accidental reversion to the pre-audit
    // diagnostic. The old text was:
    //   "cast from_storage failed — corrupt data in database column"
    // (no field name, no source error). If someone reverts the patch,
    // this test fails with a clear pointer to the audit.
    let inner = cast_panic_canary::Model {
        id: 2,
        payload: "anything".to_string(),
    };
    let result = catch_unwind(AssertUnwindSafe(|| CastPanicCanary::from(inner)));
    let msg = panic_message_of(result.err().expect("must panic"));
    assert!(
        !msg.contains("corrupt data in database column\""),
        "pre-audit panic message detected — Domain 5 M-D5-1 regression; got: {msg}",
    );
    // Sanity: the same path must not silently swallow the failure.
    assert!(!msg.is_empty(), "panic payload must not be empty",);
}
