//! Streaming multipart upload support.
//!
//! Public API:
//! - `#[derive(MultipartRequest)]` — strongly-typed extractor for handlers
//! - `UploadedFile<V>` — single uploaded file with validator `V`
//! - `parse_multipart_streaming` — low-level helper for advanced parsers
//! - `MultipartRequestHooks` — `authorize` / `after_validation` lifecycle hooks
//!
//! # Streaming model
//!
//! Each multipart part is collected into one of two backings:
//!
//! - **Memory** (`Bytes`) — fast path for small parts. Default cap is
//!   2 MiB, configurable via [`set_global_upload_spill_threshold`].
//! - **Disk** (`tempfile::NamedTempFile`) — spill path for large parts.
//!   Chunks are streamed into a temp file as they arrive from the
//!   transport, so a 200 MiB video upload never resides fully in RAM.
//!
//! [`UploadedFile::store_as`] streams from disk-backed parts directly to
//! the destination storage in 64 KiB chunks via `opendal::Operator::writer`
//! — true streaming, not a final-write of a buffered blob.
//!
//! Body is consumed exactly once per request. The derive macro
//! dispatches by `#[field("name")]` so multiple files + text fields
//! in one handler share the same parse.

use crate::error::FrameworkError;
use bytes::Bytes;
use http_body_util::BodyDataStream;
use multer::Multipart;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::NamedTempFile;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub mod validators;
use validators::UploadValidator;

/// Default per-request multipart body cap when none is configured.
/// 25 MiB matches what most production apps want as their default
/// upper bound — large enough for typical document/image uploads,
/// small enough that an unauthenticated client can't trivially DoS.
pub const DEFAULT_MAX_MULTIPART_BODY_BYTES: usize = 25 * 1024 * 1024;

/// Default in-memory buffer size before a single part spills to a
/// temp file. 2 MiB — small enough that typical avatar/image uploads
/// stay in memory (fast path), large enough that buffer thrashing is
/// rare for legitimate uploads.
pub const DEFAULT_UPLOAD_SPILL_THRESHOLD: usize = 2 * 1024 * 1024;

/// Maximum bytes captured into the sniff buffer for magic-byte content
/// inference. `infer::get` only needs the first ~32 bytes for every
/// format it recognises; 16 KiB is comfortably generous and bounds the
/// buffer size for arbitrarily large parts.
const SNIFF_BYTES: usize = 16 * 1024;

/// Streaming chunk size used when copying a disk-backed part to the
/// destination storage. 64 KiB matches the cross-disk streaming helper
/// in [`crate::filesystem::streaming`] and balances syscall/network
/// round-trips against memory pressure.
const STORE_AS_CHUNK_BYTES: usize = 64 * 1024;

static GLOBAL_MAX_BODY: AtomicUsize = AtomicUsize::new(0);
static GLOBAL_SPILL_THRESHOLD: AtomicUsize = AtomicUsize::new(0);

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

/// Set the process-global spill threshold for multipart parts. Parts
/// whose accumulated bytes exceed this value spill from memory to a
/// `tempfile::NamedTempFile` so the framework never materialises an
/// arbitrarily large body in RAM.
///
/// Setting `0` is special: it means "use [`DEFAULT_UPLOAD_SPILL_THRESHOLD`]".
/// Setting `usize::MAX` effectively disables spilling (every part is
/// buffered fully — only do this if you're certain about your body cap).
///
/// Thread-safe; can be called multiple times. The most recent value
/// wins for any subsequent request.
pub fn set_global_upload_spill_threshold(bytes: usize) {
    GLOBAL_SPILL_THRESHOLD.store(bytes, Ordering::SeqCst);
}

/// Read the currently-configured spill threshold. Returns the default
/// if [`set_global_upload_spill_threshold`] has never been called or
/// was called with `0`.
pub fn global_upload_spill_threshold() -> usize {
    let stored = GLOBAL_SPILL_THRESHOLD.load(Ordering::SeqCst);
    if stored == 0 {
        DEFAULT_UPLOAD_SPILL_THRESHOLD
    } else {
        stored
    }
}

