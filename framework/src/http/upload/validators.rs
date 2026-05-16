//! Upload validators. Composable via tuple impls — `(Image, MaxSize<N>)`
//! runs both. Implementations are `Default`-constructed inside the
//! derive macro; unit structs auto-impl `Default`, parameterized
//! built-ins use phantom types so a `Default` ctor is meaningful.
//!
//! # Streaming-aware signature
//!
//! Because parts above the configured spill threshold no longer keep their
//! full contents in memory, validators receive a bounded **sniff buffer**
//! (the first ~16 KiB of the part, sufficient for magic-byte detection)
//! plus the **total accumulated size** in bytes. Validators that care
//! about content (e.g. [`Image`], [`MimeType`]) consult `sniff`; validators
//! that care about size (e.g. [`MaxSize`]) consult `size`.

use crate::FrameworkError;

/// Streaming upload validator.
///
/// # Lifecycle
///
/// For each declared `UploadedFile<V>` field on a `#[derive(MultipartRequest)]`
/// struct, the derive macro constructs a single `V` instance via
/// `Default::default()` at the start of request handling and reuses it
/// across **both** [`validate_chunk`](Self::validate_chunk) and
/// [`validate_final`](Self::validate_final) calls for that field.
///
/// # State
///
/// Both methods take `&self`. If your validator needs to accumulate
/// state across chunks (e.g. a rolling hash, a running CRC), use
/// **interior mutability** (`std::cell::Cell`, `std::sync::Mutex`,
/// `std::sync::atomic::*`). Because the same `&V` is threaded through
/// every chunk and the final check, your interior-mutability state
/// is coherent across the entire upload.
///
/// # Composition
///
/// Tuple impls run validators in declaration order:
/// `(Image, MaxSize<5_242_880>)` runs `Image::validate_chunk` first,
/// then `MaxSize::validate_chunk` (per chunk); `validate_final` runs in
/// the same order. Short-circuits on first `Err`.
pub trait UploadValidator: Send + Sync + Default {
    /// Called after each chunk lands.
    ///
    /// - `sniff` is the first up to 16 KiB of the part (truncated when
    ///   the part is smaller), sufficient for magic-byte detection via
    ///   `infer::get`. The buffer never grows past 16 KiB regardless of
    ///   part size.
    /// - `size` is the running total of bytes received for this part.
    ///
    /// Return `Err` to short-circuit oversized uploads at the chunk
    /// boundary without buffering further. Size-based validators
    /// (`MaxSize<N>`) check `size`; content-based validators don't
    /// usually need to act per-chunk.
    fn validate_chunk(&self, sniff: &[u8], size: u64) -> Result<(), FrameworkError> {
        let _ = (sniff, size);
        Ok(())
    }

    /// Called once when the part is fully received.
    ///
    /// - `sniff` is the bounded 16 KiB prefix captured during parsing
    ///   (same buffer threaded through `validate_chunk`).
    /// - `size` is the final byte count.
    /// - `content_type` is the client-declared `Content-Type` header.
    ///   Untrusted — content sniffers should rely on `sniff`.
    fn validate_final(
        &self,
        sniff: &[u8],
        size: u64,
        content_type: Option<&str>,
    ) -> Result<(), FrameworkError> {
        let _ = (sniff, size, content_type);
        Ok(())
    }
}

/// No-op validator — `UploadedFile<()>` accepts any bytes.
impl UploadValidator for () {}

/// `MaxSize<N>` — short-circuits at byte boundary when accumulated > N.
#[derive(Default)]
pub struct MaxSize<const N: usize>;

impl<const N: usize> UploadValidator for MaxSize<N> {
    fn validate_chunk(&self, _sniff: &[u8], size: u64) -> Result<(), FrameworkError> {
        if size > N as u64 {
            return Err(FrameworkError::Domain {
                message: format!("file exceeds {N} bytes"),
                status_code: 413,
            });
        }
        Ok(())
    }
    fn validate_final(
        &self,
        _sniff: &[u8],
        size: u64,
        _ct: Option<&str>,
    ) -> Result<(), FrameworkError> {
        self.validate_chunk(&[], size)
    }
}

/// `Image` — rejects anything whose magic bytes don't claim image/*.
#[derive(Default)]
pub struct Image;

impl UploadValidator for Image {
    fn validate_final(
        &self,
        sniff: &[u8],
        _size: u64,
        _ct: Option<&str>,
    ) -> Result<(), FrameworkError> {
        // `infer::get` only needs the first ~32 bytes for every format it
        // recognises; the bounded sniff buffer (≤ 16 KiB) is generous.
        let kind = infer::get(sniff).ok_or_else(|| FrameworkError::Domain {
            message: "could not identify file type".into(),
            status_code: 422,
        })?;
        if !kind.mime_type().starts_with("image/") {
            return Err(FrameworkError::Domain {
                message: format!("expected image, got {}", kind.mime_type()),
                status_code: 422,
            });
        }
        Ok(())
    }
}

/// `MimeType<L>` — accepts a fixed list provided by an allowlist type.
pub trait MimeAllowlist: Send + Sync + Default {
    fn allowed() -> &'static [&'static str];
}

#[derive(Default)]
pub struct MimeType<L: MimeAllowlist>(std::marker::PhantomData<L>);

impl<L: MimeAllowlist + 'static> UploadValidator for MimeType<L> {
    fn validate_final(
        &self,
        sniff: &[u8],
        _size: u64,
        _ct: Option<&str>,
    ) -> Result<(), FrameworkError> {
        let kind = infer::get(sniff).ok_or_else(|| FrameworkError::Domain {
            message: "could not identify file type".into(),
            status_code: 422,
        })?;
        if !L::allowed().iter().any(|m| *m == kind.mime_type()) {
            return Err(FrameworkError::Domain {
                message: format!("disallowed mime type: {}", kind.mime_type()),
                status_code: 422,
            });
        }
        Ok(())
    }
}

/// Tuple composition. `Default` for tuples up to 12 is provided by std.
impl<A, B> UploadValidator for (A, B)
where
    A: UploadValidator,
    B: UploadValidator,
{
    fn validate_chunk(&self, sniff: &[u8], size: u64) -> Result<(), FrameworkError> {
        self.0.validate_chunk(sniff, size)?;
        self.1.validate_chunk(sniff, size)
    }
    fn validate_final(
        &self,
        sniff: &[u8],
        size: u64,
        ct: Option<&str>,
    ) -> Result<(), FrameworkError> {
        self.0.validate_final(sniff, size, ct)?;
        self.1.validate_final(sniff, size, ct)
    }
}

impl<A, B, C> UploadValidator for (A, B, C)
where
    A: UploadValidator,
    B: UploadValidator,
    C: UploadValidator,
{
    fn validate_chunk(&self, sniff: &[u8], size: u64) -> Result<(), FrameworkError> {
        self.0.validate_chunk(sniff, size)?;
        self.1.validate_chunk(sniff, size)?;
        self.2.validate_chunk(sniff, size)
    }
    fn validate_final(
        &self,
        sniff: &[u8],
        size: u64,
        ct: Option<&str>,
    ) -> Result<(), FrameworkError> {
        self.0.validate_final(sniff, size, ct)?;
        self.1.validate_final(sniff, size, ct)?;
        self.2.validate_final(sniff, size, ct)
    }
}
