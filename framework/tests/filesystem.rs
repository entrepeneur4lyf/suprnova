//! Integration tests for the `Storage` facade.
//!
//! Each test acquires a `Storage::fake()` guard which both serializes against
//! other fake-using tests (via a process-wide mutex inside the guard) and
//! resets the global disk registry on drop. This lets the tests safely run
//! under the default parallel `cargo test` runner without registry collisions.

use opendal::layers::RetryLayer;
use suprnova::filesystem::streaming::copy_between_disks;
use suprnova::{S3Config, Storage};

#[tokio::test]
async fn memory_disk_round_trip() {
    let _guard = Storage::fake();
    Storage::register_memory("test");

    let disk = Storage::disk("test").expect("registered memory disk");
    disk.write("hello.txt", "hello world")
        .await
        .expect("write succeeds");

    let bytes = disk.read("hello.txt").await.expect("read succeeds");
    assert_eq!(&bytes.to_vec(), b"hello world");
}

#[tokio::test]
async fn unknown_disk_returns_error() {
    let _guard = Storage::fake();

    let err = Storage::disk("does-not-exist").expect_err("unknown disk must error");
    let msg = err.to_string();
    assert!(
        msg.contains("does-not-exist"),
        "error should name the missing disk, got: {msg}"
    );
}

#[tokio::test]
async fn fs_disk_writes_to_temp_dir() {
    let _guard = Storage::fake();
    let tmp = tempfile::tempdir().expect("create tempdir");

    Storage::register_fs("tmp", tmp.path()).expect("fs disk init");
    let disk = Storage::disk("tmp").expect("registered fs disk");

    let payload: &[u8] = b"binary";
    disk.write("nested/file.bin", payload.to_vec())
        .await
        .expect("write succeeds");

    let on_disk = tmp.path().join("nested/file.bin");
    assert!(on_disk.exists(), "file must exist at {on_disk:?}");
    assert_eq!(
        std::fs::read(&on_disk).expect("read back from disk"),
        b"binary"
    );
}

#[tokio::test]
async fn fake_default_disk_is_preregistered() {
    let _guard = Storage::fake();

    // Storage::fake() pre-registers a "default" memory disk for convenience.
    let disk = Storage::disk("default").expect("default disk available under fake");
    disk.write("a.txt", "ok").await.expect("write to default");
    let got = disk.read("a.txt").await.expect("read from default");
    assert_eq!(&got.to_vec(), b"ok");
}

#[tokio::test]
async fn fake_guard_resets_registry_on_drop() {
    {
        let _guard = Storage::fake();
        Storage::register_memory("ephemeral");
        assert!(Storage::disk("ephemeral").is_ok());
    }
    // After the guard drops, the registry is wiped and "ephemeral" is gone.
    // The mutex inside the guard prevents other tests from observing the
    // intermediate state, so this assertion is deterministic.
    let _guard = Storage::fake();
    assert!(
        Storage::disk("ephemeral").is_err(),
        "guard drop must reset the registry"
    );
}

#[tokio::test]
async fn streaming_copy_moves_bytes_between_disks() {
    let _guard = Storage::fake();
    Storage::register_memory("src");
    Storage::register_memory("dest");
    let src = Storage::disk("src").unwrap();
    src.write("biggie.bin", vec![0u8; 1_000_000]).await.unwrap();

    let copied = copy_between_disks("src", "biggie.bin", "dest", "moved.bin")
        .await
        .unwrap();
    assert_eq!(copied, 1_000_000, "must report total bytes copied");

    let dest = Storage::disk("dest").unwrap();
    let bytes = dest.read("moved.bin").await.unwrap();
    assert_eq!(bytes.len(), 1_000_000);
}

#[tokio::test]
async fn streaming_copy_preserves_bytes_exactly() {
    let _guard = Storage::fake();
    Storage::register_memory("src");
    Storage::register_memory("dest");
    let src = Storage::disk("src").unwrap();

    // Mixed pattern so a partial copy or chunk-boundary bug is detectable.
    let pattern: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    src.write("mixed.bin", pattern.clone()).await.unwrap();

    copy_between_disks("src", "mixed.bin", "dest", "mixed.bin")
        .await
        .unwrap();

    let dest = Storage::disk("dest").unwrap();
    let copied = dest.read("mixed.bin").await.unwrap();
    assert_eq!(copied.to_vec(), pattern, "every byte must match");
}

