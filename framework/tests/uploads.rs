//! Integration tests for streaming multipart uploads.
//!
//! Covers the `#[derive(MultipartRequest)]` extractor end-to-end:
//! all six field shapes (file scalar/option/vec, text scalar/option/vec),
//! byte-boundary short-circuit on oversize, magic-byte content sniffing,
//! `authorize` and `after_validation` hooks.

mod common;

use common::{build_multipart_body, request_from_multipart};
use suprnova::http::upload::validators::{Image, MaxSize};
use suprnova::http::upload::{MultipartRequestHooks, UploadedFile};
use suprnova::{FromRequest, MultipartRequest, Request, ValidationErrors};

#[derive(MultipartRequest)]
struct AvatarUpload {
    #[field("avatar")]
    avatar: UploadedFile<(Image, MaxSize<5_242_880>)>,
    #[field("caption")]
    caption: Option<String>,
}

// A minimal valid PNG: 8-byte signature + IHDR chunk. infer recognises it.
fn tiny_png() -> Vec<u8> {
    let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x0D]);
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&[0; 13]);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes
}

// ── Smoke: low-level parser sees both fields ──

#[tokio::test]
async fn multipart_parses_two_fields_via_helper() {
    use suprnova::http::upload::parse_multipart_streaming;
    let body = build_multipart_body(
        "test",
        &[
            ("avatar", Some("a.bin"), b"image-bytes"),
            ("caption", None, b"hello"),
        ],
    );
    let req = request_from_multipart("test", body).await;
    let payload = parse_multipart_streaming(req, |_, _, _| Ok(())).await.unwrap();
    let names: Vec<&str> = payload.fields.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"avatar"));
    assert!(names.contains(&"caption"));
}

// ── Derive macro: file + optional text ──

#[tokio::test]
async fn derive_extracts_avatar_and_caption() {
    let png = tiny_png();
    let body = build_multipart_body(
        "test",
        &[
            ("avatar", Some("a.png"), &png),
            ("caption", None, b"hello world"),
        ],
    );
    let req = request_from_multipart("test", body).await;
    let form = AvatarUpload::from_request(req).await.unwrap();
    let avatar_bytes = form.avatar.bytes().await.unwrap();
    assert!(avatar_bytes.starts_with(&png[..8]));
    assert_eq!(form.caption.as_deref(), Some("hello world"));
}

#[tokio::test]
async fn derive_rejects_oversize_at_byte_boundary() {
    let big = vec![0u8; 6 * 1024 * 1024];
    let body = build_multipart_body("test", &[("avatar", Some("big.bin"), &big)]);
    let req = request_from_multipart("test", body).await;
    let err = AvatarUpload::from_request(req)
        .await
        .err()
        .expect("oversize body should fail");
    assert_eq!(err.status_code(), 413);
}

#[tokio::test]
async fn derive_rejects_non_image_via_magic_bytes() {
    let pdf = b"%PDF-1.4 lorem ipsum dolor sit amet".to_vec();
    let body = build_multipart_body("test", &[("avatar", Some("not.png"), &pdf)]);
    let req = request_from_multipart("test", body).await;
    let err = AvatarUpload::from_request(req)
        .await
        .err()
        .expect("non-image bytes should fail Image validator");
    assert_eq!(err.status_code(), 422);
}

// ── Array uploads ──

#[derive(MultipartRequest)]
struct Gallery {
    #[field("photos")]
    photos: Vec<UploadedFile<MaxSize<1_048_576>>>,
}

#[tokio::test]
async fn derive_collects_array_uploads() {
    let body = build_multipart_body(
        "test",
        &[
            ("photos", Some("a.bin"), b"first"),
            ("photos", Some("b.bin"), b"second"),
            ("photos", Some("c.bin"), b"third"),
        ],
    );
    let req = request_from_multipart("test", body).await;
    let form = Gallery::from_request(req).await.unwrap();
    assert_eq!(form.photos.len(), 3);
    assert_eq!(form.photos[0].bytes().await.unwrap().as_ref(), b"first");
    assert_eq!(form.photos[1].bytes().await.unwrap().as_ref(), b"second");
    assert_eq!(form.photos[2].bytes().await.unwrap().as_ref(), b"third");
}

// ── FromStr text parsing ──

#[derive(MultipartRequest)]
struct Submission {
    #[field("priority")]
    priority: u32,
}

#[tokio::test]
async fn derive_parses_text_field_via_fromstr() {
    let body = build_multipart_body("test", &[("priority", None, b"42")]);
    let req = request_from_multipart("test", body).await;
    let form = Submission::from_request(req).await.unwrap();
    assert_eq!(form.priority, 42);
}

