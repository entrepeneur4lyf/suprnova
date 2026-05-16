//! Streaming multipart upload support.
//!
//! Public API:
//! - `#[derive(MultipartRequest)]` — strongly-typed extractor for handlers
//! - `UploadedFile<V>` — single uploaded file with validator `V`
//! - `parse_multipart_streaming` — low-level helper for advanced parsers
//! - `MultipartRequestHooks` — `authorize` / `after_validation` lifecycle hooks
//!
//! Body is consumed exactly once per request. The derive macro
//! dispatches by `#[field("name")]` so multiple files + text fields
//! in one handler share the same parse.

use crate::error::FrameworkError;
use bytes::Bytes;
use http_body_util::BodyDataStream;
use multer::Multipart;
use std::sync::atomic::{AtomicUsize, Ordering};

pub mod validators;
use validators::UploadValidator;

/// Default per-request multipart body cap when none is configured.
/// 25 MiB matches what most production apps want as their default
/// upper bound — large enough for typical document/image uploads,
/// small enough that an unauthenticated client can't trivially DoS.
pub const DEFAULT_MAX_MULTIPART_BODY_BYTES: usize = 25 * 1024 * 1024;

static GLOBAL_MAX_BODY: AtomicUsize = AtomicUsize::new(0);

/// Set the process-global cap on multipart request body size, in bytes.
///
/// Called at boot — typically from `bootstrap.rs` — to override the
/// compile-time [`DEFAULT_MAX_MULTIPART_BODY_BYTES`]. Setting `0` is
/// special: it means "use the default". Setting `usize::MAX` disables
/// the cap entirely.
///
/// Per-struct overrides via `#[multipart(max_body_bytes = N)]` still
/// take precedence.
///
/// Thread-safe; can be called multiple times. The most recent value
/// wins for any subsequent request.
pub fn set_global_max_multipart_body_bytes(bytes: usize) {
    GLOBAL_MAX_BODY.store(bytes, Ordering::SeqCst);
}

/// Read the currently-configured global cap. Returns the default if
/// [`set_global_max_multipart_body_bytes`] has never been called or was
/// called with `0`.
pub fn global_max_multipart_body_bytes() -> usize {
    let stored = GLOBAL_MAX_BODY.load(Ordering::SeqCst);
    if stored == 0 {
        DEFAULT_MAX_MULTIPART_BODY_BYTES
    } else {
        stored
    }
}

/// A single uploaded file with associated validator `V`.
pub struct UploadedFile<V: UploadValidator = ()> {
    pub bytes: Bytes,
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    _v: std::marker::PhantomData<V>,
}

impl<V: UploadValidator> UploadedFile<V> {
    #[doc(hidden)]
    pub fn new(bytes: Bytes, file_name: Option<String>, content_type: Option<String>) -> Self {
        Self {
            bytes,
            file_name,
            content_type,
            _v: std::marker::PhantomData,
        }
    }

    /// Stream the upload directly to a Storage disk.
    pub async fn store_as(
        &self,
        disk: &opendal::Operator,
        path: &str,
    ) -> Result<(), FrameworkError> {
        disk.write(path, self.bytes.clone())
            .await
            .map_err(|e| FrameworkError::internal(format!("storage write: {e}")))?;
        Ok(())
    }

    /// Return the canonical file extension derived from the **content's**
    /// magic bytes via the `infer` crate, NOT from the client-supplied
    /// filename. Returns `"bin"` when the content type cannot be
    /// identified (binary blobs, unrecognised formats).
    ///
    /// # Why this matters
    ///
    /// The client-supplied filename is untrusted. A request like
    /// `avatar=@evil.exe` where the body is real PNG bytes would
    /// otherwise be stored with a `.exe` extension if the storage path
    /// is derived from `file_name`. Use this method whenever the path
    /// you write to disk is content-addressed rather than caller-named.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// let path = format!("avatars/{}.{}", user_id, file.extension_from_magic());
    /// storage.write(&path, file.bytes.clone()).await?;
    /// ```
    pub fn extension_from_magic(&self) -> &'static str {
        infer::get(&self.bytes)
            .map(|kind| kind.extension())
            .unwrap_or("bin")
    }
}

/// Order-preserving list of fields from a multipart body. Duplicate
/// names survive intact (for `photos[]`-style array uploads).
#[derive(Default)]
pub struct MultipartPayload {
    pub fields: Vec<(String, MultipartValue)>,
}

pub enum MultipartValue {
    File {
        bytes: Bytes,
        file_name: Option<String>,
        content_type: Option<String>,
    },
    Text(String),
}

