//! Laravel-shape convenience methods on top of [`opendal::Operator`].
//!
//! The [`DiskExt`] trait is blanket-implemented on [`opendal::Operator`] and
//! re-exported under [`crate::filesystem::DiskExt`] and at the crate root. It
//! sits on top of the operator surface — every method ultimately calls back
//! through the operator (and therefore through the `PathGuardLayer` for local
//! filesystem disks), so the guard against `..` escape is preserved.
//!
//! # Why a trait, not a wrapper
//!
//! `Storage::disk(name)` returns an [`opendal::Operator`] directly so callers
//! get the full streaming surface (`writer`, `reader`, `presign_read`, `list`,
//! `stat`, etc.) without us proxying each method. An extension trait keeps
//! that ethos: there is no wrapper to peel off, the Laravel-shape methods
//! sit *next to* the existing surface, and any future opendal method becomes
//! available the moment opendal ships it.
//!
//! # Dual API
//!
//! Where Laravel's name and opendal's name diverge, both are available. The
//! Laravel method delegates to the underlying opendal call so behaviour is
//! identical:
//!
//! | Laravel              | opendal       |
//! |----------------------|---------------|
//! | `get(path)`          | `read(path)`  |
//! | `put(path, bytes)`   | `write(path, bytes)` |
//! | `move_to(from, to)`  | `rename(from, to)` |
//! | `make_directory(p)`  | `create_dir(p)` |
//! | `delete_directory(p)`| `remove_all(p)` |
//!
//! No methods *shadow* an existing opendal method — that would silently
//! redirect internal callers (e.g. [`crate::filesystem::streaming`]) to a
//! different implementation. The Laravel names sit alongside.

use crate::FrameworkError;
use chrono::{DateTime, Utc};
use opendal::{EntryMode, Operator};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::future::Future;
use std::time::SystemTime;

/// Algorithms supported by [`DiskExt::checksum`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumAlgorithm {
    /// MD5 — Laravel's historical default. Cryptographically broken; provided
    /// for parity with Laravel's `checksum($path, ['checksum_algo' => 'md5'])`
    /// and for compatibility with object stores that surface MD5 ETags. For
    /// any new integrity check choose [`ChecksumAlgorithm::Sha256`] instead.
    Md5,
    /// SHA-1 — also broken for collision resistance; included for parity with
    /// Git/S3 toolchains that still rely on it. Prefer SHA-256.
    Sha1,
    /// SHA-256 — recommended default for any new integrity check.
    Sha256,
}

/// Laravel-shape convenience methods on top of [`opendal::Operator`].
///
/// Blanket-implemented for [`opendal::Operator`]. Bring it into scope with
/// `use suprnova::DiskExt;` to call any of these methods on a disk returned
/// by `Storage::disk(...)`.
///
/// The methods are *additive*: they sit alongside the operator's native
/// methods (`read`, `write`, `delete`, `copy`, `rename`, `stat`, `list`,
/// `create_dir`, `remove_all`, `exists`, `presign_read`, ...) without
/// shadowing any of them.
pub trait DiskExt {
    /// Negation of [`Operator::exists`].
    ///
    /// Returns `Ok(true)` if the path is *not* present. Surfaces the same
    /// error as `exists` if the backend cannot answer the question (network
    /// failure, permission denied, etc.) — a backend error is not "missing".
    fn missing(&self, path: &str) -> impl Future<Output = Result<bool, FrameworkError>> + Send;

    /// File-only existence check.
    ///
    /// Returns `Ok(true)` only if the path exists *and* the entry is a file
    /// (not a directory). Returns `Ok(false)` for non-existence, directories,
    /// or any other entry mode the backend reports.
    fn file_exists(&self, path: &str) -> impl Future<Output = Result<bool, FrameworkError>> + Send;

    /// Negation of [`DiskExt::file_exists`].
    fn file_missing(&self, path: &str)
    -> impl Future<Output = Result<bool, FrameworkError>> + Send;