#[tokio::test]
async fn derive_rejects_unparseable_text_field() {
    let body = build_multipart_body("test", &[("priority", None, b"not-a-number")]);
    let req = request_from_multipart("test", body).await;
    let err = Submission::from_request(req)
        .await
        .err()
        .expect("unparseable text should fail FromStr");
    assert_eq!(err.status_code(), 400);
}

// ── Hooks ──

#[derive(MultipartRequest)]
#[multipart(custom_hooks)]
#[allow(dead_code)] // `file` exists to exercise the macro's required-file
                    // path; the test never reaches it because `authorize`
                    // short-circuits with Unauthorized before parsing.
struct GuardedUpload {
    #[field("file")]
    file: UploadedFile,
}

impl MultipartRequestHooks for GuardedUpload {
    fn authorize(_req: &Request) -> bool {
        false
    }
}

#[tokio::test]
async fn derive_authorize_hook_short_circuits_with_unauthorized() {
    let body = build_multipart_body("test", &[("file", Some("a.bin"), b"data")]);
    let req = request_from_multipart("test", body).await;
    let err = GuardedUpload::from_request(req)
        .await
        .err()
        .expect("authorize returning false should fail");
    // FrameworkError::Unauthorized maps to 403 per framework/src/error.rs.
    assert_eq!(err.status_code(), 403);
}

#[derive(MultipartRequest)]
#[multipart(custom_hooks)]
struct ChecksumForm {
    #[field("file")]
    file: UploadedFile,
    #[field("expected_size")]
    expected_size: usize,
}

impl MultipartRequestHooks for ChecksumForm {
    fn after_validation(&self) -> Result<(), ValidationErrors> {
        // `after_validation` is sync — no `.await`. Use the pre-computed
        // `size` field, which works for both memory- and disk-backed
        // uploads without re-reading bytes.
        if self.file.size as usize != self.expected_size {
            let mut errs = ValidationErrors::new();
            errs.add("file", "size mismatch");
            return Err(errs);
        }
        Ok(())
    }
}

#[tokio::test]
async fn derive_after_validation_hook_runs_after_construction() {
    let body = build_multipart_body(
        "test",
        &[
            ("file", Some("a.bin"), b"actual_data_is_14b"),
            ("expected_size", None, b"5"),
        ],
    );
    let req = request_from_multipart("test", body).await;
    let err = ChecksumForm::from_request(req)
        .await
        .err()
        .expect("size mismatch should fail after_validation");
    assert_eq!(err.status_code(), 422);
}

// ── extension_from_magic: magic-byte-derived storage extension ──

#[tokio::test]
async fn extension_from_magic_returns_canonical_for_png() {
    let png = tiny_png();
    let body = build_multipart_body("test", &[("avatar", Some("evil.exe"), &png)]);
    let req = request_from_multipart("test", body).await;
    let form = AvatarUpload::from_request(req).await.unwrap();
    // Filename says ".exe", magic bytes say PNG — magic wins.
    assert_eq!(form.avatar.extension_from_magic(), "png");
}

#[derive(MultipartRequest)]
struct Blob {
    #[field("file")]
    file: UploadedFile,
}

#[tokio::test]
async fn extension_from_magic_falls_back_to_bin_for_unknown_content() {
    // 32 zero bytes don't match any infer signature. The field uses the
    // no-op `UploadedFile<()>` so the Image validator isn't gating the
    // request; we want to reach `extension_from_magic` and observe the
    // `"bin"` fallback path.
    let unknown = vec![0u8; 32];
    let body = build_multipart_body("test", &[("file", Some("anything.tar"), &unknown)]);
    let req = request_from_multipart("test", body).await;
    let form = Blob::from_request(req).await.unwrap();
    assert_eq!(form.file.extension_from_magic(), "bin");
}

// ── Stateful validator: one instance threaded across chunk + final phases ──

use std::sync::atomic::{AtomicUsize, Ordering};

/// Stateful validator using `AtomicUsize` interior mutability. Counts
/// `validate_chunk` calls; `validate_final` returns `Err` if the count
/// is zero, which would mean the macro constructed a SEPARATE instance
/// for the final phase (the chunk-phase counter would have been
/// discarded with that other instance). Under the corrected macro the
/// same `&self` is used in both phases, so the counter survives and
/// `from_request` succeeds.
#[derive(Default)]
struct ChunkCounter {
    count: AtomicUsize,
}

impl suprnova::http::upload::validators::UploadValidator for ChunkCounter {
    fn validate_chunk(&self, _sniff: &[u8], _size: u64) -> Result<(), suprnova::FrameworkError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn validate_final(
        &self,
        _sniff: &[u8],
        _size: u64,
        _ct: Option<&str>,
    ) -> Result<(), suprnova::FrameworkError> {
        let chunks = self.count.load(Ordering::SeqCst);
        if chunks == 0 {
            return Err(suprnova::FrameworkError::Domain {
                message: "validate_final saw zero chunks — instances not threaded".into(),
                status_code: 500,
            });
        }
        Ok(())
    }
}

