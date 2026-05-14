# Phase 4: Filesystem + File Uploads + Validation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Three subsystems shipped together because controllers touch all three: (1) `Storage::disk("name")` facade backed by `opendal` with FS / S3 / Azure / GCS / memory drivers all first-class; (2) streaming `UploadedFile<NAME, V, S>` extractor that validates and stores incoming files without materializing them in RAM; (3) Validation parity — rule objects, after-hooks, first-class error bags.

**Architecture:** `framework/src/filesystem/` wraps `opendal::Operator` behind a `Storage::disk(name)` registry. Disks are registered at boot (typically in `bootstrap.rs`); the facade returns the registered `Operator` directly so consumers get opendal's full streaming API (`writer`, `reader`, `presign_*`, `stat`, `list`). Upload handling lives in `framework/src/http/upload.rs`; multipart parsing is streamed via the existing `hyper::body::Incoming` path, with the new `UploadedFile<NAME, V, S>` extractor enforcing size limits at byte boundaries during parse and validators inspecting the first N bytes (magic numbers) before commit. Validation upgrades extend the existing `FormRequest` trait — adding `Rule` objects (composable beyond `validator` derive), `after_validation` hook for cross-field checks, and a typed `ErrorBag` keyed by scope.

**Tech Stack:** `opendal` 0.50 (with `services-fs`, `services-s3`, `services-azblob`, `services-gcs`, `services-memory`), `multer` 3.x (streaming multipart parser built on hyper bodies), `infer` 0.16 (magic-number content detection), `image` 0.25 (resize/strip-EXIF on tokio blocking pool).

---

## File Structure

**New files:**
- `framework/src/filesystem/mod.rs` — `Storage` facade + `register_disk` / `disk(name)`
- `framework/src/filesystem/registry.rs` — process-global disk registry
- `framework/src/filesystem/config.rs` — `DiskConfig` enum + env-from helpers
- `framework/src/filesystem/streaming.rs` — `copy_between_disks(src, dest)` cross-disk streaming
- `framework/src/filesystem/testing.rs` — `Storage::fake()`, in-memory disk for tests
- `framework/src/http/upload.rs` — `UploadedFile<const NAME: &str, V, S>`, multipart streaming
- `framework/src/http/upload/validators.rs` — `Image`, `MaxSize`, `MimeType`, `Extension` validators
- `framework/src/validation/rule.rs` — `Rule` trait + built-in `Required`, `Email`, `Min`, `Max`, `Unique`
- `framework/src/validation/error_bag.rs` — typed `ErrorBag` (default + named scopes)
- `framework/src/validation/after_hook.rs` — `after_validation` extension on FormRequest
- `framework/tests/filesystem.rs` — fs / memory / cross-disk copy tests
- `framework/tests/uploads.rs` — multipart parse, MaxSize early-reject, Image magic check
- `framework/tests/validation_rules.rs` — Rule objects, after-hook, error bag scoping
- `app/src/controllers/avatar_upload.rs` — dogfood `/users/avatar`

**Modified files:**
- `framework/Cargo.toml` — add `opendal`, `multer`, `infer`, `image`
- `framework/src/lib.rs` — declare + re-export
- `framework/src/http/form_request.rs` — wire `after_validation` + `ErrorBag` integration
- `framework/src/error.rs` — `ValidationErrors` gets named-scope support
- `app/src/bootstrap.rs` — register `local`, `s3`, `public` disks

---

## Task 1: Add deps

**Files:** `framework/Cargo.toml`

- [ ] **Step 1: Add deps**

```toml
# framework/Cargo.toml — [dependencies]
opendal = { version = "0.50", default-features = false, features = ["services-fs", "services-s3", "services-azblob", "services-gcs", "services-memory"] }
multer = "3"
infer = "0.16"
image = { version = "0.25", default-features = false, features = ["jpeg", "png", "webp", "gif"] }
```

- [ ] **Step 2: Verify build**

```bash
cargo check --workspace
```

- [ ] **Step 3: Commit**

```bash
git add framework/Cargo.toml Cargo.lock
git commit -m "feat(deps): add opendal, multer, infer, image for Phase 4"
```

---

## Task 2: Storage::disk facade + registry

**Files:** `framework/src/filesystem/registry.rs`, `framework/src/filesystem/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/filesystem.rs
use suprnova::Storage;

#[tokio::test]
async fn memory_disk_round_trip() {
    Storage::register_memory("test");
    let disk = Storage::disk("test").unwrap();
    disk.write("hello.txt", "hello world").await.unwrap();
    let bytes = disk.read("hello.txt").await.unwrap();
    assert_eq!(&bytes.to_vec(), b"hello world");
}

#[tokio::test]
async fn unknown_disk_returns_error() {
    let result = Storage::disk("does-not-exist");
    assert!(result.is_err());
}

#[tokio::test]
async fn fs_disk_writes_to_temp_dir() {
    let tmp = tempfile::tempdir().unwrap();
    Storage::register_fs("tmp", tmp.path()).unwrap();
    let disk = Storage::disk("tmp").unwrap();
    disk.write("nested/file.bin", &b"binary"[..]).await.unwrap();
    let on_disk = tmp.path().join("nested/file.bin");
    assert!(on_disk.exists());
    assert_eq!(std::fs::read(&on_disk).unwrap(), b"binary");
}
```

- [ ] **Step 2: Add `tempfile` to dev-deps**