#[tokio::test]
async fn streaming_copy_errors_on_missing_source_disk() {
    let _guard = Storage::fake();
    Storage::register_memory("dest");
    let err = copy_between_disks("nope", "a", "dest", "b")
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("nope") || msg.contains("not registered"),
        "error should identify the missing disk, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// `register_<driver>_with` — opendal layer composition.
// ---------------------------------------------------------------------------
//
// These tests prove that the closure passed to `register_<driver>_with`
// actually runs and that the layered `Operator` is what lands in the
// registry. We use `Arc<AtomicBool>` (NOT a `static`) so cross-test leakage
// is impossible under the default parallel test runner.

#[tokio::test]
async fn register_fs_with_layer_applies_to_operator() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let _guard = Storage::fake();
    let tmp = tempfile::tempdir().expect("create tempdir");

    let closure_ran = Arc::new(AtomicBool::new(false));
    let flag = closure_ran.clone();
    Storage::register_fs_with("layered_fs", tmp.path(), move |op| {
        flag.store(true, Ordering::SeqCst);
        op.layer(RetryLayer::new().with_max_times(3))
    })
    .expect("fs disk with layer registers");

    assert!(
        closure_ran.load(Ordering::SeqCst),
        "layer closure must run during registration"
    );

    // And the disk must still work end-to-end through the layered operator.
    let disk = Storage::disk("layered_fs").expect("registered layered fs disk");
    disk.write("hello.txt", "world").await.expect("write");
    assert_eq!(disk.read("hello.txt").await.expect("read").to_vec(), b"world");
}

#[tokio::test]
async fn register_memory_with_layer_composes() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let _guard = Storage::fake();

    let closure_ran = Arc::new(AtomicBool::new(false));
    let flag = closure_ran.clone();
    Storage::register_memory_with("layered_mem", move |op| {
        flag.store(true, Ordering::SeqCst);
        op.layer(RetryLayer::new().with_max_times(2))
    });

    assert!(
        closure_ran.load(Ordering::SeqCst),
        "memory layer closure must run"
    );

    let disk = Storage::disk("layered_mem").expect("registered layered memory disk");
    disk.write("k", "v").await.expect("write");
    assert_eq!(disk.read("k").await.expect("read").to_vec(), b"v");
}

#[tokio::test]
async fn register_s3_with_layer_compiles_and_registers() {
    let _guard = Storage::fake();

    // Build construction never touches the network — opendal validates the
    // config and produces an `Operator` lazily, so this must succeed even
    // without live credentials. We don't drive a real read/write here.
    let result = Storage::register_s3_with(
        "fake_s3",
        S3Config {
            bucket: "test-bucket".into(),
            region: Some("us-east-1".into()),
            endpoint: None,
            access_key_id: Some("AKIATEST".into()),
            secret_access_key: Some("secret".into()),
            root: None,
        },
        |op| op.layer(RetryLayer::new().with_max_times(3)),
    );
    assert!(
        result.is_ok(),
        "S3 registration with layer must succeed even without live credentials: {result:?}"
    );
    assert!(
        Storage::disk("fake_s3").is_ok(),
        "disk lookup must succeed after register_s3_with"
    );
}

#[tokio::test]
async fn register_s3_unchanged_still_works() {
    // Backwards-compat: the existing `register_s3` (no `_with`) is now a
    // thin wrapper around `register_s3_with` with an identity closure.
    // Existing call sites must not need to change.
    let _guard = Storage::fake();
    let result = Storage::register_s3(
        "plain_s3",
        S3Config {
            bucket: "test-bucket".into(),
            region: Some("us-east-1".into()),
            endpoint: None,
            access_key_id: Some("AKIATEST".into()),
            secret_access_key: Some("secret".into()),
            root: None,
        },
    );
    assert!(result.is_ok(), "register_s3 must still work: {result:?}");
    assert!(
        Storage::disk("plain_s3").is_ok(),
        "disk lookup must succeed after register_s3"
    );
}