#[derive(MultipartRequest)]
struct ProbedUpload {
    #[field("data")]
    #[allow(dead_code)] // we just need the macro to invoke the validator
    data: UploadedFile<ChunkCounter>,
}

#[tokio::test]
async fn validator_instance_is_threaded_across_chunk_and_final() {
    // 256 KiB body, well above multer's internal chunk size, so the
    // streaming path produces at least one `validate_chunk` call before
    // `validate_final` runs.
    let body = build_multipart_body("test", &[("data", Some("a.bin"), &[0u8; 256_000])]);
    let req = request_from_multipart("test", body).await;
    // If the macro constructed separate instances per phase, the final
    // phase would see count=0 and return Err. The same-instance hoist
    // means count > 0 and the request succeeds.
    let result = ProbedUpload::from_request(req).await;
    assert!(
        result.is_ok(),
        "validator instance must persist from chunk to final phase: {:?}",
        result.err().map(|e| e.to_string()),
    );
}

// ── Multipart body cap & spill threshold: default / global override / per-struct override ──
//
// These tests mutate two independent process-global atomics (body cap
// and spill threshold). Cargo runs tests within a single integration
// binary in parallel by default, so concurrent mutations would race.
// The `UploadGlobalsGuard` below combines a poison-tolerant Mutex with
// RAII reset-to-default-on-drop covering BOTH atomics so even if a test
// panics mid-assertion the next one starts clean. We share one mutex
// across body-cap and spill-threshold tests — both atomics are global,
// so simultaneous mutation from sibling tests would still race even if
// each used a separate lock.

use std::sync::Mutex;

static UPLOAD_GLOBALS_LOCK: Mutex<()> = Mutex::new(());

struct UploadGlobalsGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl UploadGlobalsGuard {
    fn acquire() -> Self {
        let guard = UPLOAD_GLOBALS_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Always start from the compile-time defaults for both atomics.
        suprnova::http::upload::set_global_max_multipart_body_bytes(0);
        suprnova::http::upload::set_global_upload_spill_threshold(0);
        Self { _guard: guard }
    }
}

impl Drop for UploadGlobalsGuard {
    fn drop(&mut self) {
        // Restore the defaults for any test that doesn't take the lock
        // (the non-cap/threshold tests don't touch the globals, but a
        // stale override could still leak across test invocations if we
        // skipped this).
        suprnova::http::upload::set_global_max_multipart_body_bytes(0);
        suprnova::http::upload::set_global_upload_spill_threshold(0);
    }
}

#[derive(suprnova::MultipartRequest)]
struct UncappedBlob {
    #[field("file")]
    #[allow(dead_code)] // the assertion is on status code, not the constructed value
    file: suprnova::UploadedFile,
}

#[tokio::test]
async fn body_cap_uses_default_when_no_override() {
    let _g = UploadGlobalsGuard::acquire();

    // 26 MiB body — exceeds the 25 MiB compile-time default.
    let big = vec![0u8; 26 * 1024 * 1024];
    let body = build_multipart_body("test", &[("file", Some("a.bin"), &big)]);
    let req = request_from_multipart("test", body).await;
    let err = UncappedBlob::from_request(req)
        .await
        .err()
        .expect("default 25 MiB cap should reject 26 MiB body");
    assert_eq!(err.status_code(), 413);
}

#[tokio::test]
async fn body_cap_respects_global_override() {
    let _g = UploadGlobalsGuard::acquire();

    // Set a 1 MiB process-global cap.
    suprnova::http::upload::set_global_max_multipart_body_bytes(1024 * 1024);

    let two_mb = vec![0u8; 2 * 1024 * 1024];
    let body = build_multipart_body("test", &[("file", Some("a.bin"), &two_mb)]);
    let req = request_from_multipart("test", body).await;
    let err = UncappedBlob::from_request(req)
        .await
        .err()
        .expect("global 1 MiB cap should reject 2 MiB body");
    assert_eq!(err.status_code(), 413);
}

#[derive(suprnova::MultipartRequest)]
#[multipart(max_body_bytes = 512)]
struct TinyBlob {
    #[field("file")]
    file: suprnova::UploadedFile,
}

