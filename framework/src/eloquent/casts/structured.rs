//! Structured casts — `Vec`, `HashMap`-shaped struct, `Collection`,
//! `serde_json::Value`, and `IndexMap<String, T>`.
//!
//! All five casts serialise the runtime value to JSON text and store
//! it in a `TEXT` column. The storage shape is intentionally
//! backend-agnostic: SQLite has no native JSON type so we pick TEXT
//! as the lowest-common-denominator that every backend round-trips
//! cleanly through SeaORM's `Value::String` boundary. Postgres /
//! MySQL accept JSON-in-TEXT just as cleanly; users who want native
//! `JSONB` / `JSON` column types in Postgres / MySQL can write a
//! manual column definition — the cast layer doesn't constrain it.
//!
//! ## `AsArrayObject` vs `AsObject`
//!
//! `AsObject<T>` is the right cast when the runtime shape is a fixed
//! struct (e.g. `Prefs { theme: String, ... }`). `AsArrayObject<T>`
//! is the right cast when the runtime shape is an associative map
//! with insertion-order semantics (`IndexMap<String, T>`). The two
//! casts produce equivalent JSON on disk; the choice is about which
//! Rust type the user wants at the field.
//!
//! ## `AsJson<T>`
//!
//! A pass-through cast for any `T: Serialize + DeserializeOwned`.
//! Useful when the field is a `serde_json::Value` or a user-defined
//! struct that's already fully describable in serde terms.

use std::marker::PhantomData;

use serde::{Serialize, de::DeserializeOwned};

use super::{Cast, DynCast, IntoDynCast};
use crate::error::FrameworkError;

// ---- AsArray<T> -----------------------------------------------------------

/// Cast `Vec<T>` ↔ JSON-encoded `TEXT`. The element type `T` must be
/// `Serialize + DeserializeOwned`.
pub struct AsArray<T>(PhantomData<T>);

impl<T> Cast for AsArray<T>
where
    T: Serialize + DeserializeOwned + Send + Sync,
{
    type Runtime = Vec<T>;
    type Storage = String;

    fn to_storage(v: &Vec<T>) -> Result<String, FrameworkError> {
        serde_json::to_string(v).map_err(|e| FrameworkError::validation("AsArray", format!("{e}")))
    }

    fn from_storage(s: &String) -> Result<Vec<T>, FrameworkError> {
        serde_json::from_str(s).map_err(|e| FrameworkError::validation("AsArray", format!("{e}")))
    }
}

struct AsArrayDyn<T>(PhantomData<T>);

impl<T> DynCast for AsArrayDyn<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        // The TEXT column round-trips as a JSON string — `v.as_str()`
        // gives us the encoded payload; parse it back into `Vec<T>`
        // then re-emit as a JSON array so downstream deserialization
        // into the user model's `Vec<T>` field succeeds.
        //
        // Domain 7 audit D7-A — was `v.as_str().unwrap_or("[]")` which
        // silently treated non-string storage as an empty array. Now
        // strict so a misconfigured column produces an explicit type
        // mismatch rather than a silent empty Vec<T>.
        let s = v.as_str().ok_or_else(|| {
            FrameworkError::validation(
                "AsArray",
                format!("dyn from_storage: expected JSON string, got {v:?}"),
            )
        })?;
        let parsed: Vec<T> = serde_json::from_str(s)
            .map_err(|e| FrameworkError::validation("AsArray", format!("dyn parse: {e}")))?;
        serde_json::to_value(parsed)
            .map_err(|e| FrameworkError::internal(format!("AsArray: re-serialize failed: {e}")))
    }

    fn to_storage_json(&self, v: &serde_json::Value) -> Result<serde_json::Value, FrameworkError> {
        Ok(serde_json::Value::String(v.to_string()))
    }
}

impl<T> IntoDynCast for AsArray<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsArrayDyn::<T>(PhantomData))
    }
}

// ---- AsObject<T> ----------------------------------------------------------

/// Cast a `Serialize + DeserializeOwned` struct ↔ JSON-encoded `TEXT`.
/// Use for fixed-shape associative data where the keys are statically
/// known (e.g. a `Prefs { theme, notifications }` config struct). Use
/// [`AsArrayObject`] when the runtime shape is a dynamic map.
pub struct AsObject<T>(PhantomData<T>);