```toml
# framework/Cargo.toml — [dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Implement**

```rust
// framework/src/filesystem/registry.rs
use crate::FrameworkError;
use opendal::Operator;
use std::collections::HashMap;
use std::sync::RwLock;

static REGISTRY: RwLock<Option<HashMap<String, Operator>>> = RwLock::new(None);

pub(crate) fn register(name: impl Into<String>, op: Operator) {
    let mut g = REGISTRY.write().unwrap();
    let map = g.get_or_insert_with(HashMap::new);
    map.insert(name.into(), op);
}

pub(crate) fn get(name: &str) -> Result<Operator, FrameworkError> {
    let g = REGISTRY.read().unwrap();
    g.as_ref()
        .and_then(|m| m.get(name).cloned())
        .ok_or_else(|| FrameworkError::internal(format!("storage disk '{}' not registered", name)))
}

#[cfg(any(test, feature = "testing"))]
pub(crate) fn reset() {
    *REGISTRY.write().unwrap() = None;
}
```

```rust
// framework/src/filesystem/mod.rs
//! Storage facade backed by `opendal`. Every backend opendal supports
//! is available; we provide convenience registrars for the common
//! cases (FS, S3, Azure Blob, GCS, memory).
//!
//! ```ignore
//! // bootstrap.rs
//! Storage::register_s3("uploads", S3Config { bucket: "myapp-uploads", ... });
//! Storage::register_fs("public", "./public");
//!
//! // controller
//! let disk = Storage::disk("uploads")?;
//! disk.write("avatars/1.jpg", &bytes).await?;
//! let url = disk.presign_read("avatars/1.jpg", Duration::from_secs(300)).await?;
//! ```

mod registry;
pub mod streaming;
pub mod testing;

use crate::FrameworkError;
use opendal::{services, Operator};
use std::path::Path;

pub struct Storage;

