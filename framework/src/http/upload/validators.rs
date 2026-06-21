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
    /// The set of allowed MIME types.
    fn allowed() -> &'static [&'static str];
}

/// Resolve the effective MIME type for a part against an allowlist.
///
/// The detected type from `infer::get` (magic-byte sniffing of the actual
/// content) is authoritative and is matched against `allowed`. The
/// client-declared `Content-Type` is consulted **only** when `infer`
/// cannot recognise the bytes, and even then markup/script payloads are
/// rejected outright so a text file (SVG, HTML, JS) carrying a spoofed
/// binary header (e.g. `image/png`) can never satisfy an image allowlist.
fn validate_against_allowlist(
    sniff: &[u8],
    content_type: Option<&str>,
    allowed: &[&str],
) -> Result<(), FrameworkError> {
    if let Some(kind) = infer::get(sniff) {
        // Detected via magic bytes — the content itself, not the header.
        if allowed.iter().any(|m| *m == kind.mime_type()) {
            return Ok(());
        }
        return Err(FrameworkError::Domain {
            message: format!("disallowed mime type: {}", kind.mime_type()),
            status_code: 422,
        });
    }

    // `infer` could not recognise the bytes. Text-based payloads (SVG,
    // HTML, XML, scripts) live here, and they are exactly what an attacker
    // would smuggle behind a spoofed binary `Content-Type`. Reject any
    // part whose leading bytes look like markup or a script before
    // considering the (untrusted) client header at all.
    if looks_like_markup_or_script(sniff) {
        return Err(FrameworkError::Domain {
            message: "file content does not match its declared type".into(),
            status_code: 422,
        });
    }

    // Genuinely unidentifiable, non-markup bytes: fall back to the client
    // header as the only remaining signal. A missing header is a reject.
    let declared = content_type
        .map(|ct| ct.split(';').next().unwrap_or(ct).trim())
        .filter(|ct| !ct.is_empty())
        .ok_or_else(|| FrameworkError::Domain {
            message: "could not identify file type".into(),
            status_code: 422,
        })?;

    if !allowed.iter().any(|m| m.eq_ignore_ascii_case(declared)) {
        return Err(FrameworkError::Domain {
            message: format!("disallowed mime type: {declared}"),
            status_code: 422,
        });
    }
    Ok(())
}

/// Heuristic: do the leading bytes look like text markup (SVG/HTML/XML) or
/// a script? Used to reject text payloads that `infer` does not recognise
/// before any client-header fallback. Whitespace and a UTF-8 BOM are
/// skipped so leading indentation does not defeat the check.
fn looks_like_markup_or_script(sniff: &[u8]) -> bool {
    let bytes = sniff.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(sniff);
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let trimmed = &bytes[start..];
    // First non-whitespace byte being `<` covers SVG, HTML, XML (incl.
    // `<?xml`, `<!DOCTYPE`, `<svg`, `<html`, `<script`).
    if trimmed.first() == Some(&b'<') {
        return true;
    }
    // Common script shebang.
    trimmed.starts_with(b"#!")
}

/// Upload validator that rejects parts whose effective MIME type is not in
/// the allowlist `L::allowed()`.
///
/// The check is driven by magic-byte sniffing of the actual content; the
/// client-sent `Content-Type` is only ever a fallback for bytes `infer`
/// cannot recognise, and never lets a markup/script payload through.
#[derive(Default)]
pub struct MimeType<L: MimeAllowlist>(std::marker::PhantomData<L>);

impl<L: MimeAllowlist + 'static> UploadValidator for MimeType<L> {
    fn validate_final(
        &self,
        sniff: &[u8],
        _size: u64,
        ct: Option<&str>,
    ) -> Result<(), FrameworkError> {
        validate_against_allowlist(sniff, ct, L::allowed())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// PNG magic bytes followed by enough of an IHDR header that
    /// `infer::get` recognises the part as `image/png`.
    const PNG_HEADER: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00,
    ];

    #[derive(Default)]
    struct OnlyPng;
    impl MimeAllowlist for OnlyPng {
        fn allowed() -> &'static [&'static str] {
            &["image/png"]
        }
    }

    fn run(sniff: &[u8], ct: Option<&str>) -> Result<(), FrameworkError> {
        MimeType::<OnlyPng>::default().validate_final(sniff, sniff.len() as u64, ct)
    }

    #[test]
    fn genuine_png_passes() {
        assert!(run(PNG_HEADER, Some("image/png")).is_ok());
        // Detection is byte-driven; even a missing/wrong header passes
        // because the magic bytes are authoritative.
        assert!(run(PNG_HEADER, None).is_ok());
        assert!(run(PNG_HEADER, Some("application/octet-stream")).is_ok());
    }

    #[test]
    fn svg_with_spoofed_png_header_is_rejected() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
        let err = run(svg, Some("image/png")).expect_err("spoofed SVG must be rejected");
        assert_eq!(err.status_code(), 422);
    }

    #[test]
    fn html_with_spoofed_png_header_is_rejected() {
        let html = b"<!DOCTYPE html><html><body><script>steal()</script></body></html>";
        assert!(run(html, Some("image/png")).is_err());
        // Leading whitespace must not defeat the markup check.
        let padded = b"   \n\t<html></html>";
        assert!(run(padded, Some("image/png")).is_err());
    }

    #[test]
    fn script_shebang_with_spoofed_header_is_rejected() {
        let script = b"#!/bin/sh\nrm -rf /\n";
        assert!(run(script, Some("image/png")).is_err());
    }

    #[test]
    fn unidentifiable_non_markup_falls_back_to_client_header() {
        // Bytes infer cannot classify and which are not markup: the
        // client header is the only signal left.
        let opaque = &[0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert!(run(opaque, Some("image/png")).is_ok());
        assert!(run(opaque, Some("image/png; charset=binary")).is_ok());
        // Wrong / missing header on unidentifiable bytes is rejected.
        assert!(run(opaque, Some("text/plain")).is_err());
        assert!(run(opaque, None).is_err());
    }
}
