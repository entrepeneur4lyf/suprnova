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
///
/// This shape is **not** identical to Laravel's
/// `CursorPaginator::toArray()` — Laravel additionally emits
/// `next_page_url` and `prev_page_url` (absolute URLs derived from
/// `path` + cursor). Suprnova routes URL generation through the
/// response-shape constructors that own URL context:
/// [`Inertia::paginate`](crate::inertia::Inertia::paginate) (cursor
/// scroll metadata) and
/// [`Resource::paginated`](crate::resources::Resource::paginated)
/// (JSON:API `links.{prev,next}` via
/// [`Paginated`](crate::pagination::Paginated)). The raw `Serialize`
/// shape stays minimal for explicit-shape consumers.
///
/// Laravel's `Cursor::encode()` is a base64-JSON plaintext payload;
/// Suprnova's cursor is AES-256-GCM encrypted via `Crypt` so the
/// keyset boundary can't be tampered with by the client. See
/// [`Self::encode_value`] / [`Self::decode_value`].
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
    /// Query-string parameter name the JSON:API link builder uses when
    /// constructing the `next`/`prev` cursor URLs. `None` resolves to
    /// `"cursor"` — the key [`Builder::cursor_paginate`] reads. Not
    /// serialized; parallels [`LengthAwarePaginator::page_name`]
    /// (clients receive the cursor values and rebuild URLs their side).
    ///
    /// [`Builder::cursor_paginate`]: crate::eloquent::Builder::cursor_paginate
    /// [`LengthAwarePaginator::page_name`]: crate::pagination::LengthAwarePaginator::page_name
    #[serde(skip)]
    pub cursor_name: Option<String>,
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
            cursor_name: None,
        }
    }

    /// Set the optional base URL for the paginator. Returns `self` for
    /// builder-style chaining.
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Override the query-string parameter name the JSON:API link builder
    /// uses for the `next`/`prev` cursor URLs. Defaults to `"cursor"`.
    /// Returns `self` for builder-style chaining.
    pub fn with_cursor_name(mut self, name: impl Into<String>) -> Self {
        self.cursor_name = Some(name.into());
        self
    }

    /// `true` when the paginator is on the first page (the entry call
    /// had no cursor, so there's nothing behind us).
    /// Equivalent to Laravel's `CursorPaginator::onFirstPage`.
    pub fn on_first_page(&self) -> bool {
        self.prev_cursor.is_none()
    }

    /// `true` when the paginator is on the last page (no further
    /// rows past this one). Equivalent to Laravel's
    /// `CursorPaginator::onLastPage`.
    pub fn on_last_page(&self) -> bool {
        self.next_cursor.is_none()
    }

    /// `true` when there is at least one more page to fetch (forward
    /// or backward). Equivalent to Laravel's
    /// `CursorPaginator::hasMorePages`, except that Laravel's cursor
    /// paginator considers itself "has more" only forward; Suprnova's
    /// cursor paginator is bidirectional and reports either direction
    /// as a "more page" — matching the bidirectional surface levelled
    /// in the 2026-05-29 closure.
    pub fn has_more_pages(&self) -> bool {
        self.next_cursor.is_some() || self.prev_cursor.is_some()
    }

    /// `true` when there are enough rows to span multiple pages.
    /// Equivalent to Laravel's `CursorPaginator::hasPages`:
    /// either we're not on the first page or there are more pages to
    /// fetch.
    pub fn has_pages(&self) -> bool {
        !self.on_first_page() || self.has_more_pages()
    }

    /// `true` when the page slice contains no rows. Equivalent to
    /// Laravel's `AbstractCursorPaginator::isEmpty`.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// `true` when the page slice contains at least one row.
    /// Equivalent to Laravel's `AbstractCursorPaginator::isNotEmpty`.
    pub fn is_not_empty(&self) -> bool {
        !self.data.is_empty()
    }

    /// Number of rows on the current page slice. Equivalent to
    /// Laravel's `AbstractCursorPaginator::count`.
    pub fn count(&self) -> usize {
        self.data.len()
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
    /// be impossible after `Server::from_config`). Cursors must be
    /// signed; never emit an unsigned/forgeable cursor payload.
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
        // fallback. Bound to `CryptPurpose::Cursor` so cursor
        // ciphertext cannot be replayed into any other surface
        // (cookie, 2FA secret, cast, etc.).
        Crypt::encrypt_string(crate::crypto::CryptPurpose::Cursor, &json)
    }

    /// Decode the wire cursor into a typed `sea_orm::Value` plus the
    /// scan direction it was emitted with.
    ///
    /// Cursors must be authenticated — there is no plaintext fallback
    /// even if `Crypt` is not initialized (which would itself be a
    /// boot bug). Any attempt to decode an unsigned base64 payload
    /// errors.
    ///
    /// # Error shape
    ///
    /// Cursors are read directly off the wire (typically the
    /// `?cursor=…` query string), so attacker-controlled garbage,
    /// tampered ciphertext, and bit-flipped base64 are the expected
    /// failure modes — not server bugs. To keep client-triggerable
    /// failures off the 500 telemetry channel:
    ///
    /// - The `Crypt::decrypt_string` step (base64 decode + AEAD tag
    ///   verification) is downgraded to a 400 `bad_request` carrying a
    ///   static "Invalid pagination cursor" message. The original
    ///   cryptographic error is intentionally not surfaced — the
    ///   client gains no signal about why decryption failed, and
    ///   operators chasing real `Crypt` problems still see the post-
    ///   decrypt path's internal errors.
    /// - The post-decrypt steps (JSON parse, variant-tag dispatch,
    ///   direction parse) stay 500: any byte sequence that survives
    ///   AEAD authentication was produced by *us*, so a malformed
    ///   payload past that point is a framework bug worth telemetry.
    ///
    /// The 400 downgrade is gated on `Crypt::is_initialized()`: a
    /// genuine uninitialized-`Crypt` (would itself be a boot bug) still
    /// propagates as 500.
    pub fn decode_value(wire: &str) -> Result<(sea_orm::Value, CursorDirection), FrameworkError> {
        let json =
            Crypt::decrypt_string(crate::crypto::CryptPurpose::Cursor, wire).map_err(|e| {
                if Crypt::is_initialized() {
                    FrameworkError::bad_request("Invalid pagination cursor")
                } else {
                    e
                }
            })?;
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

/// The keyset scan a decoded cursor resolves to, decoupled from any
/// query-construction mechanics. Both `Pagination::cursor` (SeaORM
/// `Select<E>` + typed column ops) and `Builder::cursor_paginate`
/// (the Eloquent builder's JSON `filter_op` over the primary key)
/// consume the same plan, so the bidirectional next/prev semantics
/// live in one place rather than being reimplemented per surface.
pub(crate) struct ScanPlan {
    /// `true` → fetch ASC (first page / forward step); `false` → fetch
    /// DESC (backward step, which the caller reverses back to ASC).
    pub order_asc: bool,
    /// `Some((op, boundary))` keyset filter — `op` is `">"` (forward)
    /// or `"<"` (backward). `None` on the first page.
    pub filter: Option<(&'static str, sea_orm::Value)>,
    /// Direction this scan represents; drives the cursor computation in
    /// [`finalize_page`]. Fully correlated with `order_asc` (kept
    /// separate for readability at the call sites).
    pub scan_direction: CursorDirection,
    /// The caller arrived via a forward (`next`) cursor.
    pub entered_via_next: bool,
    /// The caller arrived via a backward (`prev`) cursor.
    pub entered_via_prev: bool,
}

/// Whether a finalized page has rows before / after it, i.e. whether a
/// `prev_cursor` / `next_cursor` should be emitted.
pub(crate) struct PageFlags {
    pub has_next: bool,
    pub has_prev: bool,
}

/// Resolve a decoded cursor (`None` = first page) into the scan to run.
/// Pure — no query mechanics, no IO.
pub(crate) fn plan_scan(decoded: Option<(sea_orm::Value, CursorDirection)>) -> ScanPlan {
    match decoded {
        None => ScanPlan {
            order_asc: true,
            filter: None,
            scan_direction: CursorDirection::Next,
            entered_via_next: false,
            entered_via_prev: false,
        },
        Some((boundary, CursorDirection::Next)) => ScanPlan {
            order_asc: true,
            filter: Some((">", boundary)),
            scan_direction: CursorDirection::Next,
            entered_via_next: true,
            entered_via_prev: false,
        },
        Some((boundary, CursorDirection::Prev)) => ScanPlan {
            order_asc: false,
            filter: Some(("<", boundary)),
            scan_direction: CursorDirection::Prev,
            entered_via_next: false,
            entered_via_prev: true,
        },
    }
}

/// Trim the overflow probe row and compute the page flags.
///
/// `rows` MUST be ASC-normalized: the caller fetches `per_page + 1`
/// rows and, for a backward (DESC) scan, reverses them back to ASC
/// before calling this. With that contract the overflow row is at the
/// END for a forward scan and at the START for a backward scan, so it
/// is dropped from the correct side. Returns the trimmed page plus
/// whether a next / prev cursor should be emitted.
pub(crate) fn finalize_page<T>(
    mut rows: Vec<T>,
    per_page: u64,
    plan: &ScanPlan,
) -> (Vec<T>, PageFlags) {
    let overflow = rows.len() as u64 > per_page;
    if overflow {
        match plan.scan_direction {
            CursorDirection::Next => rows.truncate(per_page as usize),
            CursorDirection::Prev => {
                let drop = rows.len() - per_page as usize;
                rows.drain(0..drop);
            }
        }
    }
    let has_next = match plan.scan_direction {
        // Forward scan: more rows ahead iff we fetched an overflow row.
        CursorDirection::Next => overflow,
        // Backward scan: we came FROM further forward, so there is
        // always a way forward.
        CursorDirection::Prev => true,
    };
    let has_prev = match plan.scan_direction {
        // Forward scan: rows lie before us iff we stepped here via a
        // cursor at all (first page has nothing before it).
        CursorDirection::Next => plan.entered_via_next || plan.entered_via_prev,
        // Backward scan: more rows behind iff we fetched an overflow row.
        CursorDirection::Prev => overflow,
    };
    (rows, PageFlags { has_next, has_prev })
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

    fn plan_for(dir: Option<CursorDirection>) -> ScanPlan {
        let decoded = dir.map(|d| (sea_orm::Value::BigInt(Some(10)), d));
        plan_scan(decoded)
    }

    #[test]
    fn plan_scan_first_page_is_ascending_unfiltered() {
        let p = plan_for(None);
        assert!(p.order_asc);
        assert!(p.filter.is_none());
        assert_eq!(p.scan_direction, CursorDirection::Next);
        assert!(!p.entered_via_next && !p.entered_via_prev);
    }

    #[test]
    fn plan_scan_next_filters_greater_than_ascending() {
        let p = plan_for(Some(CursorDirection::Next));
        assert!(p.order_asc);
        assert_eq!(p.filter.as_ref().map(|(op, _)| *op), Some(">"));
        assert_eq!(p.scan_direction, CursorDirection::Next);
        assert!(p.entered_via_next);
    }

    #[test]
    fn plan_scan_prev_filters_less_than_descending() {
        let p = plan_for(Some(CursorDirection::Prev));
        assert!(!p.order_asc);
        assert_eq!(p.filter.as_ref().map(|(op, _)| *op), Some("<"));
        assert_eq!(p.scan_direction, CursorDirection::Prev);
        assert!(p.entered_via_prev);
    }

    #[test]
    fn finalize_first_page_full_has_next_no_prev() {
        // First page, fetched per_page+1 → overflow → next, no prev.
        let plan = plan_for(None);
        let (rows, flags) = finalize_page(vec![1, 2, 3, 4], 3, &plan);
        assert_eq!(rows, vec![1, 2, 3], "forward overflow trims from the END");
        assert!(flags.has_next);
        assert!(!flags.has_prev);
    }

    #[test]
    fn finalize_first_page_exact_has_neither() {
        // Exactly per_page rows on the first page → no overflow → no next.
        let plan = plan_for(None);
        let (rows, flags) = finalize_page(vec![1, 2, 3], 3, &plan);
        assert_eq!(rows, vec![1, 2, 3]);
        assert!(!flags.has_next);
        assert!(!flags.has_prev);
    }

    #[test]
    fn finalize_forward_step_full_has_both() {
        // Stepped here via a next cursor, page is full with overflow →
        // both directions available.
        let plan = plan_for(Some(CursorDirection::Next));
        let (rows, flags) = finalize_page(vec![11, 12, 13, 14], 3, &plan);
        assert_eq!(rows, vec![11, 12, 13]);
        assert!(flags.has_next);
        assert!(flags.has_prev, "a forward step always has a way back");
    }

    #[test]
    fn finalize_backward_step_overflow_trims_front() {
        // Backward scan: rows arrive ASC-normalized (caller reversed the
        // DESC fetch); the overflow row sits at the START and is dropped
        // there. A back-scan always has a way forward.
        let plan = plan_for(Some(CursorDirection::Prev));
        let (rows, flags) = finalize_page(vec![0, 1, 2, 3], 3, &plan);
        assert_eq!(
            rows,
            vec![1, 2, 3],
            "backward overflow trims from the START"
        );
        assert!(flags.has_next);
        assert!(
            flags.has_prev,
            "overflow on a back-scan means more lie before"
        );
    }

    #[test]
    fn finalize_backward_step_no_overflow_reaches_start() {
        // Walked back to the first page (no overflow) → no further prev,
        // but still a way forward.
        let plan = plan_for(Some(CursorDirection::Prev));
        let (rows, flags) = finalize_page(vec![1, 2, 3], 3, &plan);
        assert_eq!(rows, vec![1, 2, 3]);
        assert!(flags.has_next);
        assert!(!flags.has_prev);
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
        // Authenticated-decrypt failures land as 400 bad_request so
        // they stay off the 500 telemetry channel.
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let attacker = URL_SAFE_NO_PAD.encode(br#"{"t":"BigInt","v":42,"d":"next"}"#);
        let err = CursorPaginator::<i32>::decode_value(&attacker).unwrap_err();
        assert_eq!(err.status_code(), 400, "got: {err}");
    }

    #[test]
    fn cursor_decode_rejects_garbage() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let err = CursorPaginator::<i32>::decode_value("!!! not base64 !!!").unwrap_err();
        // Non-base64 noise off the wire is also a client error, not a
        // server bug — 400, not 500.
        assert_eq!(err.status_code(), 400, "got: {err}");
    }

    #[test]
    fn cursor_decode_tampered_ciphertext_is_400_not_500() {
        // Flip a bit in a legitimate cursor's ciphertext body so AEAD
        // tag verification fails on decrypt. The attacker-triggerable
        // path must land as 400 (client bad input) rather than 500
        // (server fault).
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let wire = CursorPaginator::<i32>::encode_value(
            &sea_orm::Value::BigInt(Some(7)),
            CursorDirection::Next,
        )
        .unwrap();
        // Mangle the last base64 char so the decoded ciphertext bytes
        // change while still being parseable as base64.
        let last = wire.chars().last().unwrap();
        let swap = if last == 'A' { 'B' } else { 'A' };
        let mut tampered = wire[..wire.len() - 1].to_string();
        tampered.push(swap);
        let err = CursorPaginator::<i32>::decode_value(&tampered).unwrap_err();
        assert_eq!(err.status_code(), 400, "got: {err}");
    }

    #[test]
    fn cursor_decode_post_decrypt_garbage_stays_500() {
        // After AEAD authentication, any payload-shape failure must
        // have been emitted by us — so it stays a 500 the operator
        // sees in telemetry, not a 400 the client sees. We synthesize
        // by encrypting a known-bad JSON payload (e.g. an unknown
        // variant tag): the ciphertext authenticates as ours but the
        // post-decrypt step rejects the body.
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        // Mint under the Cursor purpose so the AEAD step authenticates
        // (the wire is "ours") and the post-decrypt JSON parse is what
        // surfaces — otherwise the AEAD step would fail first and we'd
        // get a 400 from the bad-cursor downgrade instead of the 500
        // we're asserting.
        let wire =
            Crypt::encrypt_string(crate::crypto::CryptPurpose::Cursor, "not valid json").unwrap();
        let err = CursorPaginator::<i32>::decode_value(&wire).unwrap_err();
        assert_eq!(
            err.status_code(),
            500,
            "post-decrypt parse failures stay 500: {err}"
        );
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
        // Mint under Cursor purpose so the AEAD step authenticates and
        // the post-decrypt variant-tag dispatch is what rejects.
        let wire = Crypt::encrypt_string(crate::crypto::CryptPurpose::Cursor, bad).unwrap();
        assert!(CursorPaginator::<i32>::decode_value(&wire).is_err());
    }

    #[test]
    fn value_direction_tampering_rejected() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let bad = r#"{"t":"BigInt","v":1,"d":"sideways"}"#;
        // Same as above: bind under the cursor purpose so the AEAD
        // path passes and the direction-parse step is what rejects.
        let wire = Crypt::encrypt_string(crate::crypto::CryptPurpose::Cursor, bad).unwrap();
        assert!(CursorPaginator::<i32>::decode_value(&wire).is_err());
    }

    fn cursor_with(next: Option<&str>, prev: Option<&str>, data: Vec<i32>) -> CursorPaginator<i32> {
        CursorPaginator::new(
            data,
            10,
            next.map(|s| s.to_string()),
            prev.map(|s| s.to_string()),
        )
    }

    #[test]
    fn predicates_track_cursor_presence() {
        // First page — has next, no prev.
        let p = cursor_with(Some("NEXT"), None, vec![1, 2, 3]);
        assert!(p.on_first_page());
        assert!(!p.on_last_page());
        assert!(p.has_more_pages());
        assert!(p.has_pages());
        // Middle page — both cursors present.
        let p = cursor_with(Some("NEXT"), Some("PREV"), vec![4, 5, 6]);
        assert!(!p.on_first_page());
        assert!(!p.on_last_page());
        assert!(p.has_more_pages());
        assert!(p.has_pages());
        // Last page — prev cursor, no next.
        let p = cursor_with(None, Some("PREV"), vec![7, 8]);
        assert!(!p.on_first_page());
        assert!(p.on_last_page());
        assert!(p.has_more_pages());
        assert!(p.has_pages());
        // Single page — neither cursor present.
        let p = cursor_with(None, None, vec![1, 2]);
        assert!(p.on_first_page());
        assert!(p.on_last_page());
        assert!(!p.has_more_pages());
        assert!(!p.has_pages());
    }

    #[test]
    fn empty_and_count_predicates() {
        let p: CursorPaginator<i32> = cursor_with(None, None, vec![]);
        assert!(p.is_empty());
        assert!(!p.is_not_empty());
        assert_eq!(p.count(), 0);

        let p = cursor_with(Some("N"), None, vec![10, 20, 30]);
        assert!(!p.is_empty());
        assert!(p.is_not_empty());
        assert_eq!(p.count(), 3);
    }
}
