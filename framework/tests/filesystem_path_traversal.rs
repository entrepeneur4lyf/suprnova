//! Path-traversal confinement for local-filesystem disks.
//!
//! A local `Storage` disk is rooted at a directory. Without a guard, opendal's
//! FS backend joins the caller-supplied path onto that root WITHOUT resolving
//! `..`, so `disk.write("../escaped.txt", ..)` escapes the root and writes
//! anywhere the process can reach — arbitrary read/write outside the disk.
//!
//! Every test plants a real `secret.txt` OUTSIDE the disk root (one level up)
//! and proves that traversal attempts are rejected AND that the out-of-root
//! file is never read, deleted, or overwritten. Tests use `Storage::fake()`
//! for registry isolation (it serializes fake-using tests process-wide and
//! resets the registry on drop) and a `tempfile::tempdir()` parent that is
//! cleaned up automatically.

use suprnova::Storage;
use suprnova::filesystem::streaming::copy_between_disks;

/// Build a disk rooted at `<tmp>/root` with a planted `<tmp>/secret.txt`
/// sitting one level ABOVE the root, returning the tempdir (kept alive by the
/// caller) and the registered disk name.
fn rooted_disk_with_outside_secret(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).expect("create disk root");
    let secret = tmp.path().join("secret.txt");
    std::fs::write(&secret, b"TOP SECRET").expect("plant out-of-root secret");
    Storage::register_fs(name.to_string(), &root).expect("fs disk init");
    (tmp, secret)
}

#[tokio::test]
async fn write_with_parent_dir_component_is_rejected() {
    let _guard = Storage::fake();
    let (tmp, _secret) = rooted_disk_with_outside_secret("trav_write");
    let disk = Storage::disk("trav_write").unwrap();

    let result = disk
        .write("../escaped.txt", "owned".as_bytes().to_vec())
        .await;
    assert!(result.is_err(), "write('../escaped.txt') must be rejected");

    // Escape-proof: nothing was created outside the root.
    assert!(
        !tmp.path().join("escaped.txt").exists(),
        "rejected write must not create a file outside the disk root"
    );
}

#[tokio::test]
async fn nested_parent_dir_escape_is_rejected() {
    let _guard = Storage::fake();
    let (tmp, _secret) = rooted_disk_with_outside_secret("trav_nested");
    let disk = Storage::disk("trav_nested").unwrap();

    // A `..` buried mid-path still escapes without a guard.
    let result = disk
        .write("a/b/../../../escaped.txt", "owned".as_bytes().to_vec())
        .await;
    assert!(result.is_err(), "buried '..' traversal must be rejected");
    assert!(
        !tmp.path().join("escaped.txt").exists(),
        "rejected nested write must not escape the root"
    );
}

#[tokio::test]
async fn read_parent_dir_traversal_is_rejected() {
    let _guard = Storage::fake();
    let (_tmp, _secret) = rooted_disk_with_outside_secret("trav_read");
    let disk = Storage::disk("trav_read").unwrap();

    // Without a guard this reads the planted out-of-root secret (info leak).
    let result = disk.read("../secret.txt").await;
    assert!(
        result.is_err(),
        "read('../secret.txt') must be rejected, not leak the out-of-root file"
    );
}

#[tokio::test]
async fn stat_parent_dir_traversal_is_rejected() {
    let _guard = Storage::fake();
    let (_tmp, _secret) = rooted_disk_with_outside_secret("trav_stat");
    let disk = Storage::disk("trav_stat").unwrap();

    // Without a guard this stats the out-of-root secret (existence/size leak).
    let result = disk.stat("../secret.txt").await;
    assert!(
        result.is_err(),
        "stat('../secret.txt') must be rejected, not leak out-of-root metadata"
    );
}

#[tokio::test]
async fn delete_parent_dir_traversal_is_rejected_and_leaves_target_intact() {
    let _guard = Storage::fake();
    let (_tmp, secret) = rooted_disk_with_outside_secret("trav_delete");
    let disk = Storage::disk("trav_delete").unwrap();

    // Without a guard this deletes the out-of-root secret.
    let result = disk.delete("../secret.txt").await;
    assert!(result.is_err(), "delete('../secret.txt') must be rejected");
    assert!(
        secret.exists(),
        "rejected delete must leave the out-of-root file intact"
    );
}

#[tokio::test]
async fn copy_rejects_traversal_in_destination() {
    let _guard = Storage::fake();
    let (tmp, _secret) = rooted_disk_with_outside_secret("trav_copy_dst");
    let disk = Storage::disk("trav_copy_dst").unwrap();
    disk.write("inside.txt", "data".as_bytes().to_vec())
        .await
        .unwrap();

    let result = disk.copy("inside.txt", "../escaped.txt").await;
    assert!(result.is_err(), "copy to '../escaped.txt' must be rejected");
    assert!(
        !tmp.path().join("escaped.txt").exists(),
        "rejected copy must not write outside the root"
    );
}