impl<T> Cast for AsObject<T>
where
    T: Serialize + DeserializeOwned + Send + Sync,
{
    type Runtime = T;
    type Storage = String;

    fn to_storage(v: &T) -> Result<String, FrameworkError> {
        serde_json::to_string(v).map_err(|e| FrameworkError::validation("AsObject", format!("{e}")))
    }

    fn from_storage(s: &String) -> Result<T, FrameworkError> {
        serde_json::from_str(s).map_err(|e| FrameworkError::validation("AsObject", format!("{e}")))
    }
}

struct AsObjectDyn<T>(PhantomData<T>);

impl<T> DynCast for AsObjectDyn<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        // Domain 7 audit D7-A — strict-validate input shape.
        let s = v.as_str().ok_or_else(|| {
            FrameworkError::validation(
                "AsObject",
                format!("dyn from_storage: expected JSON string, got {v:?}"),
            )
        })?;
        let parsed: T = serde_json::from_str(s)
            .map_err(|e| FrameworkError::validation("AsObject", format!("dyn parse: {e}")))?;
        serde_json::to_value(parsed)
            .map_err(|e| FrameworkError::internal(format!("AsObject: re-serialize failed: {e}")))
    }

    fn to_storage_json(&self, v: &serde_json::Value) -> Result<serde_json::Value, FrameworkError> {
        Ok(serde_json::Value::String(v.to_string()))
    }
}

impl<T> IntoDynCast for AsObject<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsObjectDyn::<T>(PhantomData))
    }
}

// ---- AsCollection<T> ------------------------------------------------------

/// Cast `Collection<T>` ↔ JSON-encoded `TEXT`. Stores as a JSON array
/// of `T` and decodes back into the [`Collection`] wrapper. The
/// runtime type is the framework's [`Collection<T>`] (a thin
/// `Vec<T>` newtype) so the user gets slice-style indexing /
/// iteration plus the Eloquent-style methods Phase 10C adds.
///
/// [`Collection`]: crate::eloquent::Collection
/// [`Collection<T>`]: crate::eloquent::Collection
pub struct AsCollection<T>(PhantomData<T>);

impl<T> Cast for AsCollection<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + Clone,
{
    type Runtime = crate::eloquent::Collection<T>;
    type Storage = String;

    fn to_storage(v: &crate::eloquent::Collection<T>) -> Result<String, FrameworkError> {
        serde_json::to_string(v.as_ref())
            .map_err(|e| FrameworkError::validation("AsCollection", format!("{e}")))
    }

    fn from_storage(s: &String) -> Result<crate::eloquent::Collection<T>, FrameworkError> {
        let v: Vec<T> = serde_json::from_str(s)
            .map_err(|e| FrameworkError::validation("AsCollection", format!("{e}")))?;
        Ok(crate::eloquent::Collection::from(v))
    }
}

struct AsCollectionDyn<T>(PhantomData<T>);

impl<T> DynCast for AsCollectionDyn<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + Clone + 'static,
{
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        // Domain 7 audit D7-A — strict-validate input shape.
        let s = v.as_str().ok_or_else(|| {
            FrameworkError::validation(
                "AsCollection",
                format!("dyn from_storage: expected JSON string, got {v:?}"),
            )
        })?;
        let parsed: Vec<T> = serde_json::from_str(s)
            .map_err(|e| FrameworkError::validation("AsCollection", format!("dyn parse: {e}")))?;
        serde_json::to_value(parsed).map_err(|e| {
            FrameworkError::internal(format!("AsCollection: re-serialize failed: {e}"))
        })
    }

    fn to_storage_json(&self, v: &serde_json::Value) -> Result<serde_json::Value, FrameworkError> {
        Ok(serde_json::Value::String(v.to_string()))
    }
}

impl<T> IntoDynCast for AsCollection<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + Clone + 'static,
{
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsCollectionDyn::<T>(PhantomData))
    }
}

// ---- AsJson<T> ------------------------------------------------------------

