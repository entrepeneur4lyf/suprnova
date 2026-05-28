//! Cursor paginator — keyset-style pagination with encrypted cursors.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;

use crate::FrameworkError;
use crate::crypto::Crypt;

/// Direction a cursor advances in. The first page always uses
/// [`CursorDirection::Next`] implicitly (the caller passes `None`).
/// Page-to-page cursors carry their direction in the wire payload so
/// `Pagination::cursor` knows whether to filter `gt`/asc (next) or
/// `lt`/desc (prev).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorDirection {
    /// Cursor identifies the upper boundary already shown; the next
    /// page is the strictly greater rows.
    Next,
    /// Cursor identifies the lower boundary already shown; the previous
    /// page is the strictly lesser rows.
    Prev,
}

impl CursorDirection {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            CursorDirection::Next => "next",
            CursorDirection::Prev => "prev",
        }
    }

    pub(crate) fn from_str(s: &str) -> Result<Self, FrameworkError> {
        match s {
            "next" => Ok(CursorDirection::Next),
            "prev" => Ok(CursorDirection::Prev),
            other => Err(FrameworkError::internal(format!(
                "Cursor direction must be 'next' or 'prev', got '{other}'"
            ))),
        }
    }
}

/// Paginator that emits opaque cursor strings instead of page numbers.
///
/// Equivalent to Laravel's `CursorPaginator`. Returned by
/// [`Pagination::cursor`](crate::pagination::Pagination::cursor) and by
/// [`Builder::cursor_paginate`](crate::eloquent::Builder::cursor_paginate).
///
/// The boundary value carried in `next_cursor` / `prev_cursor` is the
/// last (or first) row's primary-sort column, encoded as a typed
/// SeaORM [`sea_orm::Value`] so dialects (Postgres, MySQL, SQLite)
/// receive the correctly-typed bind without any string coercion.
///
/// ## JSON shape
///
/// ```json
/// {
///   "data": [...],
///   "per_page": 10,
///   "next_cursor": "...",
///   "prev_cursor": null,
///   "path": "/api/users"
/// }
/// ```
///
/// `path` is omitted when unset; `next_cursor` and `prev_cursor` are
/// emitted as `null` (not omitted) so client schemas can rely on the
/// field's presence.
#[derive(Debug, Clone, Serialize)]
pub struct CursorPaginator<T> {
    /// The rows on this page.
    pub data: Vec<T>,
    /// Page size used to fetch this page. Mirrored from the call to
    /// [`Builder::cursor_paginate`](crate::eloquent::Builder::cursor_paginate)
    /// — useful when clients want to thread `?per_page=N` for parity
    /// with offset pagination.
    pub per_page: u64,
    /// Cursor to fetch the next page, or `None` at the last page.
    pub next_cursor: Option<String>,
    /// Cursor to fetch the previous page, or `None` on the first page
    /// (when the caller passed `cursor: None`).
    pub prev_cursor: Option<String>,
    /// Optional base URL — clients that build full pagination URLs out
    /// of `next_cursor` / `prev_cursor` use this as the path prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl<T> CursorPaginator<T> {
    /// Build a cursor paginator from its parts. `per_page` records the
    /// page size the caller asked for; `path` defaults to `None`.
    pub fn new(
        data: Vec<T>,
        per_page: u64,
        next_cursor: Option<String>,
        prev_cursor: Option<String>,
    ) -> Self {
        Self {
            data,
            per_page,
            next_cursor,
            prev_cursor,
            path: None,
        }
    }

    /// Set the optional base URL for the paginator. Returns `self` for
    /// builder-style chaining.
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

/// Wire envelope serialized into the cursor before encryption /
/// base64.
///
/// `t` is the SeaORM `Value` variant discriminator — exactly the
/// variant name (`"Int"`, `"BigInt"`, `"Uuid"`,
/// `"ChronoDateTimeUtc"`, etc.) — so the decoded `Value` re-binds with
/// the same SQL type the original column emitted. `v` is the value,
/// JSON-serialized in the natural form for that variant. `d` is the
/// scan direction (`"next"` or `"prev"`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct CursorPayload {
    pub t: String,
    pub v: serde_json::Value,
    pub d: String,
}

