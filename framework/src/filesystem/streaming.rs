//! Cross-disk streaming copy.
//!
//! [`copy_between_disks`] streams bytes from one registered storage disk to
//! another via opendal's `Reader` / `Writer` APIs. The body is consumed in
//! 64 KiB chunks (set explicitly via [`reader_with`](opendal::Operator::reader_with)
//! `.chunk(...)` so backends with smaller defaults don't materialise the whole
//! file in memory), making the helper safe for arbitrarily large objects.
//!
//! Because it is built on the `Operator` abstraction, the source and
//! destination disks can be backed by *any* opendal driver pair —
//! filesystem → S3, S3 → Azure Blob, in-memory → GCS, and so on.
//!
//! # Example
//!
//! ```rust,no_run
//! use suprnova::{Storage, filesystem::streaming::copy_between_disks};
//!
//! # async fn doc() -> Result<(), suprnova::FrameworkError> {
//! Storage::register_fs("local", "./storage")?;
//! Storage::register_memory("scratch");
//!
//! let bytes_copied =
//!     copy_between_disks("local", "uploads/big.bin", "scratch", "big.bin").await?;
//! assert!(bytes_copied > 0);
//! # Ok(())
//! # }
//! ```

use super::Storage;
use crate::FrameworkError;
use futures::TryStreamExt;

/// Streaming chunk size for the reader. 64 KiB strikes a balance between
/// syscall / network round-trips and memory pressure for large files.
const STREAM_CHUNK_BYTES: usize = 64 * 1024;

/// Copy `src_path` from disk `src` to `dest_path` on disk `dest`, streaming
/// bytes in 64 KiB chunks via opendal's reader/writer.
///
/// Returns the total number of bytes transferred on success. The destination
/// writer is explicitly `close()`-d so backends that finalise the upload on
/// close (e.g. S3 multipart) actually commit the object.
///
/// # Errors
///
/// - `FrameworkError::Internal` if either disk is not registered, the source
///   object cannot be opened, the destination cannot be opened, a chunk read
///   fails mid-stream, a chunk write fails, or the final close fails. Each
///   boundary uses a distinct message prefix so failures are identifiable in
///   logs.
pub async fn copy_between_disks(
    src: &str,
    src_path: &str,
    dest: &str,
    dest_path: &str,
) -> Result<u64, FrameworkError> {
    let src_op = Storage::disk(src)?;
    let dest_op = Storage::disk(dest)?;

    // `reader_with(..).chunk(N).await` builds a reader that fetches at most N
    // bytes per stream item — this is what makes the "streams in 64 KiB
    // chunks" guarantee real on backends whose default chunk is the whole
    // file (notably the in-memory service used in tests).
    let reader = src_op
        .reader_with(src_path)
        .chunk(STREAM_CHUNK_BYTES)
        .await
        .map_err(|e| FrameworkError::internal(format!("open source: {e}")))?;

    let mut writer = dest_op
        .writer(dest_path)
        .await
        .map_err(|e| FrameworkError::internal(format!("open dest: {e}")))?;

    // Once the writer is open, a mid-stream failure can leave a partial object
    // at `dest_path`. Run the transfer separately so that on ANY error we
    // discard the partial write before propagating — a failed copy must never
    // be observable as a truncated destination object.
    match stream_to_writer(reader, &mut writer).await {
        Ok(total) => Ok(total),
        Err(err) => {
            // `abort` discards staged writes for backends that buffer them
            // (e.g. S3 multipart parts); `delete` removes an already-visible
            // partial file (e.g. the local FS backend). Both are best-effort
            // and only logged — the caller still sees the original error.
            if let Err(abort_err) = writer.abort().await {
                tracing::warn!(
                    disk = dest,
                    path = dest_path,
                    error = %abort_err,
                    "failed to abort writer while cleaning up a failed cross-disk copy"
                );
            }
            if let Err(delete_err) = dest_op.delete(dest_path).await {
                tracing::warn!(
                    disk = dest,
                    path = dest_path,
                    error = %delete_err,
                    "failed to delete partial destination while cleaning up a failed cross-disk copy"
                );
            }
            Err(err)
        }
    }
}

/// Stream the full source object into an already-open destination writer.
///
/// Split out from [`copy_between_disks`] so the caller can clean up a partial
/// destination if any step here fails. Consumes the `reader`; borrows the
/// `writer` so the caller can still `abort()` it on error.
async fn stream_to_writer(
    reader: opendal::Reader,
    writer: &mut opendal::Writer,
) -> Result<u64, FrameworkError> {
    // Full range — copy the entire object. Stream item is `io::Result<Bytes>`.
    let stream = reader
        .into_bytes_stream(..)
        .await
        .map_err(|e| FrameworkError::internal(format!("stream open: {e}")))?;
    let mut stream = std::pin::pin!(stream);

    let mut total: u64 = 0;
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|e| FrameworkError::internal(format!("stream read: {e}")))?
    {
        total += chunk.len() as u64;
        writer
            .write(chunk)
            .await
            .map_err(|e| FrameworkError::internal(format!("write: {e}")))?;
    }

    writer
        .close()
        .await
        .map_err(|e| FrameworkError::internal(format!("close: {e}")))?;

    Ok(total)
}
