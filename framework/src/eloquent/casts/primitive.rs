//! Primitive casts — boolean, integer, float, decimal, string.
//!
//! Each cast declares its `Runtime` (the Rust type the user works
//! with in their model struct) and `Storage` (the type SeaORM
//! materialises from the column). T7b builds on these with the
//! structured + enum + dynamic-override surface; T7c layers
//! encryption / hashing on top.
//!
//! ## Boolean storage convention
//!
//! `AsBool` round-trips through `i64` because SQLite has no native
//! BOOLEAN — it stores booleans as integers 0/1. Postgres / MySQL
//! `BOOLEAN` columns accept the same i64 over the wire, so a single
//! storage shape covers every backend without driver branching.
//!
//! ## Decimal storage convention
//!
//! `AsDecimal<P>` stores as `String` rather than a native fixed-point
//! type because SeaORM's `Decimal` column-type lacks consistent
//! precision across backends; storing the user-facing string
//! representation keeps round-trip fidelity. The `P` const generic is
//! the number of decimal places to round to before storage.

use std::marker::PhantomData;

use super::{Cast, DynCast, IntoDynCast};
use crate::error::FrameworkError;

// ---- AsBool ---------------------------------------------------------------

/// Cast `bool` ↔ `INTEGER` (0/1). The single-backend-compatible storage
/// shape for booleans — every SQL backend round-trips i64 cleanly.
pub struct AsBool;

impl Cast for AsBool {
    type Runtime = bool;
    type Storage = i64;

    fn to_storage(v: &bool) -> Result<i64, FrameworkError> {
        Ok(if *v { 1 } else { 0 })
    }

    fn from_storage(s: &i64) -> Result<bool, FrameworkError> {
        Ok(*s != 0)
    }
}

struct AsBoolDyn;

impl DynCast for AsBoolDyn {
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        // Domain 7 audit D7-A — strict-validate the input shape so a
        // misconfigured column produces a clear "expected integer, got
        // <value>" message instead of silently coercing to `false`.
        let n = v.as_i64().or_else(|| v.as_u64().map(|x| x as i64)).ok_or_else(|| {
            FrameworkError::validation(
                "AsBool",
                format!("dyn from_storage: expected integer, got {v:?}"),
            )
        })?;
        Ok(serde_json::Value::Bool(n != 0))
    }

    fn to_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        let b = v.as_bool().ok_or_else(|| {
            FrameworkError::validation(
                "AsBool",
                format!("dyn to_storage: expected boolean, got {v:?}"),
            )
        })?;
        Ok(serde_json::Value::Number(if b { 1.into() } else { 0.into() }))
    }
}

impl IntoDynCast for AsBool {
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsBoolDyn)
    }
}

// ---- AsInt<I> --------------------------------------------------------------

/// Cast a narrower integer type (e.g. `i32`, `u32`, `i16`) ↔ `i64`.
/// SeaORM stores integers as i64 on the column; the cast narrows on
/// read and widens on write. Out-of-range values produce a validation
/// error at read time rather than silently truncating.
pub struct AsInt<I = i64>(PhantomData<I>);

impl<I> Cast for AsInt<I>
where
    I: TryFrom<i64> + Into<i64> + Copy + Send + Sync,
    <I as TryFrom<i64>>::Error: std::fmt::Display,
{
    type Runtime = I;
    type Storage = i64;

    fn to_storage(v: &I) -> Result<i64, FrameworkError> {
        Ok((*v).into())
    }

    fn from_storage(s: &i64) -> Result<I, FrameworkError> {
        I::try_from(*s)
            .map_err(|e| FrameworkError::validation("AsInt", format!("conversion failed: {e}")))
    }
}

struct AsInt64Dyn;

impl DynCast for AsInt64Dyn {
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        let n = v
            .as_i64()
            .ok_or_else(|| FrameworkError::validation("AsInt", "not an integer"))?;
        Ok(serde_json::Value::Number(n.into()))
    }

    fn to_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        Ok(v.clone())
    }
}

impl IntoDynCast for AsInt<i64> {
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsInt64Dyn)
    }
}

// Narrower-int variants get their own IntoDynCast specialisations when
// users need them; `AsInt<i64>` covers the common case and is the
// form the `casts!` macro resolves to with no width annotation.

// ---- AsFloat --------------------------------------------------------------

/// Cast `f64` ↔ `REAL`. Pass-through both directions — the cast exists
/// for parity with Laravel's `'float'` cast name; backends already
/// round-trip floats natively.
pub struct AsFloat;

impl Cast for AsFloat {
    type Runtime = f64;
    type Storage = f64;

    fn to_storage(v: &f64) -> Result<f64, FrameworkError> {
        Ok(*v)
    }

    fn from_storage(s: &f64) -> Result<f64, FrameworkError> {
        Ok(*s)
    }
}

struct AsFloatDyn;

