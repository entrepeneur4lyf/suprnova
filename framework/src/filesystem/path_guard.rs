//! Path-confinement layer for local-filesystem disks.
//!
//! OpenDAL's `Operator` runs `normalize_path` before the accessor — it strips a
//! leading `/` and collapses `//`, but it does NOT resolve `..`. A `..`
//! component therefore reaches the FS backend, which joins it onto the disk
//! root, so `disk.write("../escaped.txt", ..)` escapes the configured root and
//! grants arbitrary read/write/delete outside the disk. This is a custom
//! [`Layer`] that rejects any path which would leave the root.
//!
//! The guard is applied only to local-filesystem disks (`register_fs` /
//! `register_fs_with`). Object-store backends (S3, Azure Blob, GCS) and the
//! in-memory backend confine to a bucket/prefix or have no filesystem at all,
//! where `..` is just an ordinary key character — guarding them would wrongly
//! reject legitimate keys.
//!
//! # Limitation
//!
//! This guard confines `..`/absolute path *components*. It does not chase
//! symlinks: a symlink planted inside the root that points outside it is an
//! operating-system / mount concern (mount the disk root on a dedicated
//! filesystem, use `nosymfollow`, or chroot), not a `..`-traversal concern.

use opendal::raw::{
    Access, Layer, LayeredAccess, OpCopy, OpCreateDir, OpDelete, OpList, OpPresign, OpRead,
    OpRename, OpStat, OpWrite, RpCopy, RpCreateDir, RpDelete, RpList, RpPresign, RpRead, RpRename,
    RpStat, RpWrite, oio,
};
use opendal::{Error, ErrorKind, Result};
use std::path::{Component, Path};

/// Reject any path that could escape the local-filesystem disk root.
///
/// Rejects a path whose components include a parent-directory (`..`) hop or an
/// absolute/root prefix. A `..` appearing only as a *substring* of a single
/// path segment (e.g. `my..file.txt`) is allowed — the check is component-wise.
/// The separator-agnostic split is belt-and-suspenders: `\` is an ordinary
/// character on Unix (where [`Path::components`] would not split on it) but a
/// separator on Windows, so splitting on both keeps the guard correct wherever
/// it runs.
fn validate_storage_path(path: &str) -> Result<()> {
    // opendal's `normalize_path` collapses an empty path and the disk root to
    // the single indicator "/", which is what reaches this layer for a
    // root-level list/stat. That is the disk root itself, not an escape, so it
    // is allowed. Every other path arrives with its leading `/` already
    // stripped (so a `RootDir` component below can only come from a caller that
    // bypassed normalization — kept rejected as defense-in-depth).
    if path == "/" {
        return Ok(());
    }

    let has_parent_segment = path.split(['/', '\\']).any(|segment| segment == "..");
    let has_traversal_component = Path::new(path).components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    });

    if has_parent_segment || has_traversal_component {
        tracing::warn!(
            path = %path,
            "rejected storage path traversal attempt on local-filesystem disk"
        );
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "path '{path}' is not allowed on a local-filesystem disk: \
                 paths must stay within the disk root (no '..' or absolute components)"
            ),
        ));
    }
    Ok(())
}

/// [`Layer`] that wraps a local-filesystem accessor so every path-bearing
/// operation is confined to the disk root. Applied by `Storage::register_fs*`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PathGuardLayer;

impl<A: Access> Layer<A> for PathGuardLayer {
    type LayeredAccess = PathGuardAccessor<A>;

    fn layer(&self, inner: A) -> Self::LayeredAccess {
        PathGuardAccessor { inner }
    }
}

/// The accessor produced by [`PathGuardLayer`]. Validates every path before
/// forwarding to the inner FS accessor.
#[derive(Debug)]
pub(crate) struct PathGuardAccessor<A> {
    inner: A,
}

impl<A: Access> LayeredAccess for PathGuardAccessor<A> {
    type Inner = A;
    type Reader = A::Reader;
    type Writer = A::Writer;
    type Lister = A::Lister;
    type Deleter = PathGuardDeleter<A::Deleter>;

    fn inner(&self) -> &Self::Inner {
        &self.inner
    }

    async fn read(&self, path: &str, args: OpRead) -> Result<(RpRead, Self::Reader)> {
        validate_storage_path(path)?;
        self.inner.read(path, args).await
    }

    async fn write(&self, path: &str, args: OpWrite) -> Result<(RpWrite, Self::Writer)> {
        validate_storage_path(path)?;
        self.inner.write(path, args).await
    }

    async fn list(&self, path: &str, args: OpList) -> Result<(RpList, Self::Lister)> {
        validate_storage_path(path)?;
        self.inner.list(path, args).await
    }

    // `delete` carries no path at the accessor level — the path is fed to the
    // returned deleter's `delete(path)`. Wrap the deleter so it is guarded too.
    async fn delete(&self) -> Result<(RpDelete, Self::Deleter)> {
        let (rp, inner) = self.inner.delete().await?;
        Ok((rp, PathGuardDeleter { inner }))
    }

    async fn create_dir(&self, path: &str, args: OpCreateDir) -> Result<RpCreateDir> {
        validate_storage_path(path)?;
        self.inner.create_dir(path, args).await
    }

    async fn stat(&self, path: &str, args: OpStat) -> Result<RpStat> {
        validate_storage_path(path)?;
        self.inner.stat(path, args).await
    }

    async fn copy(&self, from: &str, to: &str, args: OpCopy) -> Result<RpCopy> {
        validate_storage_path(from)?;
        validate_storage_path(to)?;
        self.inner.copy(from, to, args).await
    }

    async fn rename(&self, from: &str, to: &str, args: OpRename) -> Result<RpRename> {
        validate_storage_path(from)?;
        validate_storage_path(to)?;
        self.inner.rename(from, to, args).await
    }

    async fn presign(&self, path: &str, args: OpPresign) -> Result<RpPresign> {
        validate_storage_path(path)?;
        self.inner.presign(path, args).await
    }
}

/// The deleter produced by [`PathGuardAccessor`]. `delete(path)` is where the
/// deletion path arrives, so it is validated here before forwarding.
pub(crate) struct PathGuardDeleter<D> {
    inner: D,
}

impl<D: oio::Delete> oio::Delete for PathGuardDeleter<D> {
    async fn delete(&mut self, path: &str, args: OpDelete) -> Result<()> {
        validate_storage_path(path)?;
        self.inner.delete(path, args).await
    }

    async fn close(&mut self) -> Result<()> {
        self.inner.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::validate_storage_path;

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        for bad in [
            "..",
            "../x",
            "../../x",
            "a/../../b",
            "a/b/../../../c",
            "./../x",
            "foo/..",
            "/abs/x",
            "/etc/passwd",
            "..\\windows",
            "a\\..\\b",
        ] {
            assert!(
                validate_storage_path(bad).is_err(),
                "must reject traversal path {bad:?}"
            );
        }
    }

    #[test]
    fn allows_legitimate_paths() {
        for ok in [
            "a.txt",
            "a/b/c.txt",
            "my..file.txt",
            "deeply/nested/ok.bin",
            "./relative.txt",
            "name..with..dots.txt",
            "",
            "dir/",
            // The post-normalize root indicator (root list/stat) — allowed.
            "/",
        ] {
            assert!(
                validate_storage_path(ok).is_ok(),
                "must allow legitimate path {ok:?}"
            );
        }
    }
}