#[tokio::test]
async fn body_cap_per_struct_override_wins() {
    let _g = UploadGlobalsGuard::acquire();

    // Bump the global way up — per-struct should still apply.
    suprnova::http::upload::set_global_max_multipart_body_bytes(100 * 1024 * 1024);

    // 1 KiB body — under global, over the per-struct 512-byte cap.
    let kb = vec![0u8; 1024];
    let body = build_multipart_body("test", &[("file", Some("a.bin"), &kb)]);
    let req = request_from_multipart("test", body).await;
    let err = TinyBlob::from_request(req)
        .await
        .err()
        .expect("per-struct 512-byte cap should reject 1 KiB body");
    assert_eq!(err.status_code(), 413);
}

#[tokio::test]
async fn body_cap_per_struct_under_cap_succeeds() {
    let _g = UploadGlobalsGuard::acquire();

    // 256-byte body, under the per-struct 512-byte cap — should succeed.
    let small = vec![0u8; 256];
    let body = build_multipart_body("test", &[("file", Some("a.bin"), &small)]);
    let req = request_from_multipart("test", body).await;
    let form = TinyBlob::from_request(req).await.unwrap();
    assert_eq!(form.file.size, 256);
}

// ── Spill-to-disk: true streaming for large parts ──

#[derive(suprnova::MultipartRequest)]
struct AnyFile {
    #[field("file")]
    file: suprnova::UploadedFile,
}

#[tokio::test]
async fn upload_spills_to_disk_above_threshold() {
    let _g = UploadGlobalsGuard::acquire();

    // Drop the spill threshold so a small body forces the disk path.
    // Bump the body cap so the cap doesn't reject first.
    suprnova::http::upload::set_global_upload_spill_threshold(1024); // 1 KiB
    suprnova::http::upload::set_global_max_multipart_body_bytes(10 * 1024 * 1024);

    let big = vec![7u8; 4 * 1024]; // 4 KiB — comfortably above the 1 KiB threshold
    let body = build_multipart_body("test", &[("file", Some("big.bin"), &big)]);
    let req = request_from_multipart("test", body).await;

    let form = AnyFile::from_request(req).await.unwrap();
    assert_eq!(form.file.size, 4 * 1024);
    // The async `bytes()` accessor must round-trip the spilled file
    // identical to what was uploaded.
    let bytes = form.file.bytes().await.unwrap();
    assert_eq!(bytes.len(), 4 * 1024);
    assert!(bytes.iter().all(|b| *b == 7u8));
}

#[tokio::test]
async fn upload_stays_in_memory_below_threshold() {
    let _g = UploadGlobalsGuard::acquire();

    // Default 2 MiB spill threshold (set via `0` sentinel above). A 1 KiB
    // body must NOT trigger the disk path — assertion is content-equality
    // round-tripped through the in-memory accessor.
    let small = vec![3u8; 1024];
    let body = build_multipart_body("test", &[("file", Some("small.bin"), &small)]);
    let req = request_from_multipart("test", body).await;

    let form = AnyFile::from_request(req).await.unwrap();
    assert_eq!(form.file.size, 1024);
    let bytes = form.file.bytes().await.unwrap();
    assert_eq!(bytes.len(), 1024);
    assert!(bytes.iter().all(|b| *b == 3u8));
}

#[tokio::test]
async fn store_as_streams_disk_backed_part_to_storage() {
    use suprnova::Storage;
    let _g = UploadGlobalsGuard::acquire();
    // `Storage::fake()` serialises against other Storage tests via its
    // own internal mutex; our `UploadGlobalsGuard` mutex is independent
    // (covers the spill/cap atomics). Acquiring both is fine — they
    // don't share any state.
    let _storage_guard = Storage::fake();
    Storage::register_memory("spill_dest");

    // Force the spill path with a small threshold.
    suprnova::http::upload::set_global_upload_spill_threshold(1024);
    suprnova::http::upload::set_global_max_multipart_body_bytes(10 * 1024 * 1024);

    // 4 KiB body — must spill. Non-uniform content so we can detect
    // truncation or corruption in the round-trip assertion.
    let mut big = Vec::with_capacity(4 * 1024);
    for i in 0..(4 * 1024) {
        big.push((i % 251) as u8); // 251 keeps a clear pattern without trivial repeats.
    }
    let body = build_multipart_body("test", &[("file", Some("big.bin"), &big)]);
    let req = request_from_multipart("test", body).await;

    let form = AnyFile::from_request(req).await.unwrap();
    let disk = Storage::disk("spill_dest").unwrap();
    form.file
        .store_as(&disk, "stored/big.bin")
        .await
        .expect("store_as must stream disk-backed parts");

    // Read the stored object back and assert byte-for-byte equality with
    // the original upload — this proves the streaming copy preserved
    // every byte (no early termination, no truncation, no double-write).
    let stored = disk.read("stored/big.bin").await.unwrap();
    assert_eq!(stored.to_vec(), big);
}
