//! `copy_between_disks` must not leave a partial destination object when the
//! transfer fails mid-stream.
//!
//! The source disk here is a memory backend wrapped in a layer whose reader
//! yields exactly one chunk and then errors — simulating a source that fails
//! after the destination writer has already received data. The destination is
//! a real filesystem disk, so a partial write is visible on disk: the test
//! proves the file is gone after the failed copy.

use opendal::raw::{
    Access, Layer, LayeredAccess, OpList, OpRead, OpWrite, RpDelete, RpList, RpRead, RpWrite, oio,
};
use opendal::{Buffer, Error, ErrorKind, Result};
use suprnova::Storage;
use suprnova::filesystem::streaming::copy_between_disks;

/// Layer whose reader returns one 1 KiB chunk and then fails on the next read.
#[derive(Debug, Clone, Copy)]
struct FailAfterOneChunkLayer;

impl<A: Access> Layer<A> for FailAfterOneChunkLayer {
    type LayeredAccess = FailAccessor<A>;

    fn layer(&self, inner: A) -> Self::LayeredAccess {
        FailAccessor { inner }
    }
}

#[derive(Debug)]
struct FailAccessor<A> {
    inner: A,
}

impl<A: Access> LayeredAccess for FailAccessor<A> {
    type Inner = A;
    type Reader = FailReader;
    type Writer = A::Writer;
    type Lister = A::Lister;
    type Deleter = A::Deleter;

    fn inner(&self) -> &Self::Inner {
        &self.inner
    }

    async fn read(&self, _path: &str, _args: OpRead) -> Result<(RpRead, Self::Reader)> {
        Ok((RpRead::new(), FailReader { sent_chunk: false }))
    }

    async fn write(&self, path: &str, args: OpWrite) -> Result<(RpWrite, Self::Writer)> {
        self.inner.write(path, args).await
    }

    async fn list(&self, path: &str, args: OpList) -> Result<(RpList, Self::Lister)> {
        self.inner.list(path, args).await
    }

    async fn delete(&self) -> Result<(RpDelete, Self::Deleter)> {
        self.inner.delete().await
    }
}

struct FailReader {
    sent_chunk: bool,
}

impl oio::Read for FailReader {
    async fn read(&mut self) -> Result<Buffer> {
        if self.sent_chunk {
            Err(Error::new(
                ErrorKind::Unexpected,
                "injected mid-stream read failure",
            ))
        } else {
            self.sent_chunk = true;
            Ok(Buffer::from(vec![0u8; 1024]))
        }
    }
}

#[tokio::test]
async fn copy_cleans_up_partial_destination_on_midstream_failure() {
    let _guard = Storage::fake();
    let tmp = tempfile::tempdir().expect("create tempdir");

    // Source: memory disk whose reader fails after one chunk has been read
    // (and therefore after that chunk has been written to the destination).
    Storage::register_memory_with("atomic_fail_src", |op| op.layer(FailAfterOneChunkLayer));
    // Destination: a real filesystem disk so the partial write is observable.
    Storage::register_fs("atomic_fs_dest", tmp.path()).expect("fs dest");

    let result = copy_between_disks(
        "atomic_fail_src",
        "anything",
        "atomic_fs_dest",
        "partial.bin",
    )
    .await;
    assert!(
        result.is_err(),
        "a mid-stream source failure must surface as an error"
    );

    // The one chunk that was written before the failure must have been cleaned
    // up — a failed copy must never be observable as a partial/truncated file.
    assert!(
        !tmp.path().join("partial.bin").exists(),
        "a failed copy must not leave a partial destination file"
    );
}
