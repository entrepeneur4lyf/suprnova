//! Upload validators. Composable via tuple impls — `(Image, MaxSize<N>)`
//! runs both. Implementations are `Default`-constructed inside the
//! derive macro; unit structs auto-impl `Default`, parameterized
//! built-ins use phantom types so a `Default` ctor is meaningful.

use crate::FrameworkError;

pub trait UploadValidator: Send + Sync + Default {
    /// Called after each chunk is appended to the accumulator.
    /// Return `Err` to short-circuit oversized uploads at the byte
    /// boundary without buffering further.
    fn validate_chunk(&self, accumulated: &[u8]) -> Result<(), FrameworkError> {
        let _ = accumulated;
        Ok(())
    }

    /// Called once when the part is fully received. Use for
    /// magic-byte content sniffing.
    fn validate_final(
        &self,
        full: &[u8],
        content_type: Option<&str>,
    ) -> Result<(), FrameworkError> {
        let _ = (full, content_type);
        Ok(())
    }
}

/// No-op validator — `UploadedFile<()>` accepts any bytes.
impl UploadValidator for () {}

/// `MaxSize<N>` — short-circuits at byte boundary when accumulated > N.
#[derive(Default)]
pub struct MaxSize<const N: usize>;

impl<const N: usize> UploadValidator for MaxSize<N> {
    fn validate_chunk(&self, accumulated: &[u8]) -> Result<(), FrameworkError> {
        if accumulated.len() > N {
            return Err(FrameworkError::Domain {
                message: format!("file exceeds {N} bytes"),
                status_code: 413,
            });
        }
        Ok(())
    }
    fn validate_final(&self, full: &[u8], _ct: Option<&str>) -> Result<(), FrameworkError> {
        self.validate_chunk(full)
    }
}

/// `Image` — rejects anything whose magic bytes don't claim image/*.
#[derive(Default)]
pub struct Image;

impl UploadValidator for Image {
    fn validate_final(&self, full: &[u8], _ct: Option<&str>) -> Result<(), FrameworkError> {
        let kind = infer::get(full).ok_or_else(|| FrameworkError::Domain {
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
    fn validate_final(&self, full: &[u8], _ct: Option<&str>) -> Result<(), FrameworkError> {
        let kind = infer::get(full).ok_or_else(|| FrameworkError::Domain {
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
    fn validate_chunk(&self, accumulated: &[u8]) -> Result<(), FrameworkError> {
        self.0.validate_chunk(accumulated)?;
        self.1.validate_chunk(accumulated)
    }
    fn validate_final(&self, full: &[u8], ct: Option<&str>) -> Result<(), FrameworkError> {
        self.0.validate_final(full, ct)?;
        self.1.validate_final(full, ct)
    }
}

impl<A, B, C> UploadValidator for (A, B, C)
where
    A: UploadValidator,
    B: UploadValidator,
    C: UploadValidator,
{
    fn validate_chunk(&self, accumulated: &[u8]) -> Result<(), FrameworkError> {
        self.0.validate_chunk(accumulated)?;
        self.1.validate_chunk(accumulated)?;
        self.2.validate_chunk(accumulated)
    }
    fn validate_final(&self, full: &[u8], ct: Option<&str>) -> Result<(), FrameworkError> {
        self.0.validate_final(full, ct)?;
        self.1.validate_final(full, ct)?;
        self.2.validate_final(full, ct)
    }
}