/// Default ceiling on the number of parts accepted from a single
/// multipart request.
///
/// The total-byte cap bounds raw payload bytes, but multipart framing
/// (boundary + per-part headers) is not counted toward it — so without a
/// part ceiling a body composed of many tiny parts can drive unbounded
/// `MultipartPayload` growth while staying within the byte budget. 1000
/// mirrors PHP's `max_input_vars` and sits comfortably above any
/// legitimate form.
pub const DEFAULT_MAX_MULTIPART_PARTS: usize = 1000;

static GLOBAL_MAX_PARTS: AtomicUsize = AtomicUsize::new(0);

/// Count of multipart parts spilled from memory to a temp file since
/// process start (see [`upload_tempfiles_spilled_total`]).
static UPLOAD_TEMPFILES_SPILLED: AtomicUsize = AtomicUsize::new(0);

/// Set the process-global ceiling on the number of parts accepted from a
/// single multipart request. Once a request presents more than this many
/// parts the parser rejects it with HTTP 413 *before reading the
/// offending part*, so allocation stays bounded regardless of per-field
/// configuration.
///
/// Setting `0` is special: it means "use [`DEFAULT_MAX_MULTIPART_PARTS`]".
/// Setting `usize::MAX` disables the ceiling.
///
/// Thread-safe; the most recent value wins for any subsequent request.
pub fn set_global_max_multipart_parts(parts: usize) {
    GLOBAL_MAX_PARTS.store(parts, Ordering::SeqCst);
}

/// Read the currently-configured part ceiling. Returns the default if
/// [`set_global_max_multipart_parts`] has never been called or was called
/// with `0`.
pub fn global_max_multipart_parts() -> usize {
    let stored = GLOBAL_MAX_PARTS.load(Ordering::SeqCst);
    if stored == 0 {
        DEFAULT_MAX_MULTIPART_PARTS
    } else {
        stored
    }
}

/// Number of multipart parts that have spilled from memory to a temp file
/// since process start.
///
/// A monotonically increasing process-global counter, useful as an
/// upload-pressure signal. Oversized *text* parts are rejected at the
/// in-memory spill threshold and never spill, so this counter reflects
/// only file parts that legitimately exceeded the threshold.
pub fn upload_tempfiles_spilled_total() -> usize {
    UPLOAD_TEMPFILES_SPILLED.load(Ordering::SeqCst)
}

/// Limits enforced while streaming a multipart body into a
/// [`MultipartPayload`].
///
/// Construct one and hand it to [`parse_multipart_streaming_with_limits`].
/// The [`parse_multipart_streaming`] convenience wrapper fills it from the
/// process-global accessors; `#[derive(MultipartRequest)]` fills it from
/// the per-struct `#[multipart(...)]` / `#[field(..., max_count = N)]`
/// attributes.
pub struct MultipartLimits<'a> {
    /// Hard ceiling on total accumulated body bytes across all parts.
    /// Exceeding it returns HTTP 413; a declared `Content-Length` above it
    /// is rejected before any body byte is read.
    pub max_body_bytes: usize,
    /// Hard ceiling on the number of parts. The (ceiling + 1)-th part is
    /// rejected with HTTP 413 before it is read, bounding allocation
    /// against many-tiny-parts flooding.
    pub max_parts: usize,
    /// Per-part in-memory byte threshold before a file part spills to a
    /// temp file. A text part that crosses this threshold is rejected
    /// (HTTP 400) rather than spilled.
    pub spill_threshold: usize,
    /// Per-field count ceilings keyed by wire field name. When a field
    /// reaches its ceiling, the next part carrying that name is rejected
    /// with HTTP 422 before it is read — so the (ceiling + 1)-th part
    /// never allocates. Names absent from this list are bounded only by
    /// `max_parts`.
    pub per_field_max_counts: &'a [(&'a str, usize)],
}