    /// Directory-only existence check.
    ///
    /// Returns `Ok(true)` only if the path exists *and* the entry is a
    /// directory. Note that object stores do not have real directories; on
    /// those backends only an explicit `create_dir` (or a marker key like
    /// `prefix/`) reports back as a directory.
    fn directory_exists(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<bool, FrameworkError>> + Send;

    /// Negation of [`DiskExt::directory_exists`].
    fn directory_missing(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<bool, FrameworkError>> + Send;

    /// Laravel alias for [`Operator::read`] returning `Vec<u8>`.
    ///
    /// Identical to calling `read(path).await?.to_vec()`. Provided so the
    /// "I want the file contents" call site reads the same as PHP.
    fn get(&self, path: &str) -> impl Future<Output = Result<Vec<u8>, FrameworkError>> + Send;

    /// Laravel alias for [`Operator::write`]. Accepts any bytes-like input.
    fn put(
        &self,
        path: &str,
        contents: impl Into<bytes::Bytes> + Send,
    ) -> impl Future<Output = Result<(), FrameworkError>> + Send;

    /// Read a file and deserialize it as JSON into the requested type.
    ///
    /// Surfaces serde errors as [`FrameworkError::Internal`] with a message
    /// prefix that names the path, so logs are useful when a parse fails.
    fn json<T>(&self, path: &str) -> impl Future<Output = Result<T, FrameworkError>> + Send
    where
        T: DeserializeOwned;

    /// Serialize `value` as JSON and write it to `path` (overwrites).
    ///
    /// Pretty-prints with `serde_json::to_vec_pretty` so the on-disk file is
    /// reasonable to hand-inspect. Use [`DiskExt::put`] directly if you
    /// already have bytes or want the compact form.
    fn put_json<T>(
        &self,
        path: &str,
        value: &T,
    ) -> impl Future<Output = Result<(), FrameworkError>> + Send
    where
        T: Serialize + Sync;

    /// Prepend `data` to a file, joined by `\n`. Creates the file if missing.
    fn prepend(
        &self,
        path: &str,
        data: &str,
    ) -> impl Future<Output = Result<(), FrameworkError>> + Send;

    /// Prepend `data` to a file, joined by `separator`. Creates the file if
    /// missing.
    fn prepend_with_separator(
        &self,
        path: &str,
        data: &str,
        separator: &str,
    ) -> impl Future<Output = Result<(), FrameworkError>> + Send;

    /// Append `data` to a file, joined by `\n`. Creates the file if missing.
    fn append(
        &self,
        path: &str,
        data: &str,
    ) -> impl Future<Output = Result<(), FrameworkError>> + Send;

    /// Append `data` to a file, joined by `separator`. Creates the file if
    /// missing.
    fn append_with_separator(
        &self,
        path: &str,
        data: &str,
        separator: &str,
    ) -> impl Future<Output = Result<(), FrameworkError>> + Send;

    /// Return the size of the file at `path` in bytes.
    ///
    /// Backed by `stat` and returns `Err` if the path does not exist or the
    /// backend cannot answer.
    fn size(&self, path: &str) -> impl Future<Output = Result<u64, FrameworkError>> + Send;

    /// Return the last-modified time of the file at `path` as a UTC
    /// `DateTime`, or `Ok(None)` if the backend doesn't expose one.
    fn last_modified(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<Option<DateTime<Utc>>, FrameworkError>> + Send;

    /// Return the MIME type of the file at `path`.
    ///
    /// Prefers the backend's declared `content-type` (S3, GCS, Azure all set
    /// it). Falls back to sniffing the first chunk via the `infer` crate
    /// when the backend has none.
    ///
    /// Returns `Ok(None)` only when both the backend and the sniffer come up
    /// empty — i.e. an unknown binary or text format.
    fn mime_type(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<Option<String>, FrameworkError>> + Send;

    /// Compute a hex-encoded checksum of the file at `path` using `algorithm`.
    ///
    /// Streams the file in chunks so large files do not blow up memory.
    fn checksum(
        &self,
        path: &str,
        algorithm: ChecksumAlgorithm,
    ) -> impl Future<Output = Result<String, FrameworkError>> + Send;

    /// List file paths in `directory`. With `recursive = true`, descends into
    /// subdirectories.
    ///
    /// Returns only file entries — directories are filtered out. Use
    /// [`DiskExt::directories`] for the inverse.
    fn files(
        &self,
        directory: &str,
        recursive: bool,
    ) -> impl Future<Output = Result<Vec<String>, FrameworkError>> + Send;

    /// Convenience wrapper for `files(directory, true)`.
    fn all_files(
        &self,
        directory: &str,
    ) -> impl Future<Output = Result<Vec<String>, FrameworkError>> + Send;

    /// List directory paths within `directory`. With `recursive = true`,
    /// descends into subdirectories.
    fn directories(
        &self,
        directory: &str,
        recursive: bool,
    ) -> impl Future<Output = Result<Vec<String>, FrameworkError>> + Send;

    /// Convenience wrapper for `directories(directory, true)`.
    fn all_directories(
        &self,
        directory: &str,
    ) -> impl Future<Output = Result<Vec<String>, FrameworkError>> + Send;

    /// Laravel alias for [`Operator::create_dir`].
    fn make_directory(&self, path: &str)
    -> impl Future<Output = Result<(), FrameworkError>> + Send;

    /// Laravel alias for [`Operator::remove_all`]. Deletes a directory and
    /// every entry under it.
    fn delete_directory(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<(), FrameworkError>> + Send;

    /// Laravel alias for [`Operator::rename`].
    ///
    /// `move` is a reserved word in Rust, so the Laravel-side name uses the
    /// `_to` suffix. The Rust-side name is still `rename` via the underlying
    /// operator.
    fn move_to(
        &self,
        from: &str,
        to: &str,
    ) -> impl Future<Output = Result<(), FrameworkError>> + Send;

    /// Generate a pre-signed URL granting temporary read access to `path`.
    ///
    /// Returns the URL as a `String` for parity with Laravel's `temporaryUrl`.
    /// Backed by [`Operator::presign_read`], so it errors with the same
    /// "operation unsupported" message on backends that do not implement
    /// presigning (the in-memory and local filesystem drivers fall in this
    /// bucket; S3, Azure Blob, and GCS support it).
    fn temporary_url(
        &self,
        path: &str,
        expire: std::time::Duration,
    ) -> impl Future<Output = Result<String, FrameworkError>> + Send;

    /// Generate a pre-signed URL granting temporary write access to `path`,
    /// for direct browser-to-cloud uploads.
    ///
    /// Backed by [`Operator::presign_write`]; errors on backends that do not
    /// implement presigning.
    fn temporary_upload_url(
        &self,
        path: &str,
        expire: std::time::Duration,
    ) -> impl Future<Output = Result<String, FrameworkError>> + Send;
}

impl DiskExt for Operator {
    async fn missing(&self, path: &str) -> Result<bool, FrameworkError> {
        let exists = self
            .exists(path)
            .await
            .map_err(|e| FrameworkError::internal(format!("storage exists({path}): {e}")))?;
        Ok(!exists)
    }

    async fn file_exists(&self, path: &str) -> Result<bool, FrameworkError> {
        match self.stat(path).await {
            Ok(meta) => Ok(meta.mode() == EntryMode::FILE),
            Err(e) if e.kind() == opendal::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(FrameworkError::internal(format!(
                "storage stat({path}): {e}"
            ))),
        }
    }

    async fn file_missing(&self, path: &str) -> Result<bool, FrameworkError> {
        Ok(!self.file_exists(path).await?)
    }

    async fn directory_exists(&self, path: &str) -> Result<bool, FrameworkError> {
        match self.stat(path).await {
            Ok(meta) => Ok(meta.mode() == EntryMode::DIR),
            Err(e) if e.kind() == opendal::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(FrameworkError::internal(format!(
                "storage stat({path}): {e}"
            ))),
        }
    }

    async fn directory_missing(&self, path: &str) -> Result<bool, FrameworkError> {
        Ok(!self.directory_exists(path).await?)
    }

    async fn get(&self, path: &str) -> Result<Vec<u8>, FrameworkError> {
        let buf = self
            .read(path)
            .await
            .map_err(|e| FrameworkError::internal(format!("storage read({path}): {e}")))?;
        Ok(buf.to_vec())
    }

    async fn put(
        &self,
        path: &str,
        contents: impl Into<bytes::Bytes> + Send,
    ) -> Result<(), FrameworkError> {
        self.write(path, contents.into())
            .await
            .map_err(|e| FrameworkError::internal(format!("storage write({path}): {e}")))?;
        Ok(())
    }

    async fn json<T>(&self, path: &str) -> Result<T, FrameworkError>
    where
        T: DeserializeOwned,
    {
        let bytes = self.get(path).await?;
        serde_json::from_slice::<T>(&bytes).map_err(|e| {
            FrameworkError::internal(format!("storage json({path}): parse error: {e}"))
        })
    }

    async fn put_json<T>(&self, path: &str, value: &T) -> Result<(), FrameworkError>
    where
        T: Serialize + Sync,
    {
        let bytes = serde_json::to_vec_pretty(value).map_err(|e| {
            FrameworkError::internal(format!("storage json({path}): encode error: {e}"))
        })?;
        self.put(path, bytes).await
    }

    async fn prepend(&self, path: &str, data: &str) -> Result<(), FrameworkError> {
        self.prepend_with_separator(path, data, "\n").await
    }

    async fn prepend_with_separator(
        &self,
        path: &str,
        data: &str,
        separator: &str,
    ) -> Result<(), FrameworkError> {
        if self.file_exists(path).await? {
            let existing = self.get(path).await?;
            let mut out = Vec::with_capacity(data.len() + separator.len() + existing.len());
            out.extend_from_slice(data.as_bytes());
            out.extend_from_slice(separator.as_bytes());
            out.extend_from_slice(&existing);
            self.put(path, out).await
        } else {
            self.put(path, data.as_bytes().to_vec()).await
        }
    }

    async fn append(&self, path: &str, data: &str) -> Result<(), FrameworkError> {
        self.append_with_separator(path, data, "\n").await
    }

    async fn append_with_separator(
        &self,
        path: &str,
        data: &str,
        separator: &str,
    ) -> Result<(), FrameworkError> {
        if self.file_exists(path).await? {
            let existing = self.get(path).await?;
            let mut out = Vec::with_capacity(existing.len() + separator.len() + data.len());
            out.extend_from_slice(&existing);
            out.extend_from_slice(separator.as_bytes());
            out.extend_from_slice(data.as_bytes());
            self.put(path, out).await
        } else {
            self.put(path, data.as_bytes().to_vec()).await
        }
    }

    async fn size(&self, path: &str) -> Result<u64, FrameworkError> {
        let meta = self
            .stat(path)
            .await
            .map_err(|e| FrameworkError::internal(format!("storage stat({path}): {e}")))?;
        Ok(meta.content_length())
    }

    async fn last_modified(&self, path: &str) -> Result<Option<DateTime<Utc>>, FrameworkError> {
        let meta = self
            .stat(path)
            .await
            .map_err(|e| FrameworkError::internal(format!("storage stat({path}): {e}")))?;
        Ok(meta.last_modified().map(|ts| {
            let system: SystemTime = ts.into();
            DateTime::<Utc>::from(system)
        }))
    }

    async fn mime_type(&self, path: &str) -> Result<Option<String>, FrameworkError> {
        // 1) Ask the backend. S3/Azure/GCS pass Content-Type through.
        let meta = self
            .stat(path)
            .await
            .map_err(|e| FrameworkError::internal(format!("storage stat({path}): {e}")))?;
        if let Some(declared) = meta.content_type() {
            return Ok(Some(declared.to_string()));
        }
        // 2) Sniff the first chunk via `infer`. 16 KiB is enough for every
        //    signature `infer` ships with and keeps the read cheap. Cap the
        //    range at the actual file size so the read doesn't error when the
        //    file is smaller than the sniff window.
        const SNIFF_BYTES: u64 = 16 * 1024;
        let want = SNIFF_BYTES.min(meta.content_length());
        if want == 0 {
            return Ok(None);
        }
        let reader = self
            .reader_with(path)
            .chunk(want as usize)
            .await
            .map_err(|e| FrameworkError::internal(format!("storage reader({path}): {e}")))?;
        let buf = reader
            .read(0..want)
            .await
            .map_err(|e| FrameworkError::internal(format!("storage sniff({path}): {e}")))?;
        Ok(infer::get(&buf.to_vec()).map(|k| k.mime_type().to_string()))
    }

    async fn checksum(
        &self,
        path: &str,
        algorithm: ChecksumAlgorithm,
    ) -> Result<String, FrameworkError> {
        use digest::Digest;

        const CHUNK: usize = 64 * 1024;
        let reader = self
            .reader_with(path)
            .chunk(CHUNK)
            .await
            .map_err(|e| FrameworkError::internal(format!("storage reader({path}): {e}")))?;
        let mut stream = reader
            .into_bytes_stream(..)
            .await
            .map_err(|e| FrameworkError::internal(format!("storage stream({path}): {e}")))?;

        match algorithm {
            ChecksumAlgorithm::Md5 => {
                let mut hasher = md5::Md5::new();
                use futures::StreamExt;
                while let Some(chunk) = stream.next().await {
                    let bytes = chunk.map_err(|e| {
                        FrameworkError::internal(format!("storage checksum read({path}): {e}"))
                    })?;
                    hasher.update(&bytes);
                }
                Ok(hex::encode(hasher.finalize()))
            }
            ChecksumAlgorithm::Sha1 => {
                let mut hasher = sha1::Sha1::new();
                use futures::StreamExt;
                while let Some(chunk) = stream.next().await {
                    let bytes = chunk.map_err(|e| {
                        FrameworkError::internal(format!("storage checksum read({path}): {e}"))
                    })?;
                    hasher.update(&bytes);
                }
                Ok(hex::encode(hasher.finalize()))
            }
            ChecksumAlgorithm::Sha256 => {
                let mut hasher = sha2::Sha256::new();
                use futures::StreamExt;
                while let Some(chunk) = stream.next().await {
                    let bytes = chunk.map_err(|e| {
                        FrameworkError::internal(format!("storage checksum read({path}): {e}"))
                    })?;
                    hasher.update(&bytes);
                }
                Ok(hex::encode(hasher.finalize()))
            }
        }
    }

    async fn files(&self, directory: &str, recursive: bool) -> Result<Vec<String>, FrameworkError> {
        list_entries(self, directory, recursive, EntryMode::FILE).await
    }

    async fn all_files(&self, directory: &str) -> Result<Vec<String>, FrameworkError> {
        self.files(directory, true).await
    }

    async fn directories(
        &self,
        directory: &str,
        recursive: bool,
    ) -> Result<Vec<String>, FrameworkError> {
        list_entries(self, directory, recursive, EntryMode::DIR).await
    }

    async fn all_directories(&self, directory: &str) -> Result<Vec<String>, FrameworkError> {
        self.directories(directory, true).await
    }

    async fn make_directory(&self, path: &str) -> Result<(), FrameworkError> {
        self.create_dir(path)
            .await
            .map_err(|e| FrameworkError::internal(format!("storage create_dir({path}): {e}")))
    }

    async fn delete_directory(&self, path: &str) -> Result<(), FrameworkError> {
        self.delete_with(path)
            .recursive(true)
            .await
            .map_err(|e| FrameworkError::internal(format!("storage delete_recursive({path}): {e}")))
    }

    async fn move_to(&self, from: &str, to: &str) -> Result<(), FrameworkError> {
        // Try the backend's native rename first — it's atomic on filesystems
        // and a single API call on S3 / Azure when supported. Some backends
        // (notably opendal's in-memory `services::Memory`) do not implement
        // rename or copy, so we fall back to read + write + delete so
        // `move_to` works regardless of driver.
        match self.rename(from, to).await {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == opendal::ErrorKind::Unsupported => {
                // fall through to copy fallback
            }
            Err(e) => {
                return Err(FrameworkError::internal(format!(
                    "storage rename({from} → {to}): {e}"
                )));
            }
        }
        match self.copy(from, to).await {
            Ok(()) => self
                .delete(from)
                .await
                .map_err(|e| FrameworkError::internal(format!("storage delete({from}): {e}"))),
            Err(e) if e.kind() == opendal::ErrorKind::Unsupported => {
                // Last-resort fallback: read the source, write to dest,
                // delete the source. Used by the in-memory backend where
                // neither rename nor copy are implemented.
                let bytes = self.get(from).await?;
                self.put(to, bytes).await?;
                self.delete(from)
                    .await
                    .map_err(|e| FrameworkError::internal(format!("storage delete({from}): {e}")))
            }
            Err(e) => Err(FrameworkError::internal(format!(
                "storage copy({from} → {to}): {e}"
            ))),
        }
    }

    async fn temporary_url(
        &self,
        path: &str,
        expire: std::time::Duration,
    ) -> Result<String, FrameworkError> {
        let presigned = self
            .presign_read(path, expire)
            .await
            .map_err(|e| FrameworkError::internal(format!("storage presign_read({path}): {e}")))?;
        Ok(presigned.uri().to_string())
    }

    async fn temporary_upload_url(
        &self,
        path: &str,
        expire: std::time::Duration,
    ) -> Result<String, FrameworkError> {
        let presigned = self
            .presign_write(path, expire)
            .await
            .map_err(|e| FrameworkError::internal(format!("storage presign_write({path}): {e}")))?;
        Ok(presigned.uri().to_string())
    }
}

/// Shared implementation behind [`DiskExt::files`] / [`DiskExt::directories`].
///
/// Calls `list_with(...).recursive(...)`; the mode of each entry is read
/// from its embedded metadata. The supplied `directory` is normalized to
/// either an empty string (the root) or a trailing-slash form (`prefix/`)
/// so object stores accept it.
async fn list_entries(
    op: &Operator,
    directory: &str,
    recursive: bool,
    want_mode: EntryMode,
) -> Result<Vec<String>, FrameworkError> {
    let prefix = normalise_directory(directory);
    let entries = op
        .list_with(&prefix)
        .recursive(recursive)
        .await
        .map_err(|e| FrameworkError::internal(format!("storage list({prefix}): {e}")))?;

    // opendal includes the directory entry itself in the list (e.g. listing
    // "foo/" yields "foo/" as one of the results). Skip self-entries so the
    // caller only sees children — matches Laravel's `files()`/`directories()`.
    let prefix_for_compare = prefix.clone();
    let mut paths: Vec<String> = entries
        .into_iter()
        .filter(|e| e.path() != prefix_for_compare)
        .filter(|e| e.metadata().mode() == want_mode)
        .map(|e| {
            // opendal returns directory entries with a trailing slash
            // (`"docs/sub/"`); Laravel returns them without (`"docs/sub"`).
            // Strip the slash on dir entries so PHP-side comparisons keep
            // working when ported. File paths never carry a trailing slash
            // so this is safe to apply unconditionally for `DIR` mode.
            let path = e.path();
            if want_mode == EntryMode::DIR {
                path.trim_end_matches('/').to_string()
            } else {
                path.to_string()
            }
        })
        .collect();
    // Laravel's `files()` calls `sortByPath()` before returning, so callers
    // can rely on deterministic ordering even when the backend doesn't sort.
    paths.sort();
    Ok(paths)
}

fn normalise_directory(directory: &str) -> String {
    if directory.is_empty() || directory == "/" {
        return String::new();
    }
    let trimmed = directory.trim_start_matches('/');
    if trimmed.ends_with('/') {
        trimmed.to_string()
    } else {
        format!("{trimmed}/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::Storage;

    #[tokio::test]
    async fn missing_and_exists_are_inverse() {
        let _guard = Storage::fake();
        Storage::register_memory("inverse");
        let disk = Storage::disk("inverse").unwrap();
        assert!(disk.missing("x.txt").await.unwrap());
        disk.put("x.txt", b"hi".to_vec()).await.unwrap();
        assert!(!disk.missing("x.txt").await.unwrap());
    }

    #[tokio::test]
    async fn file_and_directory_existence_distinguish_mode() {
        let _guard = Storage::fake();
        Storage::register_memory("modes");
        let disk = Storage::disk("modes").unwrap();
        disk.put("a/b.txt", b"x".to_vec()).await.unwrap();

        assert!(disk.file_exists("a/b.txt").await.unwrap());
        assert!(!disk.directory_exists("a/b.txt").await.unwrap());

        assert!(disk.file_missing("nope.txt").await.unwrap());
        assert!(disk.directory_missing("nope.txt").await.unwrap());

        disk.make_directory("dirA/").await.unwrap();
        assert!(disk.directory_exists("dirA/").await.unwrap());
        assert!(!disk.file_exists("dirA/").await.unwrap());
    }

    #[tokio::test]
    async fn get_and_put_are_read_write_aliases() {
        let _guard = Storage::fake();
        Storage::register_memory("getput");
        let disk = Storage::disk("getput").unwrap();
        disk.put("alpha.txt", b"alpha".to_vec()).await.unwrap();
        let bytes = disk.get("alpha.txt").await.unwrap();
        assert_eq!(bytes, b"alpha");
    }

    #[tokio::test]
    async fn json_round_trip() {
        let _guard = Storage::fake();
        Storage::register_memory("json");
        let disk = Storage::disk("json").unwrap();

        let value = serde_json::json!({"name": "Suprnova", "version": 1});
        disk.put_json("config.json", &value).await.unwrap();

        let decoded: serde_json::Value = disk.json("config.json").await.unwrap();
        assert_eq!(decoded, value);
    }

    #[tokio::test]
    async fn json_decode_failure_surfaces_path() {
        let _guard = Storage::fake();
        Storage::register_memory("jsonfail");
        let disk = Storage::disk("jsonfail").unwrap();
        disk.put("broken.json", b"not json".to_vec()).await.unwrap();

        let err = disk
            .json::<serde_json::Value>("broken.json")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("broken.json"),
            "json error must name the path, got: {err}"
        );
    }

    #[tokio::test]
    async fn prepend_creates_file_when_missing() {
        let _guard = Storage::fake();
        Storage::register_memory("prepnew");
        let disk = Storage::disk("prepnew").unwrap();
        disk.prepend("log.txt", "first line").await.unwrap();
        assert_eq!(&disk.get("log.txt").await.unwrap(), b"first line");
    }

    #[tokio::test]
    async fn prepend_joins_with_separator() {
        let _guard = Storage::fake();
        Storage::register_memory("prepjoin");
        let disk = Storage::disk("prepjoin").unwrap();
        disk.put("log.txt", b"old".to_vec()).await.unwrap();
        disk.prepend("log.txt", "new").await.unwrap();
        assert_eq!(&disk.get("log.txt").await.unwrap(), b"new\nold");
    }

    #[tokio::test]
    async fn append_creates_file_when_missing() {
        let _guard = Storage::fake();
        Storage::register_memory("appnew");
        let disk = Storage::disk("appnew").unwrap();
        disk.append("log.txt", "first line").await.unwrap();
        assert_eq!(&disk.get("log.txt").await.unwrap(), b"first line");
    }

    #[tokio::test]
    async fn append_joins_with_separator() {
        let _guard = Storage::fake();
        Storage::register_memory("appjoin");
        let disk = Storage::disk("appjoin").unwrap();
        disk.put("log.txt", b"line-a".to_vec()).await.unwrap();
        disk.append("log.txt", "line-b").await.unwrap();
        assert_eq!(&disk.get("log.txt").await.unwrap(), b"line-a\nline-b");
    }

    #[tokio::test]
    async fn append_with_separator_honours_caller_choice() {
        let _guard = Storage::fake();
        Storage::register_memory("appsep");
        let disk = Storage::disk("appsep").unwrap();
        disk.put("csv.txt", b"a,b".to_vec()).await.unwrap();
        disk.append_with_separator("csv.txt", "c", ",")
            .await
            .unwrap();
        assert_eq!(&disk.get("csv.txt").await.unwrap(), b"a,b,c");
    }

    #[tokio::test]
    async fn size_and_last_modified_inspect_metadata() {
        let _guard = Storage::fake();
        Storage::register_memory("meta");
        let disk = Storage::disk("meta").unwrap();
        disk.put("big.bin", vec![0u8; 1234]).await.unwrap();
        assert_eq!(disk.size("big.bin").await.unwrap(), 1234);
        // The in-memory backend always reports last_modified; just assert the
        // call succeeded.
        disk.last_modified("big.bin").await.unwrap();
    }

    #[tokio::test]
    async fn mime_type_sniffs_when_backend_silent() {
        let _guard = Storage::fake();
        Storage::register_memory("mime");
        let disk = Storage::disk("mime").unwrap();
        // PNG signature.
        let png_header: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let mut payload = png_header.to_vec();
        payload.extend_from_slice(&[0u8; 64]);
        disk.put("logo.png", payload).await.unwrap();

        let mime = disk.mime_type("logo.png").await.unwrap();
        assert_eq!(mime.as_deref(), Some("image/png"));
    }

    #[tokio::test]
    async fn mime_type_returns_none_for_unknown_blob() {
        let _guard = Storage::fake();
        Storage::register_memory("mimeunk");
        let disk = Storage::disk("mimeunk").unwrap();
        disk.put("random.bin", vec![0xAAu8; 64]).await.unwrap();
        assert!(disk.mime_type("random.bin").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn checksum_matches_known_values() {
        let _guard = Storage::fake();
        Storage::register_memory("cksum");
        let disk = Storage::disk("cksum").unwrap();
        disk.put("greet.txt", b"hello".to_vec()).await.unwrap();

        // RFC test vectors.
        let md5 = disk
            .checksum("greet.txt", ChecksumAlgorithm::Md5)
            .await
            .unwrap();
        assert_eq!(md5, "5d41402abc4b2a76b9719d911017c592");

        let sha1 = disk
            .checksum("greet.txt", ChecksumAlgorithm::Sha1)
            .await
            .unwrap();
        assert_eq!(sha1, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");

        let sha256 = disk
            .checksum("greet.txt", ChecksumAlgorithm::Sha256)
            .await
            .unwrap();
        assert_eq!(
            sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[tokio::test]
    async fn files_excludes_directories_and_sorts() {
        let _guard = Storage::fake();
        Storage::register_memory("files");
        let disk = Storage::disk("files").unwrap();
        disk.put("docs/readme.md", b"r".to_vec()).await.unwrap();
        disk.put("docs/intro.md", b"i".to_vec()).await.unwrap();
        disk.make_directory("docs/sub/").await.unwrap();

        let files = disk.files("docs", false).await.unwrap();
        assert_eq!(files, vec!["docs/intro.md", "docs/readme.md"]);
    }

    #[tokio::test]
    async fn all_files_descends_recursively() {
        let _guard = Storage::fake();
        Storage::register_memory("allfiles");
        let disk = Storage::disk("allfiles").unwrap();
        disk.put("a/x.txt", b"x".to_vec()).await.unwrap();
        disk.put("a/b/y.txt", b"y".to_vec()).await.unwrap();
        disk.put("a/b/c/z.txt", b"z".to_vec()).await.unwrap();

        let files = disk.all_files("a").await.unwrap();
        assert_eq!(files, vec!["a/b/c/z.txt", "a/b/y.txt", "a/x.txt"]);
    }

    #[tokio::test]
    async fn directories_excludes_files() {
        let _guard = Storage::fake();
        Storage::register_memory("dirs");
        let disk = Storage::disk("dirs").unwrap();
        disk.put("root/file.txt", b"f".to_vec()).await.unwrap();
        disk.make_directory("root/sub-a/").await.unwrap();
        disk.make_directory("root/sub-b/").await.unwrap();

        let dirs = disk.directories("root", false).await.unwrap();
        assert_eq!(dirs, vec!["root/sub-a", "root/sub-b"]);
    }

    #[tokio::test]
    async fn move_to_renames_the_file() {
        let _guard = Storage::fake();
        Storage::register_memory("rename");
        let disk = Storage::disk("rename").unwrap();
        disk.put("here.txt", b"x".to_vec()).await.unwrap();
        disk.move_to("here.txt", "there.txt").await.unwrap();
        assert!(disk.missing("here.txt").await.unwrap());
        assert_eq!(&disk.get("there.txt").await.unwrap(), b"x");
    }

    #[tokio::test]
    async fn temporary_url_errors_on_backends_without_presign() {
        let _guard = Storage::fake();
        Storage::register_memory("noPresign");
        let disk = Storage::disk("noPresign").unwrap();
        disk.put("x.txt", b"x".to_vec()).await.unwrap();
        let err = disk
            .temporary_url("x.txt", std::time::Duration::from_secs(60))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("presign_read"),
            "error should name the operation, got: {err}"
        );
    }

    #[tokio::test]
    async fn temporary_upload_url_errors_on_backends_without_presign() {
        let _guard = Storage::fake();
        Storage::register_memory("noPresignWrite");
        let disk = Storage::disk("noPresignWrite").unwrap();
        let err = disk
            .temporary_upload_url("x.txt", std::time::Duration::from_secs(60))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("presign_write"),
            "error should name the operation, got: {err}"
        );
    }

    #[tokio::test]
    async fn make_and_delete_directory_round_trip() {
        let _guard = Storage::fake();
        Storage::register_memory("mkdir");
        let disk = Storage::disk("mkdir").unwrap();
        disk.make_directory("trash/").await.unwrap();
        disk.put("trash/a.txt", b"a".to_vec()).await.unwrap();
        disk.put("trash/b.txt", b"b".to_vec()).await.unwrap();
        disk.delete_directory("trash/").await.unwrap();
        assert!(disk.file_missing("trash/a.txt").await.unwrap());
        assert!(disk.file_missing("trash/b.txt").await.unwrap());
    }
}