impl<T> CursorPaginator<T> {
    /// Encode a typed boundary `sea_orm::Value` plus scan direction
    /// into the wire cursor. The cursor is AES-256-GCM authenticated
    /// — `Crypt` must be initialized (the framework guarantees this
    /// via `Server::from_config` at boot).
    ///
    /// Direct callers (controllers that build cursors outside
    /// `Pagination::cursor`) use this to produce a typed cursor over
    /// a non-string boundary — pass a `Value::BigInt(...)`,
    /// `Value::Uuid(...)`, etc. and `Pagination::cursor` will
    /// re-bind the same SQL type on decode.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the SeaORM variant isn't a supported cursor
    /// boundary or if `Crypt` is not initialized (defensive — should
    /// be impossible after `Server::from_config`). Codex review
    /// finding #1: never emit an unsigned/forgeable cursor payload.
    pub fn encode_value(
        value: &sea_orm::Value,
        direction: CursorDirection,
    ) -> Result<String, FrameworkError> {
        let (t, v) = value_to_tagged_json(value)?;
        let payload = CursorPayload {
            t,
            v,
            d: direction.as_str().to_string(),
        };
        let json = serde_json::to_string(&payload).map_err(|e| {
            FrameworkError::internal(format!("Cursor payload JSON encode failed: {e}"))
        })?;
        // `Crypt::encrypt_string` returns Err when Crypt isn't
        // initialized — propagate verbatim. No plaintext base64
        // fallback (that branch was the codex-flagged fail-open path).
        Crypt::encrypt_string(&json)
    }

    /// Decode the wire cursor into a typed `sea_orm::Value` plus the
    /// scan direction it was emitted with.
    ///
    /// Cursors must be authenticated — there is no plaintext fallback
    /// even if `Crypt` is not initialized (which would itself be a
    /// boot bug). Any attempt to decode an unsigned base64 payload
    /// errors. Codex review finding #1.
    pub fn decode_value(wire: &str) -> Result<(sea_orm::Value, CursorDirection), FrameworkError> {
        let json = Crypt::decrypt_string(wire)?;
        let payload: CursorPayload = serde_json::from_str(&json).map_err(|e| {
            FrameworkError::internal(format!("Cursor payload JSON decode failed: {e}"))
        })?;
        let value = tagged_json_to_value(&payload.t, payload.v)?;
        let direction = CursorDirection::from_str(&payload.d)?;
        Ok((value, direction))
    }

    /// Encode a cursor boundary as a plain string. **Legacy helper**
    /// preserved only so callers that manually wrap a string cursor
    /// (e.g. controllers that don't go through `Pagination::cursor`)
    /// keep working. New code should use `Pagination::cursor` directly
    /// — the typed cursor encoding is automatic.
    ///
    /// Internally this calls [`Self::encode_value`] with a
    /// `Value::String` variant and `CursorDirection::Next`.
    ///
    /// # Panics
    ///
    /// Panics if `Crypt` is not initialized. The framework guarantees
    /// initialization in `Server::from_config`; if it isn't, the
    /// process never reached steady-state and emitting an unsigned
    /// cursor would be a security bug. For a non-panicking form — in
    /// library code, or anywhere outside the server's post-boot request
    /// path where the `Crypt`-initialized invariant is not guaranteed —
    /// use [`Self::try_encode_cursor`].
    pub fn encode_cursor(value: &str) -> String {
        Self::try_encode_cursor(value).expect(
            "Crypt invariant: cursors must be encrypted. \
             Initialize via Server::from_config (sets APP_KEY-derived key).",
        )
    }

    /// Fallible sibling of [`Self::encode_cursor`] — returns `Err`
    /// instead of panicking when `Crypt` is not initialized. Prefer
    /// this anywhere the post-boot `Crypt` invariant is not guaranteed;
    /// it follows the framework's `try_*` convention for fallible
    /// operations that carry an infallible Laravel-style name.
    pub fn try_encode_cursor(value: &str) -> Result<String, FrameworkError> {
        Self::encode_value(
            &sea_orm::Value::String(Some(Box::new(value.to_string()))),
            CursorDirection::Next,
        )
    }