/// Underlying storage for an [`UploadedFile`] part.
///
/// Pre-allocated by the multipart parser based on whether the part
/// crossed the spill threshold. End users construct `UploadedFile` via
/// the parser + derive macro — they don't build this enum directly.
#[doc(hidden)]
pub enum UploadedFileBacking {
    /// Small part — buffered entirely in memory.
    Memory(Bytes),
    /// Large part — written to a temp file as the body streamed in.
    /// The temp file is auto-deleted when this enum drops, so partial
    /// uploads abandoned mid-request never accumulate on disk.
    Disk(NamedTempFile),
}

/// A single uploaded file with associated validator `V`.
///
/// Backed either by an in-memory `Bytes` (for parts below the spill
/// threshold) or a `tempfile::NamedTempFile` (for larger parts streamed
/// to disk as they arrived). Use [`UploadedFile::store_as`] to write
/// to a registered storage disk — that path is fully streaming for
/// disk-backed parts and a single-op write for in-memory parts.
///
/// To inspect the raw bytes, call [`UploadedFile::bytes`] (async — the
/// disk-backed path reads asynchronously). For size checks, prefer the
/// synchronous [`UploadedFile::size`] accessor.
pub struct UploadedFile<V: UploadValidator = ()> {
    backing: UploadedFileBacking,
    /// Total size of the part in bytes. Pre-computed during parsing so
    /// callers (and the `after_validation` sync hook) can size-check
    /// without doing async I/O.
    pub size: u64,
    /// File extension inferred from magic bytes captured during parse
    /// (a bounded ≤16 KiB sniff buffer). `None` when the format is
    /// unknown; callers should fall back to `"bin"` — the
    /// [`UploadedFile::extension_from_magic`] helper does exactly that.
    inferred_extension: Option<&'static str>,
    /// Original filename declared by the client, if any. Untrusted input — never use as a filesystem path.
    pub file_name: Option<String>,
    /// `Content-Type` the client declared for this part, if any. Untrusted — verify against `inferred_extension` for security-sensitive paths.
    pub content_type: Option<String>,
    _v: std::marker::PhantomData<V>,
}

impl<V: UploadValidator> UploadedFile<V> {
    /// Internal: construct an `UploadedFile` backed by an in-memory
    /// `Bytes`. Called by the derive macro after the parser handed it a
    /// `MultipartValue::File` for a part that stayed under the spill
    /// threshold.
    #[doc(hidden)]
    pub fn from_memory(
        bytes: Bytes,
        file_name: Option<String>,
        content_type: Option<String>,
        inferred_extension: Option<&'static str>,
    ) -> Self {
        let size = bytes.len() as u64;
        Self {
            backing: UploadedFileBacking::Memory(bytes),
            size,
            inferred_extension,
            file_name,
            content_type,
            _v: std::marker::PhantomData,
        }
    }

    /// Internal: construct an `UploadedFile` backed by a temp file on
    /// disk. Called by the derive macro after the parser handed it a
    /// `MultipartValue::File` for a part that exceeded the spill
    /// threshold.
    #[doc(hidden)]
    pub fn from_disk(
        temp: NamedTempFile,
        size: u64,
        file_name: Option<String>,
        content_type: Option<String>,
        inferred_extension: Option<&'static str>,
    ) -> Self {
        Self {
            backing: UploadedFileBacking::Disk(temp),
            size,
            inferred_extension,
            file_name,
            content_type,
            _v: std::marker::PhantomData,
        }
    }

