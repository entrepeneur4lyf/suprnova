//! Cast trait + registry. Casts mediate the storage ↔ runtime
//! boundary for an Eloquent model field: a `Cast` impl declares the
//! Rust-shape (`Runtime`) and the on-disk shape (`Storage`) and
//! provides the two conversion directions.
//!
//! ## Explicit-only
//!
//! Per the spec's locked decisions there is no auto-detection from
//! field types — a `Vec<String>` field does not implicitly become
//! `AsArray<String>`. You must write
//! `#[suprnova::model(casts = { tags = AsArray<String> })]` (T7b).
//!
//! T7a ships the 10 primitive + temporal casts. T7b adds structured +
//! enum + `with_casts` runtime override. T7c adds encrypted +
//! hashed casts, completing the 21-cast surface from spec §4.

pub mod encrypted;
pub mod enum_cast;
pub mod primitive;
pub mod structured;
pub mod temporal;

use crate::error::FrameworkError;

/// Storage-shape ↔ runtime-shape cast applied at row materialisation
/// (`from_storage`) and at write (`to_storage`).
///
/// `Runtime` is the Rust type the user writes in their model struct
/// (e.g. `bool`, `chrono::NaiveDate`, `rust_decimal::Decimal`).
/// `Storage` is the type SeaORM sees for the column (e.g. `i64` for a
/// SQLite `INTEGER` boolean column, `String` for a `TEXT` date).
///
/// Both directions are fallible because temporal / decimal parsing
/// can fail — the macro propagates the `Result` through the model
/// trait's `apply_attrs_to_active_model` and `From<inner::Model>`
/// emissions.
pub trait Cast: Send + Sync {
    type Runtime;
    type Storage;

    fn to_storage(value: &Self::Runtime) -> Result<Self::Storage, FrameworkError>;
    fn from_storage(stored: &Self::Storage) -> Result<Self::Runtime, FrameworkError>;
}

/// Type-erased cast for `Builder::with_casts(...)` runtime override.
/// The Builder stores `HashMap<&str, Arc<dyn DynCast>>` so heterogeneous
/// casts can live in one map; column reads always pass through
/// `serde_json::Value` at that boundary so a single trait shape covers
/// every cast.
///
/// T7a ships the impls for the 10 primitive + temporal casts; T7b
/// ships the consumer that actually walks `Builder.runtime_casts`
/// during row materialisation.
///
/// The `from_*` / `to_*` names take `&self` because the cast instance
/// can carry config (e.g. an encryption key in T7c); they're not Rust's
/// conventional consume-self constructors. Clippy's
/// `wrong_self_convention` lint is allowed here for that reason.
#[allow(clippy::wrong_self_convention)]
pub trait DynCast: Send + Sync {
    /// Convert a raw storage value into the in-memory shape (e.g.
    /// decode a JSON column into a `serde_json::Value`).
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError>;

    /// Convert an in-memory value into its storage shape (e.g. encode
    /// a `serde_json::Value` back into a JSON string for the
    /// underlying TEXT column).
    fn to_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError>;
}

/// Bridges a statically-typed `Cast` to its `DynCast` shadow. Users
/// who want to pass a cast to `Builder::with_casts(...)` write
/// `("col_name", <AsBool as IntoDynCast>::into_dyn())`.
pub trait IntoDynCast {
    fn into_dyn() -> Box<dyn DynCast>;
}

// ---- Submodule re-exports ------------------------------------------------
//
// Hoist every cast type to `suprnova::eloquent::casts::*` so users can
// `use suprnova::eloquent::casts::AsArray;` regardless of which file
// the type was originally declared in. The crate root in `lib.rs`
// re-exports the user-facing names further (`suprnova::AsArray`).

pub use encrypted::{
    AsEncrypted, AsEncryptedArray, AsEncryptedCollection, AsEncryptedObject, AsHashed,
};
pub use enum_cast::AsEnum;
pub use primitive::{AsBool, AsDecimal, AsFloat, AsInt, AsString};
pub use structured::{AsArray, AsArrayObject, AsCollection, AsJson, AsObject};
pub use temporal::{AsDate, AsDateTime, AsImmutableDate, AsImmutableDateTime, AsTimestamp};

/// Construct a `HashMap<&'static str, Arc<dyn DynCast>>` for use with
/// `Builder::with_casts(...)`. Each entry is `field_name = CastType`;
/// the macro materialises the dyn-cast box via `IntoDynCast::into_dyn`
/// and wraps it in an `Arc` so the resulting map can be cloned per
/// builder reuse.
///
/// ```ignore
/// use suprnova::{casts, AsDate, AsJson};
///
/// let map = casts! {
///     birthday = AsDate,
///     metadata = AsJson<serde_json::Value>,
/// };
/// let rows = User::query().with_casts(map).get().await?;
/// ```
#[macro_export]
macro_rules! casts {
    ($($field:ident = $cast:ty),* $(,)?) => {{
        let mut map: ::std::collections::HashMap<
            &'static str,
            ::std::sync::Arc<dyn $crate::eloquent::casts::DynCast>,
        > = ::std::collections::HashMap::new();
        $(
            map.insert(
                stringify!($field),
                ::std::sync::Arc::<dyn $crate::eloquent::casts::DynCast>::from(
                    <$cast as $crate::eloquent::casts::IntoDynCast>::into_dyn(),
                ),
            );
        )*
        map
    }};
}
