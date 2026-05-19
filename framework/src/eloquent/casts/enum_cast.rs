//! Enum cast — `AsEnum<E>` for any `E: FromStr + AsRef<str>`.
//!
//! Stores the variant name (or the user-customised `strum::AsRefStr`
//! string) as a `TEXT` column and parses back via `FromStr`. The two
//! trait bounds together let the cast work cleanly with any enum the
//! user marks with `#[derive(strum::EnumString, strum::AsRefStr)]` —
//! or any enum where the user wrote the impls manually. There is no
//! framework lock-in on `strum`; it's just the most ergonomic way to
//! get the bounds without hand-rolling them.
//!
//! ## Why string storage, not integer
//!
//! Integer-discriminant storage is fragile in two ways:
//!   1. Reordering variants is a silent migration. A `Role::Admin = 0`
//!      that later becomes `Role::Admin = 2` after a re-order would
//!      silently swap which rows are admins.
//!   2. Schema diffs across deployments are noisy. `0` / `1` / `2`
//!      tells you nothing in a DB browser; `"Admin"` is self-describing.
//!
//! Variant name storage is the same convention Laravel uses for its
//! enum casts and is what almost every real-world Eloquent model picks.

use std::marker::PhantomData;
use std::str::FromStr;

use super::{Cast, DynCast, IntoDynCast};
use crate::error::FrameworkError;

/// Cast a `FromStr + AsRef<str>` enum ↔ `TEXT`. The enum's variant
/// name (or its `AsRefStr`-customised string) is what hits the column.
pub struct AsEnum<E>(PhantomData<E>);

impl<E> Cast for AsEnum<E>
where
    E: FromStr + AsRef<str> + Send + Sync,
    <E as FromStr>::Err: std::fmt::Display,
{
    type Runtime = E;
    type Storage = String;

    fn to_storage(v: &E) -> Result<String, FrameworkError> {
        Ok(v.as_ref().to_string())
    }

    fn from_storage(s: &String) -> Result<E, FrameworkError> {
        E::from_str(s).map_err(|e| FrameworkError::validation("AsEnum", format!("{e}")))
    }
}

struct AsEnumDyn<E>(PhantomData<E>);

impl<E> DynCast for AsEnumDyn<E>
where
    E: FromStr + AsRef<str> + Send + Sync + 'static,
    <E as FromStr>::Err: std::fmt::Display,
{
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        let s = v.as_str().unwrap_or("");
        let parsed = AsEnum::<E>::from_storage(&s.to_string())?;
        Ok(serde_json::Value::String(parsed.as_ref().to_string()))
    }

    fn to_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        Ok(v.clone())
    }
}

impl<E> IntoDynCast for AsEnum<E>
where
    E: FromStr + AsRef<str> + Send + Sync + 'static,
    <E as FromStr>::Err: std::fmt::Display,
{
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsEnumDyn::<E>(PhantomData))
    }
}