    /// Decode a cursor produced by [`Self::encode_cursor`] /
    /// [`Self::try_encode_cursor`] back to its string payload.
    /// **Legacy helper** — see [`Self::encode_cursor`].
    ///
    /// Errors when the wire cursor decodes to a non-`String` typed
    /// boundary (e.g. a `BigInt` cursor emitted by the typed
    /// [`Self::encode_value`] path). The legacy String helper used to
    /// Debug-stringify such a value, silently hiding the type mismatch;
    /// it now surfaces the mismatch so callers reach for
    /// [`Self::decode_value`] when decoding typed cursors.
    pub fn decode_cursor(wire: &str) -> Result<String, FrameworkError> {
        let (value, _dir) = Self::decode_value(wire)?;
        match value {
            sea_orm::Value::String(Some(s)) => Ok(*s),
            sea_orm::Value::String(None) => Ok(String::new()),
            other => {
                // Name the variant (a type tag, not the value) so the
                // mismatch is diagnosable without leaking cursor contents.
                let variant = value_to_tagged_json(&other)
                    .map(|(tag, _)| tag)
                    .unwrap_or_else(|_| "unknown".to_string());
                Err(FrameworkError::internal(format!(
                    "decode_cursor: expected a String cursor (as produced by \
                     encode_cursor / try_encode_cursor), got a {variant} cursor. \
                     Use CursorPaginator::decode_value to decode typed cursors."
                )))
            }
        }
    }
}

