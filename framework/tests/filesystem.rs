//! Integration tests for the `Storage` facade.
//!
//! Each test acquires a `Storage::fake()` guard which both serializes against
//! other fake-using tests (via a process-wide mutex inside the guard) and
//! resets the global disk registry on drop. This lets the tests safely run
//! under the default parallel `cargo test` runner without registry collisions.

use suprnova::filesystem::streaming::copy_between_disks;
use suprnova::Storage;

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