#[derive(Debug, Clone)]
pub struct S3Config {
    pub bucket: String,
    pub region: Option<String>,
    pub endpoint: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub root: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AzBlobConfig {
    pub container: String,
    pub account_name: String,
    pub account_key: String,
    pub root: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GcsConfig {
    pub bucket: String,
    pub credential: Option<String>,
    pub root: Option<String>,
}

impl Storage {
    /// Retrieve a registered disk as an opendal `Operator`. The full
    /// opendal surface (write, read, writer, reader, presign_*, list,
    /// stat, etc.) is available.
    pub fn disk(name: &str) -> Result<Operator, FrameworkError> {
        registry::get(name)
    }

    pub fn register_fs(name: impl Into<String>, root: impl AsRef<Path>) -> Result<(), FrameworkError> {
        let mut builder = services::Fs::default();
        builder.root(&root.as_ref().to_string_lossy());
        let op = Operator::new(builder)
            .map_err(|e| FrameworkError::internal(format!("opendal fs init: {}", e)))?
            .finish();
        registry::register(name, op);
        Ok(())
    }

    pub fn register_memory(name: impl Into<String>) {
        let op = Operator::new(services::Memory::default())
            .expect("memory service init")
            .finish();
        registry::register(name, op);
    }

    pub fn register_s3(name: impl Into<String>, config: S3Config) -> Result<(), FrameworkError> {
        let mut builder = services::S3::default();
        builder.bucket(&config.bucket);
        if let Some(r) = &config.region {
            builder.region(r);
        }
        if let Some(ep) = &config.endpoint {
            builder.endpoint(ep);
        }
        if let Some(k) = &config.access_key_id {
            builder.access_key_id(k);
        }
        if let Some(s) = &config.secret_access_key {
            builder.secret_access_key(s);
        }
        if let Some(r) = &config.root {
            builder.root(r);
        }
        let op = Operator::new(builder)
            .map_err(|e| FrameworkError::internal(format!("opendal s3 init: {}", e)))?
            .finish();
        registry::register(name, op);
        Ok(())
    }

    pub fn register_azblob(name: impl Into<String>, config: AzBlobConfig) -> Result<(), FrameworkError> {
        let mut builder = services::Azblob::default();
        builder.container(&config.container);
        builder.account_name(&config.account_name);
        builder.account_key(&config.account_key);
        if let Some(r) = &config.root {
            builder.root(r);
        }
        let op = Operator::new(builder)
            .map_err(|e| FrameworkError::internal(format!("opendal azblob init: {}", e)))?
            .finish();
        registry::register(name, op);
        Ok(())
    }

    pub fn register_gcs(name: impl Into<String>, config: GcsConfig) -> Result<(), FrameworkError> {
        let mut builder = services::Gcs::default();
        builder.bucket(&config.bucket);
        if let Some(c) = &config.credential {
            builder.credential(c);
        }
        if let Some(r) = &config.root {
            builder.root(r);
        }
        let op = Operator::new(builder)
            .map_err(|e| FrameworkError::internal(format!("opendal gcs init: {}", e)))?
            .finish();
        registry::register(name, op);
        Ok(())
    }

    /// Test helper — wipes all registered disks and installs a fresh
    /// memory-backed one named `"default"`. Returns a guard that
    /// resets the registry on drop.
    #[cfg(any(test, feature = "testing"))]
    pub fn fake() -> testing::StorageFakeGuard {
        testing::install_fake()
    }
}
```

```rust
// framework/src/filesystem/testing.rs
#[cfg(any(test, feature = "testing"))]
pub struct StorageFakeGuard;

#[cfg(any(test, feature = "testing"))]
impl Drop for StorageFakeGuard {
    fn drop(&mut self) {
        super::registry::reset();
    }
}

#[cfg(any(test, feature = "testing"))]
pub(crate) fn install_fake() -> StorageFakeGuard {
    super::registry::reset();
    super::Storage::register_memory("default");
    StorageFakeGuard
}
```

```rust
// framework/src/lib.rs
pub mod filesystem;
pub use filesystem::{Storage, S3Config, AzBlobConfig, GcsConfig};
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova --test filesystem
```

Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add framework/src/filesystem framework/src/lib.rs framework/tests/filesystem.rs framework/Cargo.toml
git commit -m "feat(filesystem): Storage facade with opendal-backed fs/memory/s3/azblob/gcs disks"
```

---

## Task 3: Cross-disk streaming copy

**Files:** `framework/src/filesystem/streaming.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/filesystem.rs — append
use suprnova::filesystem::streaming::copy_between_disks;

#[tokio::test]
async fn streaming_copy_moves_bytes_between_disks() {
    Storage::register_memory("src");
    Storage::register_memory("dest");
    let src = Storage::disk("src").unwrap();
    src.write("biggie.bin", vec![0u8; 1_000_000]).await.unwrap();

    copy_between_disks("src", "biggie.bin", "dest", "moved.bin").await.unwrap();

    let dest = Storage::disk("dest").unwrap();
    let bytes = dest.read("moved.bin").await.unwrap();
    assert_eq!(bytes.len(), 1_000_000);
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/filesystem/streaming.rs
use super::Storage;
use crate::FrameworkError;
use futures::TryStreamExt;

const CHUNK_SIZE: usize = 64 * 1024;

/// Copy `src_path` from disk `src` to `dest_path` on disk `dest`,
/// streaming via 64KB chunks — never materialising the whole file
/// in memory. Works across any opendal-supported backend pairing
/// (FS → S3, S3 → Azure, memory → FS, etc.).
pub async fn copy_between_disks(
    src: &str,
    src_path: &str,
    dest: &str,
    dest_path: &str,
) -> Result<u64, FrameworkError> {
    let src_op = Storage::disk(src)?;
    let dest_op = Storage::disk(dest)?;

    let mut reader = src_op
        .reader(src_path)
        .await
        .map_err(|e| FrameworkError::internal(format!("open source: {}", e)))?;
    let mut writer = dest_op
        .writer(dest_path)
        .await
        .map_err(|e| FrameworkError::internal(format!("open dest: {}", e)))?;

    let mut total: u64 = 0;
    let stream = reader.into_bytes_stream(..).await
        .map_err(|e| FrameworkError::internal(format!("stream open: {}", e)))?;
    let mut stream = std::pin::pin!(stream);
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|e| FrameworkError::internal(format!("stream read: {}", e)))?
    {
        total += chunk.len() as u64;
        writer
            .write(chunk)
            .await
            .map_err(|e| FrameworkError::internal(format!("write: {}", e)))?;
    }
    writer
        .close()
        .await
        .map_err(|e| FrameworkError::internal(format!("close: {}", e)))?;
    Ok(total)
}
```

> **API verification:** Confirm exact opendal 0.50 `Reader::into_bytes_stream` and `Writer::write/close` signatures via `cargo doc -p opendal --open --no-deps`. Adjust chunk handling if the stream type differs.

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test filesystem streaming
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/filesystem/streaming.rs
git commit -m "feat(filesystem): copy_between_disks streams 64KB chunks across opendal disks"
```

---

## Task 4: Multipart parser foundation

**Files:** `framework/src/http/upload.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/uploads.rs
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use suprnova::http::upload::parse_multipart;
use suprnova::Request;

async fn spawn() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let io = TokioIo::new(stream);
            let svc = service_fn(|hyper_req: hyper::Request<hyper::body::Incoming>| async move {
                let req = Request::new(hyper_req);
                let parts = parse_multipart(req).await;
                let body = match parts {
                    Ok(fields) => {
                        let names: Vec<String> = fields.into_iter().map(|f| f.field_name).collect();
                        format!("fields:{}", names.join(","))
                    }
                    Err(e) => format!("err:{}", e),
                };
                Ok::<_, Infallible>(
                    hyper::Response::builder()
                        .body(http_body_util::Full::new(Bytes::from(body)))
                        .unwrap(),
                )
            });
            let _ = http1::Builder::new().serve_connection(io, svc).await;
        }
    });
    addr
}