    /// Read the entire upload into memory.
    ///
    /// For in-memory parts this is a cheap `Bytes::clone()`. For
    /// disk-backed parts this asynchronously reads the temp file — so
    /// it allocates `size` bytes plus reads `size` bytes from disk.
    /// Prefer [`UploadedFile::store_as`] whenever the destination is a
    /// storage disk: that path streams in 64 KiB chunks and never
    /// holds the full upload in RAM.
    ///
    /// # Errors
    ///
    /// Returns [`FrameworkError::Internal`] if the disk-backed read
    /// fails (e.g. the temp file was deleted out from under us, or the
    /// process lost permissions). In-memory reads are infallible.
    pub async fn bytes(&self) -> Result<Bytes, FrameworkError> {
        match &self.backing {
            UploadedFileBacking::Memory(b) => Ok(b.clone()),
            UploadedFileBacking::Disk(temp) => {
                let path = temp.path().to_owned();
                let data = tokio::fs::read(&path).await.map_err(|e| {
                    FrameworkError::internal(format!("read uploaded temp file: {e}"))
                })?;
                Ok(Bytes::from(data))
            }
        }
    }

    /// Stream the upload directly to a storage disk.
    ///
    /// For in-memory parts: a single `Operator::write` call.
    ///
    /// For disk-backed parts: open the temp file with `tokio::fs::File`,
    /// open an `Operator::writer` on the destination, and copy 64 KiB
    /// chunks until EOF. The destination writer is explicitly closed so
    /// backends that finalise on close (S3 multipart, Azure block blob)
    /// commit the object before this method returns.
    ///
    /// # Errors
    ///
    /// Returns [`FrameworkError::Internal`] on any I/O failure — opening
    /// the temp file, reading from it, opening the destination writer,
    /// writing a chunk, or closing the destination writer. Each path
    /// uses a distinct message prefix so failures are identifiable in
    /// structured logs.
    pub async fn store_as(
        &self,
        disk: &opendal::Operator,
        path: &str,
    ) -> Result<(), FrameworkError> {
        match &self.backing {
            UploadedFileBacking::Memory(bytes) => {
                disk.write(path, bytes.clone())
                    .await
                    .map_err(|e| FrameworkError::internal(format!("storage write: {e}")))?;
            }
            UploadedFileBacking::Disk(temp) => {
                let mut reader = tokio::fs::File::open(temp.path()).await.map_err(|e| {
                    FrameworkError::internal(format!("open uploaded temp file: {e}"))
                })?;
                let mut writer = disk
                    .writer(path)
                    .await
                    .map_err(|e| FrameworkError::internal(format!("open storage writer: {e}")))?;
                let mut buf = vec![0u8; STORE_AS_CHUNK_BYTES];
                loop {
                    let n = reader.read(&mut buf).await.map_err(|e| {
                        FrameworkError::internal(format!("read uploaded temp file: {e}"))
                    })?;
                    if n == 0 {
                        break;
                    }
                    writer
                        .write(Bytes::copy_from_slice(&buf[..n]))
                        .await
                        .map_err(|e| FrameworkError::internal(format!("storage write: {e}")))?;
                }
                writer
                    .close()
                    .await
                    .map_err(|e| FrameworkError::internal(format!("storage close: {e}")))?;
            }
        }
        Ok(())
    }

    /// Return the canonical file extension derived from the **content's**
    /// magic bytes captured during parsing (a bounded ≤16 KiB sniff
    /// buffer), NOT from the client-supplied filename. Returns `"bin"`
    /// when the content type cannot be identified (binary blobs,
    /// unrecognised formats).
    ///
    /// Synchronous — the extension is pre-computed during multipart
    /// parsing, so this never re-reads the spilled temp file.
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
    /// file.store_as(&disk, &path).await?;
    /// ```
    pub fn extension_from_magic(&self) -> &'static str {
        self.inferred_extension.unwrap_or("bin")
    }
}

/// Order-preserving list of fields from a multipart body. Duplicate
/// names survive intact (for `photos[]`-style array uploads).
#[derive(Default)]
pub struct MultipartPayload {
    /// Parsed multipart parts in submission order. Duplicate names are preserved for `name[]`-style array uploads.
    pub fields: Vec<(String, MultipartValue)>,
}

