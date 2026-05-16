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

pub mod validators;
use validators::UploadValidator;

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

/// Stream the body of `req` into a `MultipartPayload`, invoking
/// `per_field_validator(name, accumulated)` after each chunk so the
/// caller can short-circuit oversized parts at byte boundaries.
pub async fn parse_multipart_streaming<F>(
    req: crate::http::Request,
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