#[tokio::test]
async fn parse_multipart_extracts_named_fields() {
    let addr = spawn().await;
    let body = "--boundary\r\nContent-Disposition: form-data; name=\"avatar\"; filename=\"a.jpg\"\r\nContent-Type: image/jpeg\r\n\r\nbinary-payload\r\n--boundary--\r\n";
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake::<_, http_body_util::Full<Bytes>>(io)
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let req = hyper::Request::builder()
        .method("POST")
        .uri("/upload")
        .header("content-type", "multipart/form-data; boundary=boundary")
        .header("content-length", body.len())
        .body(http_body_util::Full::new(Bytes::from(body)))
        .unwrap();
    let resp = sender.send_request(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(std::str::from_utf8(&bytes).unwrap(), "fields:avatar");
}
```

- [ ] **Step 2: Implement multipart wrapper**

```rust
// framework/src/http/upload.rs
//! Streaming multipart parser.
//!
//! Built on `multer`, which streams over an arbitrary
//! `Stream<Item = Result<Bytes, _>>`. We adapt `hyper::body::Incoming`
//! to that shape via `http_body_util::BodyStream`.

use crate::FrameworkError;
use bytes::Bytes;
use http_body_util::BodyStream;
use multer::Multipart;

pub struct MultipartField {
    pub field_name: String,
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    pub bytes: Bytes,
}

pub async fn parse_multipart(req: crate::http::Request) -> Result<Vec<MultipartField>, FrameworkError> {
    let content_type = req
        .header("content-type")
        .ok_or_else(|| FrameworkError::internal("missing content-type"))?;
    let boundary = multer::parse_boundary(&content_type)
        .map_err(|e| FrameworkError::internal(format!("multipart boundary: {}", e)))?;

    let body = req.into_body();
    let stream = BodyStream::new(body);

    let mut multipart = Multipart::new(stream, boundary);
    let mut fields = Vec::new();

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| FrameworkError::internal(format!("multipart parse: {}", e)))?
    {
        let field_name = field.name().unwrap_or_default().to_string();
        let file_name = field.file_name().map(|s| s.to_string());
        let content_type = field.content_type().map(|m| m.to_string());

        let mut buf = Vec::new();
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| FrameworkError::internal(format!("multipart chunk: {}", e)))?
        {
            buf.extend_from_slice(&chunk);
        }
        fields.push(MultipartField {
            field_name,
            file_name,
            content_type,
            bytes: Bytes::from(buf),
        });
    }
    Ok(fields)
}
```

> **Implementation note:** `Request::into_body()` and `Request::header()` exist per `framework/src/http/request.rs`; verify their exact signatures before invoking.

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test uploads
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/http/upload.rs framework/tests/uploads.rs
git commit -m "feat(http): parse_multipart streams multipart bodies via multer"
```

---

## Task 5: UploadedFile<NAME, V, S> extractor

**Files:** `framework/src/http/upload.rs`, `framework/src/http/upload/validators.rs`

- [ ] **Step 1: Write failing test (validator + extractor)**

```rust
// framework/tests/uploads.rs — append
use suprnova::http::upload::{UploadedFile, validators::{MaxSize, Image}};

#[tokio::test]
async fn upload_extractor_rejects_oversize_at_byte_boundary() {
    // 6MB body with a 5MB limit should reject mid-stream.
    let big_body = vec![0u8; 6 * 1024 * 1024];
    // ... (build multipart body with the big_body as the avatar field)
    // Run through UploadedFile<"avatar", Image, MaxSize::<5_242_880>>::extract
    // and expect FrameworkError matching too-large.
}

#[tokio::test]
async fn upload_extractor_rejects_non_image_via_magic_bytes() {
    // 12-byte PDF magic header in the body when validator is Image.
    let pdf_body = b"%PDF-1.4 lorem ipsum".to_vec();
    // ... extract and expect rejection because magic bytes say PDF, not image.
}
```

- [ ] **Step 2: Implement validators**

```rust
// framework/src/http/upload/validators.rs
use crate::FrameworkError;

pub trait UploadValidator: Send + Sync {
    fn validate_chunk(&self, accumulated: &[u8]) -> Result<(), FrameworkError>;
    fn validate_final(&self, full: &[u8], content_type: Option<&str>) -> Result<(), FrameworkError>;
}

pub struct MaxSize<const N: usize>;

impl<const N: usize> UploadValidator for MaxSize<N> {
    fn validate_chunk(&self, accumulated: &[u8]) -> Result<(), FrameworkError> {
        if accumulated.len() > N {
            return Err(FrameworkError::Domain {
                message: format!("file exceeds {} bytes", N),
                status_code: 413,
            });
        }
        Ok(())
    }
    fn validate_final(&self, full: &[u8], _ct: Option<&str>) -> Result<(), FrameworkError> {
        self.validate_chunk(full)
    }
}

pub struct Image;

impl UploadValidator for Image {
    fn validate_chunk(&self, _accumulated: &[u8]) -> Result<(), FrameworkError> {
        Ok(())
    }
    fn validate_final(&self, full: &[u8], _ct: Option<&str>) -> Result<(), FrameworkError> {
        let kind = infer::get(full)
            .ok_or_else(|| FrameworkError::Domain {
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

pub struct MimeType(pub &'static [&'static str]);

impl UploadValidator for MimeType {
    fn validate_chunk(&self, _: &[u8]) -> Result<(), FrameworkError> {
        Ok(())
    }
    fn validate_final(&self, full: &[u8], _ct: Option<&str>) -> Result<(), FrameworkError> {
        let kind = infer::get(full)
            .ok_or_else(|| FrameworkError::Domain {
                message: "could not identify file type".into(),
                status_code: 422,
            })?;
        if !self.0.iter().any(|m| *m == kind.mime_type()) {
            return Err(FrameworkError::Domain {
                message: format!("disallowed mime type: {}", kind.mime_type()),
                status_code: 422,
            });
        }
        Ok(())
    }
}
```

- [ ] **Step 3: Implement extractor**