/// Convert a SeaORM `Value` into the cursor wire shape. Returns the
/// variant discriminator string plus a JSON value.
fn value_to_tagged_json(v: &sea_orm::Value) -> Result<(String, serde_json::Value), FrameworkError> {
    use sea_orm::Value;
    let pair: (&'static str, serde_json::Value) = match v {
        Value::Bool(Some(b)) => ("Bool", serde_json::json!(b)),
        Value::Bool(None) => ("Bool", serde_json::Value::Null),
        Value::TinyInt(Some(i)) => ("TinyInt", serde_json::json!(i)),
        Value::TinyInt(None) => ("TinyInt", serde_json::Value::Null),
        Value::SmallInt(Some(i)) => ("SmallInt", serde_json::json!(i)),
        Value::SmallInt(None) => ("SmallInt", serde_json::Value::Null),
        Value::Int(Some(i)) => ("Int", serde_json::json!(i)),
        Value::Int(None) => ("Int", serde_json::Value::Null),
        Value::BigInt(Some(i)) => ("BigInt", serde_json::json!(i)),
        Value::BigInt(None) => ("BigInt", serde_json::Value::Null),
        Value::TinyUnsigned(Some(i)) => ("TinyUnsigned", serde_json::json!(i)),
        Value::TinyUnsigned(None) => ("TinyUnsigned", serde_json::Value::Null),
        Value::SmallUnsigned(Some(i)) => ("SmallUnsigned", serde_json::json!(i)),
        Value::SmallUnsigned(None) => ("SmallUnsigned", serde_json::Value::Null),
        Value::Unsigned(Some(i)) => ("Unsigned", serde_json::json!(i)),
        Value::Unsigned(None) => ("Unsigned", serde_json::Value::Null),
        Value::BigUnsigned(Some(i)) => ("BigUnsigned", serde_json::json!(i)),
        Value::BigUnsigned(None) => ("BigUnsigned", serde_json::Value::Null),
        Value::Float(Some(f)) => ("Float", serde_json::json!(f)),
        Value::Float(None) => ("Float", serde_json::Value::Null),
        Value::Double(Some(f)) => ("Double", serde_json::json!(f)),
        Value::Double(None) => ("Double", serde_json::Value::Null),
        Value::String(Some(s)) => ("String", serde_json::json!(**s)),
        Value::String(None) => ("String", serde_json::Value::Null),
        Value::Char(Some(c)) => ("Char", serde_json::json!(c.to_string())),
        Value::Char(None) => ("Char", serde_json::Value::Null),
        Value::Bytes(Some(b)) => (
            "Bytes",
            serde_json::json!(URL_SAFE_NO_PAD.encode(b.as_slice())),
        ),
        Value::Bytes(None) => ("Bytes", serde_json::Value::Null),
        Value::Uuid(Some(u)) => ("Uuid", serde_json::json!(u.to_string())),
        Value::Uuid(None) => ("Uuid", serde_json::Value::Null),
        Value::ChronoDate(Some(d)) => ("ChronoDate", serde_json::json!(d.to_string())),
        Value::ChronoDate(None) => ("ChronoDate", serde_json::Value::Null),
        Value::ChronoTime(Some(t)) => ("ChronoTime", serde_json::json!(t.to_string())),
        Value::ChronoTime(None) => ("ChronoTime", serde_json::Value::Null),
        Value::ChronoDateTime(Some(dt)) => ("ChronoDateTime", serde_json::json!(dt.to_string())),
        Value::ChronoDateTime(None) => ("ChronoDateTime", serde_json::Value::Null),
        Value::ChronoDateTimeUtc(Some(dt)) => (
            "ChronoDateTimeUtc",
            serde_json::json!(dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)),
        ),
        Value::ChronoDateTimeUtc(None) => ("ChronoDateTimeUtc", serde_json::Value::Null),
        Value::ChronoDateTimeLocal(Some(dt)) => (
            "ChronoDateTimeLocal",
            serde_json::json!(dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)),
        ),
        Value::ChronoDateTimeLocal(None) => ("ChronoDateTimeLocal", serde_json::Value::Null),
        Value::ChronoDateTimeWithTimeZone(Some(dt)) => (
            "ChronoDateTimeWithTimeZone",
            serde_json::json!(dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)),
        ),
        Value::ChronoDateTimeWithTimeZone(None) => {
            ("ChronoDateTimeWithTimeZone", serde_json::Value::Null)
        }
        Value::Decimal(Some(d)) => ("Decimal", serde_json::json!(d.to_string())),
        Value::Decimal(None) => ("Decimal", serde_json::Value::Null),
        Value::BigDecimal(Some(d)) => ("BigDecimal", serde_json::json!(d.to_string())),
        Value::BigDecimal(None) => ("BigDecimal", serde_json::Value::Null),
        other => {
            return Err(FrameworkError::internal(format!(
                "Cursor: SeaORM Value variant {other:?} is not supported as a cursor \
                 boundary. Use a column whose type maps to a scalar variant \
                 (integers, floats, bool, string, bytes, uuid, datetime, decimal)."
            )));
        }
    };
    Ok((pair.0.to_string(), pair.1))
}