// ---------------------------------------------------------------------------
// Newly-enabled opendal layers: logging, tracing, timeout, prometheus-client.
// ---------------------------------------------------------------------------
//
// These tests prove the layer features in `framework/Cargo.toml` resolve and
// that the layer types are reachable to consumers through the `register_*_with`
// entry points. We compose each layer onto a memory disk and round-trip a
// write/read — the assertions are that registration succeeds AND that the
// layered operator is functionally indistinguishable from an unlayered one for
// happy-path operations.

#[tokio::test]
async fn register_with_logging_layer_composes_and_round_trips() {
    use opendal::layers::LoggingLayer;

    let _guard = Storage::fake();
    Storage::register_memory_with("logged", |op| op.layer(LoggingLayer::default()));

    let disk = Storage::disk("logged").expect("logged disk available");
    disk.write("file.txt", "logged-write")
        .await
        .expect("write through LoggingLayer succeeds");
    let bytes = disk.read("file.txt").await.expect("read through LoggingLayer");
    assert_eq!(&bytes.to_vec(), b"logged-write");
}

#[tokio::test]
async fn register_with_tracing_layer_composes_and_round_trips() {
    use opendal::layers::TracingLayer;

    let _guard = Storage::fake();
    Storage::register_memory_with("traced", |op| op.layer(TracingLayer::new()));

    let disk = Storage::disk("traced").expect("traced disk available");
    disk.write("file.txt", "traced-write")
        .await
        .expect("write through TracingLayer succeeds");
    let bytes = disk.read("file.txt").await.expect("read through TracingLayer");
    assert_eq!(&bytes.to_vec(), b"traced-write");
}

#[tokio::test]
async fn register_with_timeout_layer_composes_and_round_trips() {
    use opendal::layers::TimeoutLayer;
    use std::time::Duration;

    let _guard = Storage::fake();
    Storage::register_memory_with("timed", |op| {
        op.layer(TimeoutLayer::new().with_timeout(Duration::from_secs(30)))
    });

    let disk = Storage::disk("timed").expect("timed disk available");
    disk.write("file.txt", "timed-write")
        .await
        .expect("write through TimeoutLayer succeeds within 30s");
    let bytes = disk.read("file.txt").await.expect("read through TimeoutLayer");
    assert_eq!(&bytes.to_vec(), b"timed-write");
}

#[tokio::test]
async fn register_with_prometheus_client_layer_composes_and_round_trips() {
    use opendal::layers::PrometheusClientLayer;
    use prometheus_client::registry::Registry;

    let _guard = Storage::fake();
    let mut registry = Registry::default();
    // `PrometheusClientLayer::builder` registers histograms + counters into
    // the registry; constructing it proves the feature flag works and the
    // crate's API is reachable. Different opendal 0.56 patch releases shape
    // this slightly differently — the builder pattern is the stable surface.
    let layer = PrometheusClientLayer::builder()
        .register(&mut registry);
    Storage::register_memory_with("metered", move |op| op.layer(layer));

    let disk = Storage::disk("metered").expect("metered disk available");
    disk.write("file.txt", "metered-write")
        .await
        .expect("write through PrometheusClientLayer succeeds");
    let bytes = disk.read("file.txt").await.expect("read through PrometheusClientLayer");
    assert_eq!(&bytes.to_vec(), b"metered-write");
}

#[tokio::test]
async fn register_with_full_production_layer_stack_round_trips() {
    // Compose the recommended production stack — retry, timeout, logging,
    // tracing — in the documented order. The test proves the full
    // composition produces a working operator and doesn't conflict between
    // layers.
    use opendal::layers::{LoggingLayer, RetryLayer, TimeoutLayer, TracingLayer};
    use std::time::Duration;

    let _guard = Storage::fake();
    Storage::register_memory_with("full_stack", |op| {
        op.layer(RetryLayer::new().with_max_times(3))
            .layer(TimeoutLayer::new().with_timeout(Duration::from_secs(30)))
            .layer(LoggingLayer::default())
            .layer(TracingLayer::new())
    });

    let disk = Storage::disk("full_stack").expect("full-stack disk available");
    disk.write("file.txt", "stacked-write")
        .await
        .expect("write through full layer stack succeeds");
    let bytes = disk.read("file.txt").await.expect("read through full layer stack");
    assert_eq!(&bytes.to_vec(), b"stacked-write");
}