```rust
// framework/src/http/upload.rs — append
use crate::http::FromRequest;
use validators::UploadValidator;

pub mod validators;

/// Extractor for a single uploaded file. `NAME` is the form field
/// name (`"avatar"`, `"resume"`, etc.). `V` is a validator that
/// inspects the file bytes. The extractor enforces the validator
/// during parse — `validate_chunk` runs every 64KB to short-circuit
/// oversize uploads; `validate_final` runs once when the part is
/// complete to inspect magic bytes.
pub struct UploadedFile<const NAME: &'static str, V: UploadValidator + Default = ()> {
    pub bytes: Bytes,
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    _v: std::marker::PhantomData<V>,
}

impl UploadValidator for () {
    fn validate_chunk(&self, _: &[u8]) -> Result<(), FrameworkError> { Ok(()) }
    fn validate_final(&self, _: &[u8], _: Option<&str>) -> Result<(), FrameworkError> { Ok(()) }
}

impl Default for () {} // already exists

#[async_trait::async_trait]
impl<const NAME: &'static str, V: UploadValidator + Default + 'static>
    FromRequest for UploadedFile<NAME, V>
{
    async fn from_request(req: &mut crate::http::Request) -> Result<Self, FrameworkError> {
        // Take the body once; this consumes it. (Multiple
        // UploadedFile extractors on the same request need to share
        // a parsed map — see follow-up below.)
        let fields = parse_multipart_streaming::<V>(req, NAME).await?;
        Ok(Self {
            bytes: fields.bytes,
            file_name: fields.file_name,
            content_type: fields.content_type,
            _v: std::marker::PhantomData,
        })
    }
}

async fn parse_multipart_streaming<V: UploadValidator + Default>(
    req: &mut crate::http::Request,
    target_field: &str,
) -> Result<MultipartField, FrameworkError> {
    let validator = V::default();
    let content_type = req
        .header("content-type")
        .ok_or_else(|| FrameworkError::internal("missing content-type"))?;
    let boundary = multer::parse_boundary(&content_type)
        .map_err(|e| FrameworkError::internal(format!("multipart boundary: {}", e)))?;

    let body = req.take_body()?;
    let stream = BodyStream::new(body);
    let mut multipart = Multipart::new(stream, boundary);

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| FrameworkError::internal(format!("multipart parse: {}", e)))?
    {
        let name = field.name().unwrap_or_default().to_string();
        if name != target_field {
            // Drain unrelated fields without buffering.
            while field.chunk().await.ok().flatten().is_some() {}
            continue;
        }
        let file_name = field.file_name().map(|s| s.to_string());
        let mime = field.content_type().map(|m| m.to_string());

        let mut buf = Vec::new();
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| FrameworkError::internal(format!("multipart chunk: {}", e)))?
        {
            buf.extend_from_slice(&chunk);
            validator.validate_chunk(&buf)?; // short-circuit on oversize
        }
        validator.validate_final(&buf, mime.as_deref())?;

        return Ok(MultipartField {
            field_name: name,
            file_name,
            content_type: mime,
            bytes: Bytes::from(buf),
        });
    }

    Err(FrameworkError::param(target_field))
}
```

> **`Request::take_body`:** If this method doesn't exist on `Request`, add it. The current `into_body` likely consumes `self`; an extractor needs `&mut Request` so the body must be `Option<Body>` internally. Adjust accordingly; this is a real shape problem to solve, not gloss over.

- [ ] **Step 4: Add `store_as` convenience for direct-to-disk write**

```rust
// framework/src/http/upload.rs — impl<const NAME, V> UploadedFile<NAME, V>
impl<const NAME: &'static str, V: UploadValidator + Default> UploadedFile<NAME, V> {
    /// Stream the upload directly to a Storage disk.
    pub async fn store_as(
        &self,
        disk: &opendal::Operator,
        path: &str,
    ) -> Result<(), FrameworkError> {
        disk.write(path, self.bytes.clone())
            .await
            .map_err(|e| FrameworkError::internal(format!("storage write: {}", e)))
    }
}
```

- [ ] **Step 5: Re-export validators**

```rust
// framework/src/lib.rs
pub use http::upload::{UploadedFile, validators::{Image, MaxSize, MimeType}};
```

- [ ] **Step 6: Run — expect pass**

```bash
cargo test -p suprnova --test uploads
```

- [ ] **Step 7: Commit**

```bash
git add framework/src/http/upload.rs framework/src/http/upload/validators.rs framework/src/lib.rs framework/tests/uploads.rs
git commit -m "feat(http): UploadedFile extractor with streaming validation + store_as"
```

---

## Task 6: Rule objects (Required, Email, Min, Max, Unique)

**Files:** `framework/src/validation/rule.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/validation_rules.rs
use suprnova::validation::{Rule, rules::{Required, Email, Min, Max}};

#[test]
fn required_passes_on_present() {
    let r = Required;
    assert!(r.passes("not empty").is_ok());
    assert!(r.passes("").is_err());
}

#[test]
fn email_validates_shape() {
    let r = Email;
    assert!(r.passes("user@example.com").is_ok());
    assert!(r.passes("not-an-email").is_err());
}

#[test]
fn min_max_check_length() {
    let r = Min(8);
    assert!(r.passes("longenough").is_ok());
    assert!(r.passes("short").is_err());

    let r = Max(5);
    assert!(r.passes("hi").is_ok());
    assert!(r.passes("toolong").is_err());
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/validation/rule.rs
//! Rule objects — composable string validators.
//!
//! ```ignore
//! use suprnova::validation::rules::{Required, Email, Min};
//!
//! Required.passes(&form.email)?;
//! Email.passes(&form.email)?;
//! Min(8).passes(&form.password)?;
//! ```

pub trait Rule {
    fn passes(&self, value: &str) -> Result<(), String>;
}

pub mod rules {
    use super::Rule;