/// Inverse of [`value_to_tagged_json`]. Validates that JSON shape
/// matches the claimed discriminator.
fn tagged_json_to_value(tag: &str, v: serde_json::Value) -> Result<sea_orm::Value, FrameworkError> {
    use sea_orm::Value;
    let bad = |what: &str| {
        FrameworkError::internal(format!(
            "Cursor: tag '{tag}' payload could not be parsed as {what}"
        ))
    };
    if v.is_null() {
        return Ok(match tag {
            "Bool" => Value::Bool(None),
            "TinyInt" => Value::TinyInt(None),
            "SmallInt" => Value::SmallInt(None),
            "Int" => Value::Int(None),
            "BigInt" => Value::BigInt(None),
            "TinyUnsigned" => Value::TinyUnsigned(None),
            "SmallUnsigned" => Value::SmallUnsigned(None),
            "Unsigned" => Value::Unsigned(None),
            "BigUnsigned" => Value::BigUnsigned(None),
            "Float" => Value::Float(None),
            "Double" => Value::Double(None),
            "String" => Value::String(None),
            "Char" => Value::Char(None),
            "Bytes" => Value::Bytes(None),
            "Uuid" => Value::Uuid(None),
            "ChronoDate" => Value::ChronoDate(None),
            "ChronoTime" => Value::ChronoTime(None),
            "ChronoDateTime" => Value::ChronoDateTime(None),
            "ChronoDateTimeUtc" => Value::ChronoDateTimeUtc(None),
            "ChronoDateTimeLocal" => Value::ChronoDateTimeLocal(None),
            "ChronoDateTimeWithTimeZone" => Value::ChronoDateTimeWithTimeZone(None),
            "Decimal" => Value::Decimal(None),
            "BigDecimal" => Value::BigDecimal(None),
            other => {
                return Err(FrameworkError::internal(format!(
                    "Cursor: unknown variant tag '{other}'"
                )));
            }
        });
    }

    match tag {
        "Bool" => v
            .as_bool()
            .map(|b| Value::Bool(Some(b)))
            .ok_or_else(|| bad("bool")),
        "TinyInt" => v
            .as_i64()
            .and_then(|i| i8::try_from(i).ok())
            .map(|i| Value::TinyInt(Some(i)))
            .ok_or_else(|| bad("i8")),
        "SmallInt" => v
            .as_i64()
            .and_then(|i| i16::try_from(i).ok())
            .map(|i| Value::SmallInt(Some(i)))
            .ok_or_else(|| bad("i16")),
        "Int" => v
            .as_i64()
            .and_then(|i| i32::try_from(i).ok())
            .map(|i| Value::Int(Some(i)))
            .ok_or_else(|| bad("i32")),
        "BigInt" => v
            .as_i64()
            .map(|i| Value::BigInt(Some(i)))
            .ok_or_else(|| bad("i64")),
        "TinyUnsigned" => v
            .as_u64()
            .and_then(|i| u8::try_from(i).ok())
            .map(|i| Value::TinyUnsigned(Some(i)))
            .ok_or_else(|| bad("u8")),
        "SmallUnsigned" => v
            .as_u64()
            .and_then(|i| u16::try_from(i).ok())
            .map(|i| Value::SmallUnsigned(Some(i)))
            .ok_or_else(|| bad("u16")),
        "Unsigned" => v
            .as_u64()
            .and_then(|i| u32::try_from(i).ok())
            .map(|i| Value::Unsigned(Some(i)))
            .ok_or_else(|| bad("u32")),
        "BigUnsigned" => v
            .as_u64()
            .map(|i| Value::BigUnsigned(Some(i)))
            .ok_or_else(|| bad("u64")),
        "Float" => v
            .as_f64()
            .map(|f| Value::Float(Some(f as f32)))
            .ok_or_else(|| bad("f32")),
        "Double" => v
            .as_f64()
            .map(|f| Value::Double(Some(f)))
            .ok_or_else(|| bad("f64")),
        "String" => v
            .as_str()
            .map(|s| Value::String(Some(Box::new(s.to_string()))))
            .ok_or_else(|| bad("string")),
        "Char" => v
            .as_str()
            .and_then(|s| {
                let mut it = s.chars();
                let c = it.next()?;
                if it.next().is_none() { Some(c) } else { None }
            })
            .map(|c| Value::Char(Some(c)))
            .ok_or_else(|| bad("char")),
        "Bytes" => v
            .as_str()
            .and_then(|s| URL_SAFE_NO_PAD.decode(s).ok())
            .map(|b| Value::Bytes(Some(Box::new(b))))
            .ok_or_else(|| bad("base64-bytes")),
        "Uuid" => v
            .as_str()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .map(|u| Value::Uuid(Some(Box::new(u))))
            .ok_or_else(|| bad("uuid")),
        "ChronoDate" => v
            .as_str()
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .map(|d| Value::ChronoDate(Some(Box::new(d))))
            .ok_or_else(|| bad("chrono::NaiveDate")),
        "ChronoTime" => v
            .as_str()
            .and_then(|s| {
                chrono::NaiveTime::parse_from_str(s, "%H:%M:%S%.f")
                    .or_else(|_| chrono::NaiveTime::parse_from_str(s, "%H:%M:%S"))
                    .ok()
            })
            .map(|t| Value::ChronoTime(Some(Box::new(t))))
            .ok_or_else(|| bad("chrono::NaiveTime")),
        "ChronoDateTime" => v
            .as_str()
            .and_then(|s| {
                chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
                    .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f"))
                    .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
                    .ok()
            })
            .map(|dt| Value::ChronoDateTime(Some(Box::new(dt))))
            .ok_or_else(|| bad("chrono::NaiveDateTime")),
        "ChronoDateTimeUtc" => v
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| Value::ChronoDateTimeUtc(Some(Box::new(dt.with_timezone(&chrono::Utc)))))
            .ok_or_else(|| bad("chrono::DateTime<Utc>")),
        "ChronoDateTimeLocal" => v
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| Value::ChronoDateTimeLocal(Some(Box::new(dt.with_timezone(&chrono::Local)))))
            .ok_or_else(|| bad("chrono::DateTime<Local>")),
        "ChronoDateTimeWithTimeZone" => v
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| Value::ChronoDateTimeWithTimeZone(Some(Box::new(dt))))
            .ok_or_else(|| bad("chrono::DateTime<FixedOffset>")),
        "Decimal" => v
            .as_str()
            .and_then(|s| s.parse::<rust_decimal::Decimal>().ok())
            .map(|d| Value::Decimal(Some(Box::new(d))))
            .ok_or_else(|| bad("rust_decimal::Decimal")),
        "BigDecimal" => v
            .as_str()
            .and_then(|s| {
                use std::str::FromStr;
                bigdecimal::BigDecimal::from_str(s).ok()
            })
            .map(|d| Value::BigDecimal(Some(Box::new(d))))
            .ok_or_else(|| bad("bigdecimal::BigDecimal")),
        other => Err(FrameworkError::internal(format!(
            "Cursor: unknown variant tag '{other}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cursor tests share Crypt state with the encryption suite. We use
    // the same install-once pattern; either suite may install first.
    use std::sync::Mutex;
    static CURSOR_LOCK: Mutex<()> = Mutex::new(());

    fn ensure_key() {
        // _test_install_key returns false if a key is already present —
        // that's fine; we just need *some* key in the OnceLock.
        let _ = crate::crypto::_test_install_key(crate::EncryptionKey::generate());
    }

    #[test]
    fn encrypted_cursor_round_trip_string_legacy_api() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let wire = CursorPaginator::<i32>::encode_cursor("user-42");
        // With Crypt active, cursor is opaque (not equal to base64 of plaintext)
        let plain_b64 = URL_SAFE_NO_PAD.encode(b"user-42");
        assert_ne!(wire, plain_b64);
        let decoded = CursorPaginator::<i32>::decode_cursor(&wire).unwrap();
        assert_eq!(decoded, "user-42");
    }

    #[test]
    fn try_encode_cursor_round_trips_via_decode_cursor() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let wire = CursorPaginator::<i32>::try_encode_cursor("user-7").unwrap();
        assert_eq!(
            CursorPaginator::<i32>::decode_cursor(&wire).unwrap(),
            "user-7"
        );
    }

    #[test]
    fn decode_cursor_errors_on_non_string_typed_cursor() {
        // A typed (non-String) cursor — e.g. one produced by the typed
        // `encode_value` path — must NOT silently Debug-stringify through
        // the legacy String helper; it errors so a type mismatch surfaces.
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let wire = CursorPaginator::<i32>::encode_value(
            &sea_orm::Value::BigInt(Some(42)),
            CursorDirection::Next,
        )
        .unwrap();
        assert!(CursorPaginator::<i32>::decode_cursor(&wire).is_err());
    }

    #[test]
    fn cursor_decode_rejects_plain_base64_when_crypt_initialized() {
        // Security regression: when Crypt has a key, an attacker-
        // crafted plain-base64 cursor MUST be rejected.
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let attacker = URL_SAFE_NO_PAD.encode(br#"{"t":"BigInt","v":42,"d":"next"}"#);
        assert!(CursorPaginator::<i32>::decode_value(&attacker).is_err());
    }

    #[test]
    fn cursor_decode_rejects_garbage() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        assert!(CursorPaginator::<i32>::decode_value("!!! not base64 !!!").is_err());
    }

    #[test]
    fn value_bigint_round_trip() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let v = sea_orm::Value::BigInt(Some(9_876_543_210_i64));
        let wire = CursorPaginator::<i32>::encode_value(&v, CursorDirection::Next).unwrap();
        let (got, dir) = CursorPaginator::<i32>::decode_value(&wire).unwrap();
        assert!(matches!(got, sea_orm::Value::BigInt(Some(n)) if n == 9_876_543_210));
        assert_eq!(dir, CursorDirection::Next);
    }

    #[test]
    fn value_int32_round_trip_preserves_variant() {
        // Important: encoding an Int(i32) must decode back as Int —
        // not BigInt — or Postgres int4 columns will see the wrong bind.
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let v = sea_orm::Value::Int(Some(42_i32));
        let wire = CursorPaginator::<i32>::encode_value(&v, CursorDirection::Next).unwrap();
        let (got, _dir) = CursorPaginator::<i32>::decode_value(&wire).unwrap();
        assert!(
            matches!(got, sea_orm::Value::Int(Some(n)) if n == 42_i32),
            "expected Int(42), got {got:?}"
        );
    }

    #[test]
    fn value_uuid_round_trip() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let u = uuid::Uuid::from_u128(0x1234_5678_90ab_cdef_fedc_ba09_8765_4321_u128);
        let v = sea_orm::Value::Uuid(Some(Box::new(u)));
        let wire = CursorPaginator::<i32>::encode_value(&v, CursorDirection::Prev).unwrap();
        let (got, dir) = CursorPaginator::<i32>::decode_value(&wire).unwrap();
        match got {
            sea_orm::Value::Uuid(Some(decoded)) => assert_eq!(*decoded, u),
            other => panic!("expected Uuid, got {other:?}"),
        }
        assert_eq!(dir, CursorDirection::Prev);
    }

    #[test]
    fn value_datetime_utc_round_trip() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let dt: chrono::DateTime<chrono::Utc> =
            chrono::DateTime::parse_from_rfc3339("2026-05-14T18:30:00.123456789Z")
                .unwrap()
                .with_timezone(&chrono::Utc);
        let v = sea_orm::Value::ChronoDateTimeUtc(Some(Box::new(dt)));
        let wire = CursorPaginator::<i32>::encode_value(&v, CursorDirection::Next).unwrap();
        let (got, _dir) = CursorPaginator::<i32>::decode_value(&wire).unwrap();
        match got {
            sea_orm::Value::ChronoDateTimeUtc(Some(decoded)) => assert_eq!(*decoded, dt),
            other => panic!("expected ChronoDateTimeUtc, got {other:?}"),
        }
    }

    #[test]
    fn value_string_round_trip() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let v = sea_orm::Value::String(Some(Box::new("sn-1@example.com".to_string())));
        let wire = CursorPaginator::<i32>::encode_value(&v, CursorDirection::Next).unwrap();
        let (got, _dir) = CursorPaginator::<i32>::decode_value(&wire).unwrap();
        match got {
            sea_orm::Value::String(Some(s)) => assert_eq!(*s, "sn-1@example.com"),
            other => panic!("expected Value::String, got {other:?}"),
        }
    }

    #[test]
    fn value_unknown_tag_rejected() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let bad = r#"{"t":"NotAVariant","v":42,"d":"next"}"#;
        let wire = Crypt::encrypt_string(bad).unwrap();
        assert!(CursorPaginator::<i32>::decode_value(&wire).is_err());
    }

    #[test]
    fn value_direction_tampering_rejected() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let bad = r#"{"t":"BigInt","v":1,"d":"sideways"}"#;
        let wire = Crypt::encrypt_string(bad).unwrap();
        assert!(CursorPaginator::<i32>::decode_value(&wire).is_err());
    }
}