/// A parsed multipart field — either a file part (whose backing is
/// either in-memory or a disk-spilled temp file) or a text part.
pub enum MultipartValue {
    /// File part. `backing` decides where the content lives; `size`
    /// is the byte count; `inferred_extension` is the result of
    /// `infer::get` over the bounded sniff buffer captured during parse
    /// — pre-computed so the derive macro can hand it to
    /// [`UploadedFile::from_memory`] / [`UploadedFile::from_disk`]
    /// without re-reading the spilled temp file. `sniff` is the same
    /// bounded ≤16 KiB prefix captured during parse, surfaced here so
    /// `validate_final` callers (the derive macro, primarily) can run
    /// content-aware checks without re-reading the spilled file.
    File {
        /// Where the bytes live — in-memory `Bytes` or a disk-spilled temp file.
        backing: UploadedFileBacking,
        /// Total size of the part in bytes.
        size: u64,
        /// Client-declared filename, if any. Untrusted input — never use as a filesystem path.
        file_name: Option<String>,
        /// Client-declared `Content-Type`, if any. Untrusted — cross-check with `inferred_extension`.
        content_type: Option<String>,
        /// Extension inferred from the magic-byte sniff buffer; `None` when the format is unknown.
        inferred_extension: Option<&'static str>,
        /// Bounded ≤16 KiB prefix captured during parse, for content-aware validators.
        sniff: Vec<u8>,
    },
    /// Text part — a non-file field carrying its UTF-8 value.
    Text(String),
}

/// Internal: the parser's per-part output before classification into
/// `MultipartValue::File` vs `MultipartValue::Text`. Keeps the
/// inner-loop signature small.
struct CollectedPart {
    backing: PartBacking,
    size: u64,
    sniff: Vec<u8>,
    inferred_extension: Option<&'static str>,
}

/// Internal: the byte buffer underlying a `CollectedPart`. Either an
/// in-memory `Vec<u8>` (for small parts) or a `NamedTempFile` (for
/// spilled parts). Converted to [`UploadedFileBacking`] (file) or a
/// `String` (text) at the end of `collect_part`.
enum PartBacking {
    Memory(Vec<u8>),
    Disk(NamedTempFile),
}

