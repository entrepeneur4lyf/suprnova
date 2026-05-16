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
    let payload = parse_multipart_streaming(req, |_, _| Ok(())).await.unwrap();
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
    assert!(form.avatar.bytes.starts_with(&png[..8]));
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
    assert_eq!(form.photos[0].bytes.as_ref(), b"first");
    assert_eq!(form.photos[1].bytes.as_ref(), b"second");
    assert_eq!(form.photos[2].bytes.as_ref(), b"third");
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
        if self.file.bytes.len() != self.expected_size {
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