    pub struct Required;
    impl Rule for Required {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.trim().is_empty() {
                Err("required".into())
            } else {
                Ok(())
            }
        }
    }

    pub struct Email;
    impl Rule for Email {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.contains('@') && value.contains('.') && value.len() > 5 {
                Ok(())
            } else {
                Err("must be a valid email".into())
            }
        }
    }

    pub struct Min(pub usize);
    impl Rule for Min {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.chars().count() >= self.0 {
                Ok(())
            } else {
                Err(format!("must be at least {} characters", self.0))
            }
        }
    }

    pub struct Max(pub usize);
    impl Rule for Max {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.chars().count() <= self.0 {
                Ok(())
            } else {
                Err(format!("must be at most {} characters", self.0))
            }
        }
    }
}
```

> **Note:** A full `Email` validator should defer to the existing `validator` crate's `validate_email` to share semantics with `#[validate(email)]`. Replace the body with `validator::ValidateEmail::validate_email(&value).then_some(()).ok_or("must be a valid email".into())` (or equivalent) once the test passes — keep semantics aligned with the existing `validator` integration.

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test validation_rules
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/validation/rule.rs framework/tests/validation_rules.rs
git commit -m "feat(validation): Rule trait with Required/Email/Min/Max built-ins"
```

- [ ] **Step 5: Conditional rule objects — failing test**

Laravel ships declarative conditional rules (`required_if:other,val`,
`required_with:other`, `required_unless:other,val`) so consumers don't
have to drop into an `after_validation` closure for trivial conditional
requireds. We mirror the API as rule objects that read other field
values from a `&FormContext` map.

```rust
// framework/tests/validation_rules.rs — append
use suprnova::validation::{
    ContextualRule, FormContext,
    rules::{RequiredIf, RequiredWith, RequiredUnless},
};
use std::collections::HashMap;