/// Stream a single part out of `field`, spilling to a temp file once
/// the accumulated buffer crosses `spill_threshold` bytes.
///
/// Updates `*total_so_far` after each chunk and short-circuits with a
/// 413 if the running total exceeds `body_cap`. Validators see the
/// bounded sniff buffer + current accumulated size and may also
/// short-circuit.
async fn collect_part<F>(
    field: &mut multer::Field<'_>,
    name: &str,
    per_field_validator: &mut F,
    spill_threshold: usize,
    body_cap: usize,
    total_so_far: &mut usize,
    is_text: bool,
) -> Result<CollectedPart, FrameworkError>
where
    F: FnMut(&str, &[u8], u64) -> Result<(), FrameworkError>,
{
    let mut mem: Vec<u8> = Vec::new();
    let mut spill: Option<(NamedTempFile, tokio::fs::File)> = None;
    let mut size: u64 = 0;
    let mut sniff: Vec<u8> = Vec::with_capacity(SNIFF_BYTES.min(spill_threshold + 1));

    while let Some(chunk) = field.chunk().await.map_err(|e| FrameworkError::Domain {
        message: format!("multipart chunk: {e}"),
        status_code: 400,
    })? {
        size = size.saturating_add(chunk.len() as u64);

        // Global body cap. `saturating_add` guards against `usize`
        // wraparound on pathologically large streams.
        *total_so_far = total_so_far.saturating_add(chunk.len());
        if *total_so_far > body_cap {
            return Err(FrameworkError::Domain {
                message: format!("multipart body exceeds {body_cap} bytes (cap)"),
                status_code: 413,
            });
        }

        // Capture sniff bytes up to SNIFF_BYTES. Once the buffer is
        // full, additional chunks contribute nothing to it — bound is
        // hard so a 200 MiB upload's sniff stays at 16 KiB.
        let remaining_sniff = SNIFF_BYTES.saturating_sub(sniff.len());
        if remaining_sniff > 0 {
            let take = remaining_sniff.min(chunk.len());
            sniff.extend_from_slice(&chunk[..take]);
        }

        match &mut spill {
            None => {
                // In-memory fast path. Once `mem` crosses
                // `spill_threshold`, drain into a fresh temp file and
                // switch backing for every subsequent chunk.
                mem.extend_from_slice(&chunk);
                if mem.len() > spill_threshold {
                    // A text part must fit in memory: the spill threshold
                    // is a sizing hint for opaque file payloads, not for
                    // arbitrary form fields. Reject an oversized text part
                    // here, the moment it crosses the threshold, instead
                    // of streaming the rest to a temp file only to reject
                    // it after the part is fully consumed.
                    if is_text {
                        return Err(FrameworkError::Domain {
                            message: format!(
                                "text field '{name}' exceeded the {spill_threshold}-byte in-memory limit; reject as oversized"
                            ),
                            status_code: 400,
                        });
                    }
                    UPLOAD_TEMPFILES_SPILLED.fetch_add(1, Ordering::SeqCst);
                    let temp = NamedTempFile::new().map_err(|e| {
                        FrameworkError::internal(format!("create upload tempfile: {e}"))
                    })?;
                    let mut writer = tokio::fs::File::create(temp.path()).await.map_err(|e| {
                        FrameworkError::internal(format!("open upload tempfile: {e}"))
                    })?;
                    writer.write_all(&mem).await.map_err(|e| {
                        FrameworkError::internal(format!("spill upload tempfile: {e}"))
                    })?;
                    mem.clear();
                    spill = Some((temp, writer));
                }
            }
            Some((_, writer)) => {
                writer
                    .write_all(&chunk)
                    .await
                    .map_err(|e| FrameworkError::internal(format!("write upload tempfile: {e}")))?;
            }
        }

        // Validator callback. Streaming-aware signature: bounded sniff
        // buffer + total accumulated size. Validators that care about
        // content (Image) consult sniff; validators that care about
        // size (MaxSize) consult size. Fires AFTER the body cap so a
        // 413 from the cap takes precedence.
        per_field_validator(name, &sniff, size)?;
    }

    let inferred_extension = if sniff.is_empty() {
        None
    } else {
        infer::get(&sniff).map(|k| k.extension())
    };

    let backing = if let Some((temp, mut writer)) = spill {
        writer
            .flush()
            .await
            .map_err(|e| FrameworkError::internal(format!("flush upload tempfile: {e}")))?;
        // Drop the writer so the OS file handle closes before the
        // consumer (`store_as` / `bytes()`) re-opens the path. Saves an
        // edge case where buffered writes haven't flushed yet on some
        // platforms.
        drop(writer);
        PartBacking::Disk(temp)
    } else {
        PartBacking::Memory(mem)
    };

    Ok(CollectedPart {
        backing,
        size,
        sniff,
        inferred_extension,
    })
}

