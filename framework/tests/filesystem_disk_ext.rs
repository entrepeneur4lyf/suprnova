//! End-to-end tests for the `DiskExt`/`DiskAssertExt` Laravel-shape surfaces
//! and the registry hygiene helpers (`Storage::forget`/`purge`/`disks`).
//!
//! Pair each test with `Storage::fake()` so the process-global registry is
//! isolated and serialized against other storage tests.

use suprnova::filesystem::testing::DiskAssertExt;
use suprnova::{ChecksumAlgorithm, DiskExt, Storage};

#[tokio::test]
async fn full_laravel_round_trip_on_memory_disk() {
    let _guard = Storage::fake();
    Storage::register_memory("workflow");
    let disk = Storage::disk("workflow").unwrap();

    // put / get / json / put_json
    disk.put("hello.txt", b"hi".to_vec()).await.unwrap();
    assert_eq!(disk.get("hello.txt").await.unwrap(), b"hi");

    disk.put_json("config.json", &serde_json::json!({"v": 1}))
        .await
        .unwrap();
    let cfg: serde_json::Value = disk.json("config.json").await.unwrap();
    assert_eq!(cfg["v"], 1);

    // prepend / append
    disk.append("log.txt", "first").await.unwrap();
    disk.append("log.txt", "second").await.unwrap();
    disk.prepend("log.txt", "zero").await.unwrap();
    assert_eq!(disk.get("log.txt").await.unwrap(), b"zero\nfirst\nsecond");

    // size / mime_type / checksum
    assert_eq!(disk.size("hello.txt").await.unwrap(), 2);
    assert!(disk.mime_type("hello.txt").await.unwrap().is_none());
    let sha = disk
        .checksum("hello.txt", ChecksumAlgorithm::Sha256)
        .await
        .unwrap();
    assert_eq!(
        sha,
        "8f434346648f6b96df89dda901c5176b10a6d83961dd3c1ac88b59b2dc327aa4"
    );

    // files / directories / all_files
    disk.put("docs/a.md", b"a".to_vec()).await.unwrap();
    disk.put("docs/b.md", b"b".to_vec()).await.unwrap();
    disk.make_directory("docs/nested/").await.unwrap();
    assert_eq!(
        disk.files("docs", false).await.unwrap(),
        vec!["docs/a.md", "docs/b.md"]
    );
    assert_eq!(
        disk.directories("docs", false).await.unwrap(),
        vec!["docs/nested"]
    );

    // move_to (falls back to copy+delete on the memory backend)
    disk.move_to("hello.txt", "moved.txt").await.unwrap();
    assert!(disk.missing("hello.txt").await.unwrap());
    assert_eq!(disk.get("moved.txt").await.unwrap(), b"hi");

    // delete_directory
    disk.delete_directory("docs/").await.unwrap();
    assert!(disk.directory_missing("docs/").await.unwrap());
}

#[tokio::test]
async fn assert_helpers_pass_on_happy_path() {
    let _guard = Storage::fake();
    Storage::register_memory("asserts");
    let disk = Storage::disk("asserts").unwrap();

    disk.put("present.txt", b"data".to_vec()).await.unwrap();
    disk.assert_exists("present.txt").await;
    disk.assert_contents("present.txt", b"data").await;
    disk.assert_missing("not-here.txt").await;

    disk.put("bucket/a.txt", b"x".to_vec()).await.unwrap();
    disk.put("bucket/b.txt", b"y".to_vec()).await.unwrap();
    disk.assert_count("bucket", 2, false).await;

    disk.assert_directory_empty("empty/").await;
}

#[tokio::test]
#[should_panic(expected = "expected disk path to exist")]
async fn assert_exists_panics_when_missing() {
    let _guard = Storage::fake();
    Storage::register_memory("missing");
    let disk = Storage::disk("missing").unwrap();
    disk.assert_exists("never-written.txt").await;
}

#[tokio::test]
#[should_panic(expected = "expected disk path to be missing")]
async fn assert_missing_panics_when_present() {
    let _guard = Storage::fake();
    Storage::register_memory("present");
    let disk = Storage::disk("present").unwrap();
    disk.put("a.txt", b"x".to_vec()).await.unwrap();
    disk.assert_missing("a.txt").await;
}

#[tokio::test]
#[should_panic(expected = "expected directory")]
async fn assert_count_panics_on_mismatch() {
    let _guard = Storage::fake();
    Storage::register_memory("countmiss");
    let disk = Storage::disk("countmiss").unwrap();
    disk.put("a/x.txt", b"x".to_vec()).await.unwrap();
    disk.assert_count("a", 5, false).await;
}

#[tokio::test]
async fn registry_forget_drops_a_disk() {
    let _guard = Storage::fake();
    Storage::register_memory("droppable");
    assert!(Storage::disk("droppable").is_ok());

    let removed = Storage::forget("droppable");
    assert!(removed, "forget must return true when the disk was present");

    assert!(
        Storage::disk("droppable").is_err(),
        "forgotten disk must no longer resolve"
    );

    // Forgetting an unknown name is a no-op that returns false.
    assert!(!Storage::forget("never-registered"));
}

#[tokio::test]
async fn registry_purge_clears_every_disk_but_keeps_the_registry_alive() {
    let _guard = Storage::fake();
    Storage::register_memory("one");
    Storage::register_memory("two");
    Storage::register_memory("three");
    assert_eq!(Storage::disks().len(), 4); // 3 + the pre-registered "default"

    Storage::purge();
    assert!(Storage::disks().is_empty());

    // The registry itself is still usable after purge — re-registration works.
    Storage::register_memory("post-purge");
    assert_eq!(Storage::disks(), vec!["post-purge".to_string()]);
}

#[tokio::test]
async fn disks_listing_is_sorted() {
    let _guard = Storage::fake();
    Storage::purge();
    Storage::register_memory("c");
    Storage::register_memory("a");
    Storage::register_memory("b");
    assert_eq!(
        Storage::disks(),
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
}
