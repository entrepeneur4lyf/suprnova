//! #380b (Augment) — Inertia eager-prop serialization no longer forces a
//! panic on recoverable `Serialize` failures.
//!
//! Closes the Codex MEDIUM "infallible public surfaces still convert
//! recoverable errors into panics" for the Inertia surface. The panicking
//! helpers are RETAINED as ergonomic escape hatches: on the HTTP request
//! path a panic is caught by the panic-recovery middleware and converted to
//! a 500. The new fallible siblings return `Err(FrameworkError)` (naming the
//! offending field/key) so a bad `Serialize` impl becomes a recoverable
//! error off that path (queue workers, the scheduler, CLI), where no panic
//! net exists:
//!
//! - `#[derive(Data)]` gains `__try_into_inertia_props`, surfaced through
//!   `Inertia::try_data`; the infallible `__into_inertia_props` /
//!   `Inertia::data` keeps panicking (Domain 5 M-D5-9 diagnostic shape).
//! - The eager prop builders gain `try_with` / `try_always` /
//!   `try_merge_with` / `try_scroll` / `try_flash` siblings of the
//!   `to_value_or_die`-backed `with` / `always` / `merge_with` / `scroll` /
//!   `flash`.
//!
//! Teeth: against the pre-#380b code these `try_*` paths did not exist; the
//! only way to serialize an eager prop was the panicking helper.

use std::panic::{AssertUnwindSafe, catch_unwind};

use suprnova::serde::ser::Error as _;
use suprnova::serde::{Deserialize, Deserializer, Serialize, Serializer};
use suprnova::{Inertia, InertiaResponse, MergeStrategy, ScrollMetadata};

/// A field type whose `Serialize` impl always fails — the only way to drive
/// `serde_json::to_value` to `Err` for an otherwise-ordinary type. The
/// `Deserialize` impl trivially succeeds so `#[derive(Data)]` (which consumes
/// the eager fields on its input side) is satisfied.
#[derive(Debug, Default, Clone)]
struct BoomSerialize;

impl Serialize for BoomSerialize {
    fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(S::Error::custom("boom: serialize always fails"))
    }
}

impl<'de> Deserialize<'de> for BoomSerialize {
    fn deserialize<D: Deserializer<'de>>(_deserializer: D) -> Result<Self, D::Error> {
        Ok(BoomSerialize)
    }
}

/// DTO with one eager field that fails to serialize. `validator::Validate` is
/// required because `FormRequest` (which `#[derive(Data)]` implements) lists
/// it as a supertrait; with no `#[validate(...)]` attributes it is a no-op.
#[derive(suprnova::Data, validator::Validate)]
struct BoomDto {
    ok: i32,
    bad: BoomSerialize,
}

/// DTO whose fields all serialize cleanly — exercises the happy path.
#[derive(suprnova::Data, validator::Validate)]
struct OkDto {
    id: i32,
    name: String,
}

/// Extract a panic payload as a `String` regardless of whether it was raised
/// via `panic!(format!(...))` (String payload) or a `&'static str`.
fn panic_message_of(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else {
        "<unrecognised panic payload type>".to_string()
    }
}

// ---- #[derive(Data)] macro surface ------------------------------------

#[test]
fn try_into_inertia_props_returns_err_naming_field() {
    let dto = BoomDto {
        ok: 1,
        bad: BoomSerialize,
    };
    // Inherent method emitted by `#[derive(Data)]`; no trait import needed.
    let Err(err) = dto.__try_into_inertia_props() else {
        panic!("__try_into_inertia_props must return Err when a field's Serialize fails");
    };
    let msg = err.to_string();

    assert!(msg.contains("bad"), "error must name the field; got: {msg}");
    assert!(
        msg.contains("BoomDto"),
        "error must name the struct; got: {msg}",
    );
    assert!(
        msg.contains("boom: serialize always fails"),
        "error must surface the source serde error; got: {msg}",
    );
}

#[test]
fn try_data_facade_returns_err_naming_field() {
    let Err(err) = Inertia::try_data(
        "Boom/Page",
        BoomDto {
            ok: 1,
            bad: BoomSerialize,
        },
    ) else {
        panic!("Inertia::try_data must return Err, not panic, on serialize failure");
    };
    assert!(
        err.to_string().contains("bad"),
        "error must name the field; got: {err}",
    );
}

#[test]
fn data_facade_still_panics_naming_field() {
    // The infallible escape hatch keeps its M-D5-9 diagnostic panic so the
    // request-level panic-recovery middleware can translate it to a 500.
    let result = catch_unwind(AssertUnwindSafe(|| {
        Inertia::data(
            "Boom/Page",
            BoomDto {
                ok: 2,
                bad: BoomSerialize,
            },
        )
    }));
    let Err(payload) = result else {
        panic!("Inertia::data must still panic on bad Serialize");
    };
    let msg = panic_message_of(payload);

    assert!(msg.contains("bad"), "panic must name the field; got: {msg}");
    assert!(
        msg.contains("BoomDto"),
        "panic must name the struct; got: {msg}",
    );
}

#[test]
fn try_data_ok_path_builds_response() {
    let resp = Inertia::try_data(
        "Ok/Page",
        OkDto {
            id: 7,
            name: "ada".to_string(),
        },
    );
    assert!(
        resp.is_ok(),
        "Inertia::try_data must succeed when all fields serialize cleanly",
    );
}

// ---- InertiaResponse eager-prop builders ------------------------------

#[test]
fn try_with_returns_err_naming_key() {
    let Err(err) = InertiaResponse::new("C").try_with("widget", BoomSerialize) else {
        panic!("try_with must return Err on serialize failure");
    };
    assert!(
        err.to_string().contains("widget"),
        "error must name the prop key; got: {err}",
    );
}

#[test]
fn try_always_returns_err_naming_key() {
    let Err(err) = InertiaResponse::new("C").try_always("flag", BoomSerialize) else {
        panic!("try_always must return Err on serialize failure");
    };
    assert!(
        err.to_string().contains("flag"),
        "error must name the prop key; got: {err}",
    );
}

#[test]
fn try_merge_with_returns_err_naming_key() {
    let Err(err) = InertiaResponse::new("C").try_merge_with(
        "list",
        BoomSerialize,
        MergeStrategy::Append { match_on: None },
    ) else {
        panic!("try_merge_with must return Err on serialize failure");
    };
    assert!(
        err.to_string().contains("list"),
        "error must name the prop key; got: {err}",
    );
}

#[test]
fn try_scroll_returns_err_naming_key() {
    let Err(err) =
        InertiaResponse::new("C").try_scroll("rows", ScrollMetadata::new("page"), BoomSerialize)
    else {
        panic!("try_scroll must return Err on serialize failure");
    };
    assert!(
        err.to_string().contains("rows"),
        "error must name the prop key; got: {err}",
    );
}

#[test]
fn try_flash_returns_err_naming_key() {
    let Err(err) = InertiaResponse::new("C").try_flash("toast", BoomSerialize) else {
        panic!("try_flash must return Err on serialize failure");
    };
    assert!(
        err.to_string().contains("toast"),
        "error must name the prop key; got: {err}",
    );
}

#[test]
fn try_with_ok_path_inserts_prop() {
    let resp = InertiaResponse::new("C").try_with("name", "ada");
    assert!(
        resp.is_ok(),
        "try_with must succeed when the value serializes cleanly",
    );
}