/// Stream the body of `req` into a `MultipartPayload`, capped at
/// `max_body_bytes` total accumulated bytes across all parts, bounded to
/// `max_parts` parts, spilling file parts above `spill_threshold` to temp
/// files, and enforcing any per-field `max_count` ceilings in
/// `per_field_max_counts` during streaming.
///
/// The `per_field_validator` callback fires after each chunk with
/// `(field_name, sniff_buffer, total_size_so_far)`. Validators may
/// short-circuit oversized or wrong-content fields at the chunk
/// boundary.
///
/// The total-body cap is enforced BEFORE `per_field_validator` runs,
/// so it fires even when no validator has been configured for the
/// field (e.g. `UploadedFile<()>` or plain `Option<String>` fields).
///
/// # Errors
///
/// - 400 if the request is malformed (missing content-type, bad boundary)
/// - 413 if a declared `Content-Length`, the accumulated body size, or the
///   number of parts exceeds the configured ceiling
/// - 422 if a part would push a field past its `per_field_max_counts` ceiling
/// - Whatever `per_field_validator` returns (typically 413 for individual
///   field size caps via `MaxSize<N>`, or 422 for content checks)
/// - 500 for I/O failures spilling to / writing the temp file
pub async fn parse_multipart_streaming_with_limits<F>(
    req: crate::http::Request,
    limits: MultipartLimits<'_>,
    mut per_field_validator: F,
) -> Result<MultipartPayload, FrameworkError>
where
    F: FnMut(&str, &[u8], u64) -> Result<(), FrameworkError>,
{
    let MultipartLimits {
        max_body_bytes,
        max_parts,
        spill_threshold,
        per_field_max_counts,
    } = limits;

    let content_type = req
        .content_type()
        .ok_or_else(|| FrameworkError::Domain {
            message: "missing content-type".into(),
            status_code: 400,
        })?
        .to_string();
    let boundary = multer::parse_boundary(&content_type).map_err(|e| FrameworkError::Domain {
        message: format!("invalid multipart boundary: {e}"),
        status_code: 400,
    })?;

    // Pre-reject an honestly-declared oversized body before reading a
    // single frame, mirroring the generic body path (`body_bytes_with_cap`).
    // A client that lies (small Content-Length, large body) is still caught
    // progressively by the per-chunk byte cap inside `collect_part`.
    if let Some(declared) = req
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok())
        && declared > max_body_bytes as u64
    {
        return Err(FrameworkError::Domain {
            message: format!("multipart body exceeds {max_body_bytes} bytes (cap)"),
            status_code: 413,
        });
    }

    let (_parts, body) = req.into_parts();
    // `BodyStream` would yield `Result<Frame<Bytes>, _>` and `Frame<Bytes>`
    // does not impl `Into<Bytes>` (multer's bound). `BodyDataStream` drops
    // trailer frames and yields `Result<Bytes, hyper::Error>` directly,
    // which is exactly what multer wants.
    //
    // Multipart bodies are never pre-buffered by middleware (CSRF only
    // buffers form-urlencoded). If we somehow see a buffered body here
    // it's a programming error — return a clear 400 rather than
    // silently truncating.
    let incoming = match body {
        crate::http::BodyState::Streaming(inc) => inc,
        crate::http::BodyState::Buffered(_) => {
            return Err(FrameworkError::Domain {
                message: "multipart upload received a pre-buffered body — this is a \
                          framework bug; multipart bodies must arrive as streams"
                    .into(),
                status_code: 400,
            });
        }
        crate::http::BodyState::Consumed => {
            return Err(FrameworkError::Domain {
                message: "multipart upload received a fully consumed body — middleware \
                          drained the body without buffering"
                    .into(),
                status_code: 400,
            });
        }
    };
    let stream = BodyDataStream::new(incoming);
    let mut multipart = Multipart::new(stream, boundary);

    let mut payload = MultipartPayload::default();
    let mut total_bytes: usize = 0;

    // Per-field count ceilings (`max_count`) enforced during streaming.
    // `cap_for` maps a field's wire name to its ceiling; `seen_for` tracks
    // how many parts with that name have been accepted so far. Keying
    // `seen_for` by the `&str` borrowed from `cap_for` (not the per-part
    // `String`) satisfies the borrow checker without cloning names.
    let cap_for: std::collections::HashMap<&str, usize> =
        per_field_max_counts.iter().copied().collect();
    let mut seen_for: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut part_count: usize = 0;

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

        // Bound the total part count before reading the part. The byte cap
        // limits raw payload bytes but not the *number* of parts (multipart
        // framing bytes aren't counted toward it), so without this a body
        // of many tiny parts could grow `payload.fields` unbounded within
        // the byte budget. Reject the (max_parts + 1)-th part before it
        // allocates.
        part_count += 1;
        if part_count > max_parts {
            return Err(FrameworkError::Domain {
                message: format!("multipart request exceeds {max_parts} parts (cap)"),
                status_code: 413,
            });
        }

        // Per-field `max_count`: reject the (ceiling + 1)-th part carrying
        // this name before it is read, so the extra part never allocates.
        if let Some((field_key, &cap)) = cap_for.get_key_value(name.as_str()) {
            let seen = seen_for.entry(*field_key).or_insert(0);
            *seen += 1;
            if *seen > cap {
                return Err(FrameworkError::Domain {
                    message: format!("field '{name}' exceeds max_count {cap}"),
                    status_code: 422,
                });
            }
        }

        let collected = collect_part(
            &mut field,
            &name,
            &mut per_field_validator,
            spill_threshold,
            max_body_bytes,
            &mut total_bytes,
            file_name.is_none(),
        )
        .await?;

        // Classification: presence of `filename=` in Content-Disposition
        // is the canonical marker of a file part. Text parts may carry
        // a `Content-Type`, so we don't use `mime.is_some()` as the
        // discriminator.
        let value = if file_name.is_some() {
            let backing = match collected.backing {
                PartBacking::Memory(v) => UploadedFileBacking::Memory(Bytes::from(v)),
                PartBacking::Disk(t) => UploadedFileBacking::Disk(t),
            };
            MultipartValue::File {
                backing,
                size: collected.size,
                file_name,
                content_type: mime,
                inferred_extension: collected.inferred_extension,
                sniff: collected.sniff,
            }
        } else {
            // Text parts must fit in memory — the spill threshold is a
            // sizing hint for opaque file payloads, not arbitrary form
            // fields. A multi-MiB text field is an attack signal: reject
            // with 400.
            let buf: Vec<u8> = match collected.backing {
                PartBacking::Memory(v) => v,
                PartBacking::Disk(_) => {
                    return Err(FrameworkError::Domain {
                        message: format!(
                            "text field '{name}' exceeded spill threshold ({spill_threshold} bytes); reject as oversized"
                        ),
                        status_code: 400,
                    });
                }
            };
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
/// `per_field_validator(name, sniff, size)` after each chunk so the
/// caller can short-circuit oversized parts at byte boundaries.
///
/// Stream the body of `req` into a [`MultipartPayload`], capped at
/// `max_body_bytes` total bytes and spilling file parts above
/// `spill_threshold` to temp files.
///
/// A convenience over [`parse_multipart_streaming_with_limits`] for callers
/// that only need to pin the byte cap and spill threshold: the part-count
/// ceiling defaults to [`global_max_multipart_parts`] and no per-field
/// `max_count` ceilings are applied. `Content-Length` pre-rejection and the
/// global part-count cap still apply.
pub async fn parse_multipart_streaming_with_cap<F>(
    req: crate::http::Request,
    max_body_bytes: usize,
    spill_threshold: usize,
    per_field_validator: F,
) -> Result<MultipartPayload, FrameworkError>
where
    F: FnMut(&str, &[u8], u64) -> Result<(), FrameworkError>,
{
    parse_multipart_streaming_with_limits(
        req,
        MultipartLimits {
            max_body_bytes,
            max_parts: global_max_multipart_parts(),
            spill_threshold,
            per_field_max_counts: &[],
        },
        per_field_validator,
    )
    .await
}

/// Thin wrapper around [`parse_multipart_streaming_with_limits`] that
/// fills [`MultipartLimits`] from the process-global accessors
/// ([`global_max_multipart_body_bytes`], [`global_max_multipart_parts`],
/// [`global_upload_spill_threshold`]) and applies no per-field count
/// ceilings. Callers that need to pin limits to known values — or enforce
/// per-field `max_count` — should call
/// [`parse_multipart_streaming_with_limits`] directly;
/// `#[derive(MultipartRequest)]` does exactly that.
pub async fn parse_multipart_streaming<F>(
    req: crate::http::Request,
    per_field_validator: F,
) -> Result<MultipartPayload, FrameworkError>
where
    F: FnMut(&str, &[u8], u64) -> Result<(), FrameworkError>,
{
    parse_multipart_streaming_with_limits(
        req,
        MultipartLimits {
            max_body_bytes: global_max_multipart_body_bytes(),
            max_parts: global_max_multipart_parts(),
            spill_threshold: global_upload_spill_threshold(),
            per_field_max_counts: &[],
        },
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
