# Filesystem & Storage

Suprnova's storage facade gives you a single, named-disk API over local
filesystems, in-memory backends, and the major object stores (S3, Azure Blob,
Google Cloud Storage). Under the hood it is built on
[`opendal`](https://docs.rs/opendal) — but the consumer surface is shaped to
match Laravel's `Storage::disk(...)` calls, so PHP muscle memory translates
straight across.

```rust,no_run
use suprnova::{DiskExt, Storage};

# async fn doc() -> Result<(), suprnova::FrameworkError> {
Storage::register_fs("local", "./storage")?;
let disk = Storage::disk("local")?;

disk.put("notes/hello.txt", b"hello world".to_vec()).await?;
let bytes = disk.get("notes/hello.txt").await?;
assert_eq!(bytes, b"hello world");
# Ok(())
# }
```

## Registering disks

Every disk is registered once at boot via `Storage::register_*` and looked up
by name through `Storage::disk(name)`. There is no "default backend" the
others degrade into — each driver is a peer.

| Constructor                          | Backend                       |
|--------------------------------------|-------------------------------|
| `Storage::register_fs(name, root)`   | Local filesystem              |
| `Storage::register_memory(name)`     | In-process memory (tests)     |
| `Storage::register_s3(name, cfg)`    | Amazon S3 or S3-compatible    |
| `Storage::register_azblob(name, cfg)`| Azure Blob Storage            |
| `Storage::register_gcs(name, cfg)`   | Google Cloud Storage          |

Every constructor has a `_with` variant that hands you the `opendal::Operator`
just before it lands in the registry so you can install retry/timeout/logging
layers around it:

```rust,ignore
use opendal::layers::{LoggingLayer, RetryLayer, TimeoutLayer};
use std::time::Duration;
use suprnova::Storage;

Storage::register_fs_with("local", "./storage", |op| {
    op.layer(RetryLayer::new().with_max_times(3))
      .layer(TimeoutLayer::new().with_timeout(Duration::from_secs(30)))
      .layer(LoggingLayer::default())
})?;
```

The cloud constructors (`register_s3`, `register_azblob`, `register_gcs`)
apply a `RetryLayer` (3 attempts) by default since transient throttling /
5xx errors are routine on object stores. Use the `_with` variants when you
need full control.

### Path-traversal guard

Local filesystem disks have a `PathGuardLayer` applied before any user-supplied
layers. A request like `disk.write("../escaped.txt", ..)` is rejected before
it reaches the OS — no `..` component or absolute prefix can escape the disk
root. Object stores and the in-memory backend do not get the guard (a key
like `../foo` is just an ordinary key character on those backends).

## The Laravel-shape disk surface

`Storage::disk(name)` returns an `opendal::Operator` directly so you can use
its full streaming surface (`writer`, `reader`, `presign_read`, `list`,
`stat`, ...). On top of that, the [`DiskExt`] trait — blanket-implemented on
`Operator` and re-exported as `suprnova::DiskExt` — adds every Laravel
convenience method you'd reach for through `Storage::disk('local')->...`.

Bring it into scope with `use suprnova::DiskExt;`.

### Existence checks

```rust,ignore
disk.exists("a.txt").await?;        // raw opendal
disk.missing("a.txt").await?;       // negation
disk.file_exists("a.txt").await?;   // file only (not a directory)
disk.file_missing("a.txt").await?;
disk.directory_exists("dir/").await?;
disk.directory_missing("dir/").await?;
```

### Reading and writing

| Laravel name | Rust-native equivalent | Note |
|--------------|------------------------|------|
| `get(path)`  | `read(path)`           | `get` returns `Vec<u8>`; `read` returns opendal's `Buffer`. |
| `put(path, contents)` | `write(path, contents)` | Both accept any `Into<Bytes>`. |
| `json::<T>(path)` | — | Reads + deserializes via serde_json. |
| `put_json(path, &value)` | — | Pretty-prints via serde_json. |
| `prepend(path, data)` | — | Joins with `\n`. Use `prepend_with_separator` for a custom join. |
| `append(path, data)`  | — | Joins with `\n`. Use `append_with_separator` for a custom join. |

`prepend` and `append` create the file if it does not yet exist, so they are
safe as the first write to a log file.

### Metadata

```rust,ignore
let bytes  = disk.size("a.bin").await?;          // u64
let when   = disk.last_modified("a.bin").await?; // Option<DateTime<Utc>>
let mime   = disk.mime_type("a.bin").await?;     // Option<String>
let digest = disk.checksum("a.bin", ChecksumAlgorithm::Sha256).await?;
```

`mime_type` first asks the backend — S3, Azure, and GCS pass the stored
`Content-Type` through. If the backend does not have one, it sniffs the first
16 KiB via the `infer` crate. `Ok(None)` is reserved for unrecognised binary
blobs.

`checksum` supports `Md5`, `Sha1`, and `Sha256` via [`ChecksumAlgorithm`].
MD5 and SHA-1 are included for parity with Laravel and object-store ETags;
choose SHA-256 for any new integrity check.

### Listing

```rust,ignore
let files = disk.files("docs", false).await?;     // top-level files
let all   = disk.all_files("docs").await?;        // recursive
let dirs  = disk.directories("docs", false).await?;
let all   = disk.all_directories("docs").await?;
```

All four return sorted `Vec<String>` so callers can rely on stable ordering
across backends. Directories are filtered out of `files`, and vice versa.
Directory paths are returned **without** a trailing slash (`"docs/sub"`) to
match Laravel's `Storage::directories()` output — opendal's underlying
`list` reports `"docs/sub/"` but we strip the slash for parity.

### Mutating directories and files

| Laravel name           | opendal native        |
|------------------------|-----------------------|
| `make_directory(path)` | `create_dir(path)`    |
| `delete_directory(p)`  | `delete_with(p).recursive(true)` |
| `move_to(from, to)`    | `rename(from, to)`    |

`move_to` falls back to `copy + delete` if the backend doesn't support
rename, and to `read + write + delete` if it doesn't support copy either —
so it works against the in-memory driver used in tests as well as against
production backends.

### Pre-signed URLs

```rust,ignore
let read_url   = disk.temporary_url("uploads/a.pdf", Duration::from_secs(900)).await?;
let upload_url = disk.temporary_upload_url("uploads/new.pdf", Duration::from_secs(900)).await?;
```

`temporary_url` and `temporary_upload_url` return the URL as a `String` for
Laravel parity. They are backed by `Operator::presign_read` /
`presign_write`, so they error with an `Unsupported` message on backends
that do not implement presigning (the in-memory and local-filesystem
drivers fall in this bucket; S3, Azure Blob, and GCS support it).

## Cross-disk streaming copy

`copy_between_disks(src, src_path, dest, dest_path)` streams the source
object into the destination in 64 KiB chunks, regardless of the backend
pair. Source and destination can be backed by *any* opendal driver — local
filesystem to S3, S3 to Azure Blob, in-memory to GCS, and so on.

```rust,ignore
use suprnova::filesystem::streaming::copy_between_disks;

Storage::register_fs("local", "./storage")?;
Storage::register_memory("scratch");
let bytes = copy_between_disks("local", "uploads/big.bin", "scratch", "big.bin").await?;
```

If any step fails mid-copy, the partial destination object is aborted and
deleted before the original error propagates — a failed copy is never
observable as a truncated destination.

## Registry hygiene

```rust,ignore
let removed = Storage::forget("local");  // bool: was it present?
Storage::purge();                        // drop every disk
let names = Storage::disks();            // Vec<String>, sorted
```

These mirror Laravel's `FilesystemManager::forgetDisk` / `purge` and are
useful for configuration reloads and admin dashboards. They are not
test-only: production code occasionally needs to drop and re-register a
disk at runtime (e.g. after a secrets rotation).

## Testing

`Storage::fake()` returns a guard that:

1. Acquires a process-global mutex so concurrent `#[tokio::test]` cases do
   not race on the shared registry, and
2. Resets the registry on construction and on drop, leaving the suite in a
   clean state for whichever test runs next.

A `"default"` memory disk is pre-registered for convenience.

```rust,ignore
use suprnova::filesystem::testing::DiskAssertExt;
use suprnova::{DiskExt, Storage};

#[tokio::test]
async fn stores_and_asserts() {
    let _guard = Storage::fake();
    Storage::register_memory("uploads");
    let disk = Storage::disk("uploads").unwrap();

    disk.put("a.txt", b"hello".to_vec()).await.unwrap();

    disk.assert_exists("a.txt").await;
    disk.assert_contents("a.txt", b"hello").await;
    disk.assert_missing("not-here.txt").await;
    disk.assert_count("", 1, false).await;
    disk.assert_directory_empty("docs/").await;
}
```

The four assertion helpers — `assert_exists`, `assert_contents`,
`assert_missing`, `assert_count`, `assert_directory_empty` — are exposed via
the [`DiskAssertExt`] trait, gated on `#[cfg(any(test, feature = "testing"))]`
so production code cannot reach for them.

## Parity quick reference

| Laravel `Storage::disk(...)->...`     | Suprnova                                                 |
|---------------------------------------|----------------------------------------------------------|
| `exists($path)`                       | `disk.exists(path)`                                      |
| `missing($path)`                      | `disk.missing(path)`                                     |
| `fileExists($path)` / `fileMissing`   | `disk.file_exists(path)` / `file_missing(path)`          |
| `directoryExists($p)` / `directoryMissing` | `disk.directory_exists(p)` / `directory_missing(p)` |
| `get($path)`                          | `disk.get(path)` (`Vec<u8>`)                             |
| `json($path)`                         | `disk.json::<T>(path)`                                   |
| `put($path, $contents)`               | `disk.put(path, bytes)`                                  |
| `prepend($path, $data)`               | `disk.prepend(path, data)`                               |
| `append($path, $data)`                | `disk.append(path, data)`                                |
| `size($path)`                         | `disk.size(path)`                                        |
| `lastModified($path)`                 | `disk.last_modified(path)`                               |
| `mimeType($path)`                     | `disk.mime_type(path)`                                   |
| `checksum($path, ['checksum_algo' => 'sha256'])` | `disk.checksum(path, ChecksumAlgorithm::Sha256)` |
| `files($dir, $recursive)`             | `disk.files(dir, recursive)`                             |
| `allFiles($dir)`                      | `disk.all_files(dir)`                                    |
| `directories($dir, $recursive)`       | `disk.directories(dir, recursive)`                       |
| `allDirectories($dir)`                | `disk.all_directories(dir)`                              |
| `makeDirectory($path)`                | `disk.make_directory(path)`                              |
| `deleteDirectory($path)`              | `disk.delete_directory(path)`                            |
| `move($from, $to)`                    | `disk.move_to(from, to)` (or opendal-native `rename`)    |
| `copy($from, $to)`                    | `disk.copy(from, to)` (opendal-native)                   |
| `delete($path)`                       | `disk.delete(path)` (opendal-native)                     |
| `temporaryUrl($path, $expiry)`        | `disk.temporary_url(path, expire)` (or opendal-native `presign_read`) |
| `temporaryUploadUrl($path, $expiry)`  | `disk.temporary_upload_url(path, expire)` (or opendal-native `presign_write`) |
| `Storage::fake()`                     | `Storage::fake()`                                        |
| `Storage::disk()->assertExists()`     | `disk.assert_exists(path).await`                         |
| `FilesystemManager::forgetDisk($n)`   | `Storage::forget(name)`                                  |
| `FilesystemManager::purge()`          | `Storage::purge()`                                       |

[`DiskExt`]: https://docs.rs/suprnova/latest/suprnova/trait.DiskExt.html
[`DiskAssertExt`]: https://docs.rs/suprnova/latest/suprnova/filesystem/testing/trait.DiskAssertExt.html
[`ChecksumAlgorithm`]: https://docs.rs/suprnova/latest/suprnova/enum.ChecksumAlgorithm.html