/// Stream the body of `req` into a `MultipartPayload`, capped at
/// `max_body_bytes` total accumulated bytes across all parts. The
/// `per_field_validator` callback fires after each chunk and may
/// short-circuit oversized individual fields independently.
///
/// The total-body cap is enforced BEFORE `per_field_validator` runs,
/// so it fires even when no validator has been configured for the
/// field (e.g. `UploadedFile<()>` or plain `Option<String>` fields).
///
/// # Errors
///
/// - 400 if the request is malformed (missing content-type, bad boundary)
/// - 413 if the total body exceeds `max_body_bytes`
/// - Whatever `per_field_validator` returns (typically 413 for individual
///   field size caps via `MaxSize<N>`, or 422 for content checks)
pub async fn parse_multipart_streaming_with_cap<F>(
    req: crate::http::Request,
    max_body_bytes: usize,
    mut per_field_validator: F,
) -> Result<MultipartPayload, FrameworkError>
where
    F: FnMut(&str, &[u8]) -> Result<(), FrameworkError>,
{
    let content_type = req
        .content_type()
        .ok_or_else(|| FrameworkError::Domain {
            message: "missing content-type".into(),
            status_code: 400,
        })?
        .to_string();
    let boundary =
        multer::parse_boundary(&content_type).map_err(|e| FrameworkError::Domain {
            message: format!("invalid multipart boundary: {e}"),
            status_code: 400,
        })?;

    let (_parts, body) = req.into_parts();
    // `BodyStream` would yield `Result<Frame<Bytes>, _>` and `Frame<Bytes>`
    // does not impl `Into<Bytes>` (multer's bound). `BodyDataStream` drops
    // trailer frames and yields `Result<Bytes, hyper::Error>` directly,
    // which is exactly what multer wants.
    let stream = BodyDataStream::new(body);
    let mut multipart = Multipart::new(stream, boundary);

    let mut payload = MultipartPayload::default();
    let mut total_bytes: usize = 0;

    while let Some(mut field) =
        multipart
            .next_field()
            .await
            .map_err(|e| FrameworkError::Domain {
                message: format!("multipart parse: {e}"),
                status_code: 400,
            })?
    {
        let name = field.name().unwrap_or_default().to_string();
        let file_name = field.file_name().map(|s| s.to_string());
        let mime = field.content_type().map(|m| m.to_string());

        let mut buf = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(|e| FrameworkError::Domain {
            message: format!("multipart chunk: {e}"),
            status_code: 400,
        })? {
            // Global body cap — short-circuits BEFORE the per-field validator
            // runs, so it fires even when no validator has been configured for
            // the field. `saturating_add` guards against `usize` wraparound on
            // pathologically large streams.
            total_bytes = total_bytes.saturating_add(chunk.len());
            if total_bytes > max_body_bytes {
                return Err(FrameworkError::Domain {
                    message: format!(
                        "multipart body exceeds {max_body_bytes} bytes (cap)"
                    ),
                    status_code: 413,
                });
            }

            buf.extend_from_slice(&chunk);
            per_field_validator(&name, &buf)?;
        }

        // Classification: presence of `filename=` in Content-Disposition
        // is the canonical marker of a file part. Text parts may carry
        // a `Content-Type`, so we don't use `mime.is_some()` as the
        // discriminator.
        let value = if file_name.is_some() {
            MultipartValue::File {
                bytes: Bytes::from(buf),
                file_name,
                content_type: mime,
            }
        } else {
            MultipartValue::Text(String::from_utf8(buf).map_err(|_| FrameworkError::Domain {
                message: format!("text field '{name}' is not valid UTF-8"),
                status_code: 400,
            })?)
        };

        payload.fields.push((name, value));
    }

    Ok(payload)
}

/// Stream the body of `req` into a `MultipartPayload`, invoking
/// `per_field_validator(name, accumulated)` after each chunk so the
/// caller can short-circuit oversized parts at byte boundaries.
///
/// Thin wrapper around [`parse_multipart_streaming_with_cap`] using the
/// process-global cap from [`global_max_multipart_body_bytes`]. New
/// callers that want to pin the cap to a known value should prefer
/// [`parse_multipart_streaming_with_cap`] directly; this exists for
/// backwards compatibility with the pre-cap signature.
pub async fn parse_multipart_streaming<F>(
    req: crate::http::Request,
    per_field_validator: F,
) -> Result<MultipartPayload, FrameworkError>
where
    F: FnMut(&str, &[u8]) -> Result<(), FrameworkError>,
{
    parse_multipart_streaming_with_cap(
        req,
        global_max_multipart_body_bytes(),
        per_field_validator,
    )
    .await
}

/// Lifecycle hooks for multipart request structs. Mirrors
/// `FormRequest::authorize` / `FormRequest::after_validation` so users
/// have one mental model.
///
/// `#[derive(MultipartRequest)]` emits an empty `impl MultipartRequestHooks for MyStruct {}`
/// unless the struct carries `#[multipart(custom_hooks)]`. With
/// `custom_hooks`, the user provides the impl themselves.
pub trait MultipartRequestHooks {
    /// Called BEFORE the body is consumed. Return `false` to short-circuit
    /// with `FrameworkError::Unauthorized` (maps to HTTP 403 in this codebase).
    fn authorize(_req: &crate::http::Request) -> bool {
        true
    }

    /// Called AFTER the struct is fully constructed. Return
    /// `Err(ValidationErrors)` to surface cross-field validation
    /// failures as a 422 response.
    fn after_validation(&self) -> Result<(), crate::error::ValidationErrors> {
        Ok(())
    }
}