impl DynCast for AsFloatDyn {
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        let f = v
            .as_f64()
            .or_else(|| v.as_i64().map(|i| i as f64))
            .ok_or_else(|| FrameworkError::validation("AsFloat", "not a number"))?;
        Ok(serde_json::json!(f))
    }

    fn to_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        Ok(v.clone())
    }
}

impl IntoDynCast for AsFloat {
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsFloatDyn)
    }
}

// ---- AsString --------------------------------------------------------------

/// Cast `String` ↔ `TEXT`. Pass-through; exists for parity with the
/// Laravel cast surface and so `with_casts(...)` can erase it to a
/// `DynCast` like every other cast.
pub struct AsString;

impl Cast for AsString {
    type Runtime = String;
    type Storage = String;

    fn to_storage(v: &String) -> Result<String, FrameworkError> {
        Ok(v.clone())
    }

    fn from_storage(s: &String) -> Result<String, FrameworkError> {
        Ok(s.clone())
    }
}

struct AsStringDyn;

impl DynCast for AsStringDyn {
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        Ok(v.clone())
    }

    fn to_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        Ok(v.clone())
    }
}

impl IntoDynCast for AsString {
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsStringDyn)
    }
}

// ---- AsDecimal<P> ----------------------------------------------------------

/// Cast `rust_decimal::Decimal` ↔ `TEXT`. `P` is the precision (number
/// of decimal places); values are rounded to `P` places on the way to
/// storage. Storage is a fixed-format string so the round-trip is
/// backend-agnostic.
pub struct AsDecimal<const PRECISION: u32 = 4>;

impl<const P: u32> Cast for AsDecimal<P> {
    type Runtime = rust_decimal::Decimal;
    type Storage = String;

    fn to_storage(v: &rust_decimal::Decimal) -> Result<String, FrameworkError> {
        Ok(v.round_dp(P).to_string())
    }

    fn from_storage(s: &String) -> Result<rust_decimal::Decimal, FrameworkError> {
        s.parse::<rust_decimal::Decimal>()
            .map_err(|e| FrameworkError::validation("AsDecimal", format!("parse: {e}")))
    }
}

struct AsDecimalDyn<const P: u32>;

impl<const P: u32> DynCast for AsDecimalDyn<P> {
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        // Domain 7 audit D7-A — was `v.as_str().unwrap_or("0")` which
        // silently coerced non-strings to "0" and returned 0.00 without
        // surfacing the type mismatch. Now strict.
        let s = v
            .as_str()
            .ok_or_else(|| {
                FrameworkError::validation(
                    "AsDecimal",
                    format!("dyn from_storage: expected JSON string, got {v:?}"),
                )
            })?
            .to_string();
        let d = AsDecimal::<P>::from_storage(&s)?;
        Ok(serde_json::Value::String(d.to_string()))
    }

    fn to_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        Ok(v.clone())
    }
}

impl<const P: u32> IntoDynCast for AsDecimal<P> {
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsDecimalDyn::<P>)
    }
}

#[cfg(test)]
mod tests {
    //! Domain 7 audit D7-A regression — the dyn-cast layer used to
    //! silently coerce malformed JSON input ("" / "0" / `false`) before
    //! attempting to parse, producing cryptic downstream errors. The
    //! fix makes every dyn cast surface an explicit type-mismatch
    //! diagnostic so a misconfigured column produces a clear "expected
    //! JSON <type>, got <actual>" error.
    //!
    //! These tests assert ONE representative misshape per cast — full
    //! happy-path coverage lives in
    //! `framework/tests/eloquent_casts_*.rs`.
    use super::*;
    use serde_json::json;

    #[test]
    fn as_bool_from_storage_rejects_non_integer() {
        let dyn_cast = AsBool::into_dyn();
        let err = dyn_cast
            .from_storage_json(&json!("true"))
            .err()
            .expect("non-integer input must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("AsBool") && msg.contains("expected integer"),
            "error must name the cast + expected shape; got: {msg}",
        );
    }

    #[test]
    fn as_bool_to_storage_rejects_non_boolean() {
        let dyn_cast = AsBool::into_dyn();
        let err = dyn_cast
            .to_storage_json(&json!(1))
            .err()
            .expect("non-boolean input must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("AsBool") && msg.contains("expected boolean"),
            "error must name the cast + expected shape; got: {msg}",
        );
    }

    #[test]
    fn as_decimal_from_storage_rejects_non_string() {
        let dyn_cast = <AsDecimal<4> as IntoDynCast>::into_dyn();
        let err = dyn_cast
            .from_storage_json(&json!(42))
            .err()
            .expect("non-string input must reject (was silently coerced to '0')");
        let msg = format!("{err}");
        assert!(
            msg.contains("AsDecimal") && msg.contains("expected JSON string"),
            "error must name the cast + expected shape; got: {msg}",
        );
    }
}