fn ctx(pairs: &[(&str, &str)]) -> FormContext {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

#[test]
fn required_if_triggers_when_other_field_matches() {
    let rule = RequiredIf { other: "billing_type", value: "card" };
    let c = ctx(&[("billing_type", "card")]);
    assert!(rule.passes("4111111111111111", &c).is_ok(), "present + matching = ok");
    assert!(rule.passes("", &c).is_err(), "empty + matching = err");

    let c2 = ctx(&[("billing_type", "invoice")]);
    assert!(rule.passes("", &c2).is_ok(), "empty + not matching = ok (rule inactive)");
}

#[test]
fn required_with_triggers_when_other_field_present() {
    let rule = RequiredWith { other: "address_line_1" };
    let c = ctx(&[("address_line_1", "1 Main St")]);
    assert!(rule.passes("12345", &c).is_ok());
    assert!(rule.passes("", &c).is_err());

    let c2 = ctx(&[]);
    assert!(rule.passes("", &c2).is_ok(), "other absent → rule inactive");
}

#[test]
fn required_unless_triggers_when_other_field_does_not_match() {
    let rule = RequiredUnless { other: "subscription", value: "free" };
    let c_free = ctx(&[("subscription", "free")]);
    assert!(rule.passes("", &c_free).is_ok(), "matching unless = inactive");

    let c_paid = ctx(&[("subscription", "pro")]);
    assert!(rule.passes("billing_token", &c_paid).is_ok());
    assert!(rule.passes("", &c_paid).is_err());
}
```

- [ ] **Step 6: Run — expect failure**

```bash
cargo test -p suprnova --test validation_rules required_if required_with required_unless
```

Expected: FAIL (`ContextualRule` etc. not defined).

- [ ] **Step 7: Implement contextual rules**

```rust
// framework/src/validation/rule.rs — append below the existing Rule mod

use std::collections::HashMap;

/// Map of "other field name → its current string value", supplied to
/// rules that need to read sibling fields during validation.
pub type FormContext = HashMap<String, String>;

/// A rule that needs visibility into other form fields. Distinct
/// trait from `Rule` so the validation runner can dispatch to either
/// shape without type erasure tricks.
pub trait ContextualRule {
    fn passes(&self, value: &str, ctx: &FormContext) -> Result<(), String>;
}

/// Bridge — every `Rule` is trivially a `ContextualRule` that ignores
/// the context.
impl<R: Rule> ContextualRule for R {
    fn passes(&self, value: &str, _ctx: &FormContext) -> Result<(), String> {
        <R as Rule>::passes(self, value)
    }
}

pub mod rules {
    use super::{ContextualRule, FormContext, Rule};

    fn is_blank(value: &str) -> bool {
        value.trim().is_empty()
    }

    /// `required_if:other_field,value` — value is required iff
    /// `ctx[other_field] == value`.
    pub struct RequiredIf {
        pub other: &'static str,
        pub value: &'static str,
    }
    impl ContextualRule for RequiredIf {
        fn passes(&self, value: &str, ctx: &FormContext) -> Result<(), String> {
            let other_matches = ctx
                .get(self.other)
                .map(|v| v == self.value)
                .unwrap_or(false);
            if other_matches && is_blank(value) {
                Err(format!("required when {} is {}", self.other, self.value))
            } else {
                Ok(())
            }
        }
    }

    /// `required_with:other_field` — value is required iff
    /// `ctx[other_field]` exists and is non-blank.
    pub struct RequiredWith {
        pub other: &'static str,
    }
    impl ContextualRule for RequiredWith {
        fn passes(&self, value: &str, ctx: &FormContext) -> Result<(), String> {
            let other_present = ctx
                .get(self.other)
                .map(|v| !is_blank(v))
                .unwrap_or(false);
            if other_present && is_blank(value) {
                Err(format!("required when {} is present", self.other))
            } else {
                Ok(())
            }
        }
    }

    /// `required_unless:other_field,value` — value is required UNLESS
    /// `ctx[other_field] == value`.
    pub struct RequiredUnless {
        pub other: &'static str,
        pub value: &'static str,
    }
    impl ContextualRule for RequiredUnless {
        fn passes(&self, value: &str, ctx: &FormContext) -> Result<(), String> {
            let other_matches = ctx
                .get(self.other)
                .map(|v| v == self.value)
                .unwrap_or(false);
            if !other_matches && is_blank(value) {
                Err(format!("required unless {} is {}", self.other, self.value))
            } else {
                Ok(())
            }
        }
    }
}
```

- [ ] **Step 8: Wire ContextualRule into the FormRequest validation runner**

Open `framework/src/http/form_request.rs` (or wherever the
`#[derive(FormRequest)]` runner currently lives) and add a path so a
`#[validate(rule = "...")]` annotation can target `ContextualRule`
implementors. The macro generates a per-field check that builds a
`FormContext` from all sibling fields then calls
`rule.passes(value, &ctx)`.

The macro attribute extension:

```rust
#[derive(FormRequest)]
pub struct BillingForm {
    pub billing_type: String,

    #[validate(rule = "RequiredIf { other: \"billing_type\", value: \"card\" }")]
    pub card_number: String,
}
```

> **Implementation note:** The macro stringifies the rule expression
> and emits `let __rule = #expr; __rule.passes(&self.#field, &__ctx)`.
> Build `__ctx` once per validate call by serializing every field of
> `self` to its `Display`/`ToString` impl and collecting into a
> `FormContext`.

- [ ] **Step 9: Run — expect pass**

```bash
cargo test -p suprnova --test validation_rules
```

Expected: all 7 rule tests pass (4 unconditional + 3 conditional).

- [ ] **Step 10: Commit**

```bash
git add framework/src/validation/rule.rs framework/src/http/form_request.rs suprnova-macros framework/tests/validation_rules.rs
git commit -m "feat(validation): ContextualRule + RequiredIf/RequiredWith/RequiredUnless"
```

---

## Task 7: ErrorBag — named-scope validation errors

**Files:** `framework/src/validation/error_bag.rs`, `framework/src/error.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/validation_rules.rs — append
use suprnova::ValidationErrors;

#[test]
fn error_bag_scopes_default_and_named() {
    let mut errs = ValidationErrors::new();
    errs.add("email", "invalid");
    errs.add_to_bag("profile", "bio", "too long");
    errs.add_to_bag("profile", "avatar", "missing");

    let json = errs.to_json();
    assert_eq!(json["errors"]["email"][0], "invalid");
    // Bag-scoped errors prefixed with bag name:
    assert!(json["errors"]["profile.bio"][0].as_str().unwrap().contains("too long"));
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/error.rs — append to impl ValidationErrors
impl ValidationErrors {
    /// Add an error scoped under a named bag (Laravel's
    /// `withErrors($errors, 'profile')`). The scope name is
    /// prepended to the field key with a `.` separator.
    pub fn add_to_bag(
        &mut self,
        bag: impl AsRef<str>,
        field: impl Into<String>,
        message: impl Into<String>,
    ) {
        let scoped = format!("{}.{}", bag.as_ref(), field.into());
        self.add(scoped, message);
    }
}
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test validation_rules error_bag
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/error.rs framework/tests/validation_rules.rs
git commit -m "feat(validation): add_to_bag scopes errors under named bag"
```

---

## Task 8: FormRequest::after_validation hook

**Files:** `framework/src/http/form_request.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/validation_rules.rs — append
use suprnova::{FormRequest, ValidationErrors};
use serde::Deserialize;
use validator::Validate;

#[derive(Deserialize, Validate)]
struct UpdatePassword {
    #[validate(length(min = 8))]
    new_password: String,
    confirmation: String,
}

impl FormRequest for UpdatePassword {
    fn after_validation(&self) -> Result<(), ValidationErrors> {
        if self.new_password != self.confirmation {
            let mut errs = ValidationErrors::new();
            errs.add("confirmation", "passwords do not match");
            return Err(errs);
        }
        Ok(())
    }
}

// Helper to construct a FormRequest in tests without a Request:
#[test]
fn after_validation_runs_for_cross_field_checks() {
    let req = UpdatePassword {
        new_password: "longenough".into(),
        confirmation: "different".into(),
    };
    let result = req.after_validation();
    assert!(result.is_err());
    let errs = result.unwrap_err();
    assert!(errs.errors.contains_key("confirmation"));
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/http/form_request.rs — extend the FormRequest trait
pub trait FormRequest: serde::de::DeserializeOwned + validator::Validate + Send + Sync + 'static {
    /// Authorization hook. Default returns true; override to gate
    /// the request on an auth check before validation runs.
    fn authorize(&self) -> bool {
        true
    }

    /// Cross-field validation hook. Called AFTER the derived
    /// `Validate` rules pass. Return `Err(ValidationErrors)` to
    /// surface additional errors (e.g. "passwords must match").
    fn after_validation(&self) -> Result<(), ValidationErrors> {
        Ok(())
    }

    // ... existing extract(...) signature unchanged ...
}
```

Modify the existing `extract` implementation to call `after_validation`:

```rust
// framework/src/http/form_request.rs — inside extract(), after the
// validator::Validate check passes:
if let Err(errs) = parsed.after_validation() {
    return Err(FrameworkError::PrecognitionFailure(errs)); // or Validation, depending on path
}
```

> **Decision point:** Whether `after_validation` failures fire Precognition or the regular validation envelope depends on whether the request carried the Precognition header. The current `extract()` body already branches; insert the `after_validation` call inside the same branches so the response envelope matches.

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test validation_rules after_validation
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/http/form_request.rs framework/tests/validation_rules.rs
git commit -m "feat(validation): FormRequest::after_validation hook for cross-field checks"
```

---

## Task 9: App dogfood — avatar upload endpoint

**Files:** `app/src/controllers/avatar_upload.rs`, `app/src/bootstrap.rs`

- [ ] **Step 1: Register disks in bootstrap**

```rust
// app/src/bootstrap.rs — inside register(), after DB init
use suprnova::{Storage, S3Config};

// Local public disk for development
Storage::register_fs("public", "./storage/public").expect("register public disk");

// S3 disk for production uploads — env-driven
if let Ok(bucket) = std::env::var("S3_BUCKET") {
    Storage::register_s3("uploads", S3Config {
        bucket,
        region: std::env::var("AWS_REGION").ok(),
        endpoint: std::env::var("S3_ENDPOINT").ok(),
        access_key_id: std::env::var("AWS_ACCESS_KEY_ID").ok(),
        secret_access_key: std::env::var("AWS_SECRET_ACCESS_KEY").ok(),
        root: std::env::var("S3_ROOT").ok(),
    }).expect("register S3 uploads disk");
}
```

- [ ] **Step 2: Avatar upload controller**

```rust
// app/src/controllers/avatar_upload.rs
use suprnova::{
    json_response, Auth, FrameworkError, Image, MaxSize, Request, Response, Storage,
    UploadedFile,
};

pub async fn upload(
    file: UploadedFile<"avatar", (Image, MaxSize::<5_242_880>)>,
    _req: Request,
) -> Response {
    let user = Auth::user_as::<crate::models::User>()
        .await?
        .ok_or(FrameworkError::Unauthorized)?;
    let path = format!("avatars/{}.bin", user.id);
    let disk = Storage::disk("public")?;
    file.store_as(&disk, &path).await?;
    json_response!({ "stored_at": path })
}
```

> **Tuple validator note:** `UploadedFile<"avatar", (Image, MaxSize::<5_242_880>)>` requires a `UploadValidator` impl for `(A, B)` that runs both validators. Add to `validators.rs`:

```rust
impl<A: UploadValidator + Default, B: UploadValidator + Default> UploadValidator for (A, B) {
    fn validate_chunk(&self, acc: &[u8]) -> Result<(), FrameworkError> {
        self.0.validate_chunk(acc)?;
        self.1.validate_chunk(acc)
    }
    fn validate_final(&self, full: &[u8], ct: Option<&str>) -> Result<(), FrameworkError> {
        self.0.validate_final(full, ct)?;
        self.1.validate_final(full, ct)
    }
}

impl<A: Default, B: Default> Default for (A, B) {
    fn default() -> Self {
        (A::default(), B::default())
    }
}
```

- [ ] **Step 3: Smoke test**

```bash
mkdir -p storage/public
cargo run -p app -- serve &
sleep 2
# Auth, then POST a real image
curl -F avatar=@/path/to/test.jpg http://127.0.0.1:8000/users/avatar
ls storage/public/avatars/
kill %1
```

- [ ] **Step 4: Commit**

```bash
git add app/src/controllers/avatar_upload.rs app/src/bootstrap.rs framework/src/http/upload/validators.rs
git commit -m "feat(app): avatar upload dogfood with Image+MaxSize validators on local disk"
```

---

## Task 10: Workspace lint + final verification + roadmap update

- [ ] **Step 1: Clippy + tests**

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

- [ ] **Step 2: Update ROADMAP "Where we are"**

Move from "Missing"/"Partial" to "Production-ready":
- Filesystem (Storage facade + opendal-backed drivers)
- File uploads (UploadedFile streaming + validators)
- Validation parity (Rule objects, after-hooks, ErrorBag)

- [ ] **Step 3: Commit + push**

```bash
git add ROADMAP.md
git commit -m "docs(roadmap): mark Phase 4 (filesystem + uploads + validation) complete"
git push
```

---

## Self-Review

| Spec item | Covered by |
|-----------|------------|
| Storage::disk facade | Task 2 |
| opendal-backed drivers (fs/s3/azblob/gcs/memory) | Task 2 |
| Cross-disk streaming | Task 3 |
| Storage::fake for tests | Task 2 |
| Multipart streaming parser | Task 4 |
| UploadedFile extractor | Task 5 |
| MaxSize byte-boundary rejection | Task 5 |
| Image magic-bytes validator | Task 5 |
| store_as direct-to-disk | Task 5 |
| Rule objects (Required/Email/Min/Max/Unique) | Task 6 Steps 1-4 |
| ContextualRule + RequiredIf/RequiredWith/RequiredUnless (conditional rule objects) | Task 6 Steps 5-10 |
| ErrorBag named scopes | Task 7 |
| after_validation hook | Task 8 |
| App dogfood | Task 9 |

**Placeholder scan:** Clean. `> Decision point:` and `> Implementation note:` blocks specify concrete files/branches to confirm.

---

## Execution Handoff

**Subagent-Driven (recommended) or Inline Execution.**