#[tokio::test]
async fn copy_rejects_traversal_in_source() {
    let _guard = Storage::fake();
    let (_tmp, _secret) = rooted_disk_with_outside_secret("trav_copy_src");
    let disk = Storage::disk("trav_copy_src").unwrap();

    let result = disk.copy("../secret.txt", "inside.txt").await;
    assert!(
        result.is_err(),
        "copy from '../secret.txt' must be rejected, not exfiltrate the secret"
    );
}

#[tokio::test]
async fn rename_rejects_traversal() {
    let _guard = Storage::fake();
    let (tmp, _secret) = rooted_disk_with_outside_secret("trav_rename");
    let disk = Storage::disk("trav_rename").unwrap();
    disk.write("movable.txt", "data".as_bytes().to_vec())
        .await
        .unwrap();

    let result = disk.rename("movable.txt", "../escaped.txt").await;
    assert!(
        result.is_err(),
        "rename to '../escaped.txt' must be rejected"
    );
    assert!(
        !tmp.path().join("escaped.txt").exists(),
        "rejected rename must not move a file outside the root"
    );
    assert!(
        disk.read("movable.txt").await.is_ok(),
        "rejected rename must leave the original in place"
    );
}

#[tokio::test]
async fn create_dir_rejects_traversal() {
    let _guard = Storage::fake();
    let (tmp, _secret) = rooted_disk_with_outside_secret("trav_mkdir");
    let disk = Storage::disk("trav_mkdir").unwrap();

    let result = disk.create_dir("../escaped_dir/").await;
    assert!(
        result.is_err(),
        "create_dir('../escaped_dir/') must be rejected"
    );
    assert!(
        !tmp.path().join("escaped_dir").exists(),
        "rejected create_dir must not create a directory outside the root"
    );
}

#[tokio::test]
async fn list_rejects_traversal() {
    let _guard = Storage::fake();
    let (_tmp, _secret) = rooted_disk_with_outside_secret("trav_list");
    let disk = Storage::disk("trav_list").unwrap();

    // Without a guard this lists the parent directory (enumeration leak).
    let result = disk.list("../").await;
    assert!(
        result.is_err(),
        "list('../') must be rejected, not enumerate outside the root"
    );
}

// ---------------------------------------------------------------------------
// The guard must NOT over-reject legitimate paths.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn legitimate_nested_path_is_allowed() {
    let _guard = Storage::fake();
    let (_tmp, _secret) = rooted_disk_with_outside_secret("ok_nested");
    let disk = Storage::disk("ok_nested").unwrap();

    disk.write("a/b/c.txt", "deep".as_bytes().to_vec())
        .await
        .expect("legitimate nested write must succeed");
    let bytes = disk.read("a/b/c.txt").await.expect("read back");
    assert_eq!(&bytes.to_vec(), b"deep");
}

#[tokio::test]
async fn filename_containing_dotdot_substring_is_allowed() {
    let _guard = Storage::fake();
    let (_tmp, _secret) = rooted_disk_with_outside_secret("ok_dotdot");
    let disk = Storage::disk("ok_dotdot").unwrap();

    // `..` as a SUBSTRING of a filename is not a traversal — the guard checks
    // whole path components, so this legitimate name must be allowed.
    disk.write("my..file.txt", "fine".as_bytes().to_vec())
        .await
        .expect("filename with '..' substring must be allowed");
    let bytes = disk.read("my..file.txt").await.expect("read back");
    assert_eq!(&bytes.to_vec(), b"fine");
}

// ---------------------------------------------------------------------------
// The framework's own cross-disk helper routes through the guarded operator.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn copy_between_disks_helper_rejects_traversal_destination() {
    let _guard = Storage::fake();
    let (tmp, _secret) = rooted_disk_with_outside_secret("trav_helper");
    let disk = Storage::disk("trav_helper").unwrap();
    disk.write("inside.txt", "data".as_bytes().to_vec())
        .await
        .unwrap();

    // `copy_between_disks` opens a writer on the destination path — the guard
    // on the FS disk must reject the traversal without any explicit check in
    // the helper itself.
    let result =
        copy_between_disks("trav_helper", "inside.txt", "trav_helper", "../escaped.txt").await;
    assert!(
        result.is_err(),
        "copy_between_disks must inherit the FS disk's traversal guard"
    );
    assert!(
        !tmp.path().join("escaped.txt").exists(),
        "rejected helper copy must not write outside the root"
    );
}

#[tokio::test]
async fn copy_between_disks_helper_rejects_traversal_source() {
    let _guard = Storage::fake();
    let (_tmp, _secret) = rooted_disk_with_outside_secret("trav_helper_src");
    let _disk = Storage::disk("trav_helper_src").unwrap();

    let result = copy_between_disks(
        "trav_helper_src",
        "../secret.txt",
        "trav_helper_src",
        "inside.txt",
    )
    .await;
    assert!(
        result.is_err(),
        "copy_between_disks must reject a traversal source path"
    );
}