/// Cast any `Serialize + DeserializeOwned` type ↔ JSON-encoded `TEXT`.
/// Pass-through both directions — the cast exists so the storage shape
/// is uniform (TEXT) across backends. Use when the field is a
/// `serde_json::Value` or a user-defined struct that's already
/// serde-describable.
pub struct AsJson<T>(PhantomData<T>);

impl<T> Cast for AsJson<T>
where
    T: Serialize + DeserializeOwned + Send + Sync,
{
    type Runtime = T;
    type Storage = String;

    fn to_storage(v: &T) -> Result<String, FrameworkError> {
        serde_json::to_string(v).map_err(|e| FrameworkError::validation("AsJson", format!("{e}")))
    }

    fn from_storage(s: &String) -> Result<T, FrameworkError> {
        serde_json::from_str(s).map_err(|e| FrameworkError::validation("AsJson", format!("{e}")))
    }
}

struct AsJsonDyn<T>(PhantomData<T>);

impl<T> DynCast for AsJsonDyn<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        // Domain 7 audit D7-A — strict-validate input shape.
        let s = v.as_str().ok_or_else(|| {
            FrameworkError::validation(
                "AsJson",
                format!("dyn from_storage: expected JSON string, got {v:?}"),
            )
        })?;
        let parsed: T = serde_json::from_str(s)
            .map_err(|e| FrameworkError::validation("AsJson", format!("dyn parse: {e}")))?;
        serde_json::to_value(parsed)
            .map_err(|e| FrameworkError::internal(format!("AsJson: re-serialize failed: {e}")))
    }

    fn to_storage_json(&self, v: &serde_json::Value) -> Result<serde_json::Value, FrameworkError> {
        Ok(serde_json::Value::String(v.to_string()))
    }
}

impl<T> IntoDynCast for AsJson<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsJsonDyn::<T>(PhantomData))
    }
}

// ---- AsArrayObject<T> -----------------------------------------------------

/// Cast `IndexMap<String, T>` ↔ JSON-encoded `TEXT`. Use when the
/// runtime shape is a dynamic-key map and the order of keys is
/// significant (e.g. a UI ordering of labels). For fixed-shape
/// records, use [`AsObject`].
///
/// `IndexMap` over `HashMap` is intentional: serde's JSON
/// serialisation preserves insertion order through `IndexMap` (Rust's
/// `HashMap` randomises bucket order), and the framework's
/// `serde_json` is already configured with `preserve_order` for the
/// same reason.
pub struct AsArrayObject<T>(PhantomData<T>);

impl<T> Cast for AsArrayObject<T>
where
    T: Serialize + DeserializeOwned + Send + Sync,
{
    type Runtime = indexmap::IndexMap<String, T>;
    type Storage = String;

    fn to_storage(v: &indexmap::IndexMap<String, T>) -> Result<String, FrameworkError> {
        serde_json::to_string(v)
            .map_err(|e| FrameworkError::validation("AsArrayObject", format!("{e}")))
    }

    fn from_storage(s: &String) -> Result<indexmap::IndexMap<String, T>, FrameworkError> {
        serde_json::from_str(s)
            .map_err(|e| FrameworkError::validation("AsArrayObject", format!("{e}")))
    }
}

struct AsArrayObjectDyn<T>(PhantomData<T>);

impl<T> DynCast for AsArrayObjectDyn<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        // Domain 7 audit D7-A — strict-validate input shape.
        let s = v.as_str().ok_or_else(|| {
            FrameworkError::validation(
                "AsArrayObject",
                format!("dyn from_storage: expected JSON string, got {v:?}"),
            )
        })?;
        let parsed: indexmap::IndexMap<String, T> = serde_json::from_str(s)
            .map_err(|e| FrameworkError::validation("AsArrayObject", format!("dyn parse: {e}")))?;
        serde_json::to_value(parsed).map_err(|e| {
            FrameworkError::internal(format!("AsArrayObject: re-serialize failed: {e}"))
        })
    }

    fn to_storage_json(&self, v: &serde_json::Value) -> Result<serde_json::Value, FrameworkError> {
        Ok(serde_json::Value::String(v.to_string()))
    }
}

impl<T> IntoDynCast for AsArrayObject<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsArrayObjectDyn::<T>(PhantomData))
    }
}
