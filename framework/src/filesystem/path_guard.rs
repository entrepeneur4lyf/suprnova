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
//! # Symlink confinement
//!
//! The lexical check ([`validate_storage_path`]) is the first, cheap gate, but
//! it only confines `..`/absolute path *components*. A symlink planted inside
//! the root that points outside it survives the lexical check yet escapes the
//! root once the kernel follows it — a real second-stage traversal vector (an
//! uploaded/extracted symlink, then a read/write through it). After the lexical
//! gate, [`validate_resolved_path`] canonicalizes the on-disk target (resolving
//! every symlink) and re-checks that the canonical path is still inside the
//! canonicalized disk root. For paths that do not exist yet (new writes), the
//! parent directory is canonicalized instead, so the destination directory
//! cannot itself be a symlink leading out of the root. Anything that resolves
//! outside the root is rejected.

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

/// Build the `PermissionDenied` error returned when a path resolves outside the
/// disk root via a symlink (or any other on-disk indirection).
fn symlink_escape_error(path: &str) -> Error {
    tracing::warn!(
        path = %path,
        "rejected storage path that resolves outside the disk root (symlink escape) \
         on local-filesystem disk"
    );
    Error::new(
        ErrorKind::PermissionDenied,
        format!(
            "path '{path}' is not allowed on a local-filesystem disk: \
             it resolves (via a symlink) outside the disk root"
        ),
    )
}

/// Second-stage guard: after the lexical [`validate_storage_path`] check passes,
/// resolve the on-disk target and confirm it is still inside the disk root.
///
/// `root` is the local-filesystem disk root as reported by the inner accessor
/// ([`opendal::raw::AccessorInfo::root`]); the FS backend already canonicalized
/// it at build time, so it is an absolute, symlink-free directory. `path` is the
/// normalized, leading-`/`-stripped storage path that reached the accessor.
///
/// The full on-disk path is `root + path`. If it exists, it is canonicalized
/// (which resolves every symlink component) and must lie under the canonical
/// root. If it does not exist yet — the common case for a new write — we walk
/// the target's ancestors up to the *nearest ancestor that actually exists* and
/// canonicalize that one, so a symlinked ancestor directory is still rejected
/// even before the leaf (and any intermediate dirs) are created. Components that
/// exist nowhere on disk are the only ones safe to create under the root; an
/// existing ancestor that resolves (or traverses a symlink) outside the root is
/// an escape and is rejected.
///
/// Canonicalization uses `tokio::fs` so it never blocks the async executor,
/// matching the FS backend's own `tokio::fs`-based IO.
async fn validate_resolved_path(root: &str, path: &str) -> Result<()> {
    validate_storage_path(path)?;

    // The post-normalize root indicator is the disk root itself — already inside.
    if path == "/" || path.is_empty() {
        return Ok(());
    }

    let canonical_root = tokio::fs::canonicalize(root).await.map_err(|e| {
        Error::new(
            ErrorKind::Unexpected,
            "canonicalize of local-filesystem disk root failed",
        )
        .set_source(e)
    })?;

    // A trailing `/` (directory marker) doesn't change which on-disk node the
    // path refers to; strip it so `Path` joins cleanly.
    let relative = path.trim_end_matches('/');
    let target = Path::new(root).join(relative);

    // Walk from the leaf upward to the nearest ancestor that exists on disk and
    // canonicalize *that*. `target.ancestors()` yields the target first, then
    // each successive parent, so the first one that resolves is either the leaf
    // itself (existing target) or the deepest existing directory above it (new
    // write). Confining only the *immediate* parent — as an earlier version did —
    // let an intermediate symlink escape: if `root/evil -> /outside`, then
    // writing `evil/newdir/payload` has a missing leaf AND a missing immediate
    // parent (`evil/newdir`), so the old early-return treated it as safe while
    // the FS backend would follow `evil` and write to `/outside/newdir/payload`.
    // Resolving the nearest *existing* ancestor (`root/evil`, the symlink) and
    // requiring it to be within the root catches that escape. Components that
    // exist nowhere on disk — `newdir`, `payload` — are the only ones genuinely
    // safe to create under the root, since the kernel can only follow links that
    // already exist.
    let mut resolved: Option<std::path::PathBuf> = None;
    for ancestor in target.ancestors() {
        match tokio::fs::canonicalize(ancestor).await {
            Ok(resolved_ancestor) => {
                resolved = Some(resolved_ancestor);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(Error::new(
                    ErrorKind::Unexpected,
                    "canonicalize of storage path failed",
                )
                .set_source(e));
            }
        }
    }

    // No ancestor resolved at all. This only happens if even the disk root is
    // gone, but `canonical_root` above already required it to exist, so fall back
    // to the canonical root for the prefix check.
    let resolved = resolved.unwrap_or_else(|| canonical_root.clone());

    if is_within_root(&canonical_root, &resolved) {
        Ok(())
    } else {
        Err(symlink_escape_error(path))
    }
}

/// True when `resolved` is the canonical root itself or a descendant of it. Both
/// arguments must already be canonical (absolute, symlink-free) so the
/// component-wise prefix check is sound: [`Path::starts_with`] matches whole
/// path components and returns true for equality, so it is not fooled by
/// `/rootevil` vs `/root` the way a lexical string `starts_with` would be.
fn is_within_root(canonical_root: &Path, resolved: &Path) -> bool {
    resolved.starts_with(canonical_root)
}

/// Fetch the inner FS accessor's canonical root once per guarded operation.
/// The FS backend reports an absolute, already-canonicalized root via
/// [`opendal::raw::AccessorInfo::root`].
fn inner_root_string<A: Access>(inner: &A) -> String {
    inner.info().root().as_ref().to_string()
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
        let root = inner_root_string(&self.inner);
        validate_resolved_path(&root, path).await?;
        self.inner.read(path, args).await
    }

    async fn write(&self, path: &str, args: OpWrite) -> Result<(RpWrite, Self::Writer)> {
        let root = inner_root_string(&self.inner);
        validate_resolved_path(&root, path).await?;
        self.inner.write(path, args).await
    }

    async fn list(&self, path: &str, args: OpList) -> Result<(RpList, Self::Lister)> {
        let root = inner_root_string(&self.inner);
        validate_resolved_path(&root, path).await?;
        self.inner.list(path, args).await
    }

    // `delete` carries no path at the accessor level — the path is fed to the
    // returned deleter's `delete(path)`. Wrap the deleter so it is guarded too,
    // handing it the disk root so it can run the same resolved-path check.
    async fn delete(&self) -> Result<(RpDelete, Self::Deleter)> {
        let root = inner_root_string(&self.inner);
        let (rp, inner) = self.inner.delete().await?;
        Ok((rp, PathGuardDeleter { inner, root }))
    }

    async fn create_dir(&self, path: &str, args: OpCreateDir) -> Result<RpCreateDir> {
        let root = inner_root_string(&self.inner);
        validate_resolved_path(&root, path).await?;
        self.inner.create_dir(path, args).await
    }

    async fn stat(&self, path: &str, args: OpStat) -> Result<RpStat> {
        let root = inner_root_string(&self.inner);
        validate_resolved_path(&root, path).await?;
        self.inner.stat(path, args).await
    }

    async fn copy(&self, from: &str, to: &str, args: OpCopy) -> Result<RpCopy> {
        let root = inner_root_string(&self.inner);
        validate_resolved_path(&root, from).await?;
        validate_resolved_path(&root, to).await?;
        self.inner.copy(from, to, args).await
    }

    async fn rename(&self, from: &str, to: &str, args: OpRename) -> Result<RpRename> {
        let root = inner_root_string(&self.inner);
        validate_resolved_path(&root, from).await?;
        validate_resolved_path(&root, to).await?;
        self.inner.rename(from, to, args).await
    }

    async fn presign(&self, path: &str, args: OpPresign) -> Result<RpPresign> {
        let root = inner_root_string(&self.inner);
        validate_resolved_path(&root, path).await?;
        self.inner.presign(path, args).await
    }
}

/// The deleter produced by [`PathGuardAccessor`]. `delete(path)` is where the
/// deletion path arrives, so it is validated here before forwarding. It carries
/// the disk root captured at `delete()` time so it can run the same
/// resolved-path (symlink) check as the other operations.
pub(crate) struct PathGuardDeleter<D> {
    inner: D,
    root: String,
}

impl<D: oio::Delete> oio::Delete for PathGuardDeleter<D> {
    async fn delete(&mut self, path: &str, args: OpDelete) -> Result<()> {
        validate_resolved_path(&self.root, path).await?;
        self.inner.delete(path, args).await
    }

    async fn close(&mut self) -> Result<()> {
        self.inner.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_resolved_path, validate_storage_path};

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

    // ------------------------------------------------------------------
    // Symlink confinement (`validate_resolved_path`). The lexical gate is
    // clean for every path below — these escapes only manifest once the
    // on-disk symlink is followed, which is exactly what the resolved check
    // catches.
    // ------------------------------------------------------------------

    /// Canonical disk root for the test, mirroring the FS backend (which
    /// canonicalizes its root at build time). On macOS `/tmp` is itself a
    /// symlink, so the root must be canonicalized for the prefix check to hold.
    fn canonical_root(dir: &std::path::Path) -> String {
        std::fs::canonicalize(dir)
            .expect("canonicalize test root")
            .to_string_lossy()
            .into_owned()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_pointing_outside_root_is_rejected_for_existing_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).expect("create root");
        // A real directory OUTSIDE the root, with a secret file in it.
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).expect("create outside");
        std::fs::write(outside.join("secret.txt"), b"TOP SECRET").expect("plant secret");
        // A symlink INSIDE the root that points at the outside directory.
        std::os::unix::fs::symlink(&outside, root.join("escape")).expect("create escaping symlink");

        let root_str = canonical_root(&root);
        // Reading/stat-ing through the symlink resolves to the outside file and
        // must be rejected even though "escape/secret.txt" is lexically clean.
        assert!(
            validate_resolved_path(&root_str, "escape/secret.txt")
                .await
                .is_err(),
            "read through a symlink that escapes the root must be rejected"
        );
        // Writing a NEW file through the escaping symlink: the leaf doesn't
        // exist, so the parent ("escape" -> outside) is canonicalized and must
        // be rejected.
        assert!(
            validate_resolved_path(&root_str, "escape/newfile.txt")
                .await
                .is_err(),
            "write through a symlinked directory that escapes the root must be rejected"
        );
        // The symlink target itself, resolved, is outside the root.
        assert!(
            validate_resolved_path(&root_str, "escape").await.is_err(),
            "operating on the escaping symlink node (resolved) must be rejected"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_ancestor_escape_with_missing_immediate_parent_is_rejected() {
        // The escape the nearest-existing-ancestor walk closes: the symlinked
        // ancestor is NOT the immediate parent of the write target — both the
        // leaf and its immediate parent don't exist, so an
        // immediate-parent-only check would canonicalize NotFound and wave the
        // write through, letting the FS backend follow the symlink out of root.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).expect("create root");
        // A real directory OUTSIDE the root.
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).expect("create outside");
        // A symlink INSIDE the root that points at the outside directory.
        std::os::unix::fs::symlink(&outside, root.join("evil")).expect("create escaping symlink");

        let root_str = canonical_root(&root);
        // Writing `evil/newdir/payload`: leaf missing AND immediate parent
        // (`evil/newdir`) missing, but `evil` -> outside exists. The walk must
        // resolve `evil` (the nearest existing ancestor), see it escapes, and
        // reject. `outside/newdir/payload` would otherwise be created off-root.
        assert!(
            validate_resolved_path(&root_str, "evil/newdir/payload")
                .await
                .is_err(),
            "write whose nearest existing ancestor is an escaping symlink must be rejected"
        );
        // A deeper missing chain through the same symlink is rejected too.
        assert!(
            validate_resolved_path(&root_str, "evil/a/b/c/payload")
                .await
                .is_err(),
            "an even deeper missing chain through the escaping symlink must be rejected"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_pointing_inside_root_is_allowed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("root");
        let real_dir = root.join("real");
        std::fs::create_dir_all(&real_dir).expect("create nested dir");
        std::fs::write(real_dir.join("data.txt"), b"inside").expect("write data");
        // A symlink inside the root that points at another location inside the
        // root — legitimate, must be allowed.
        std::os::unix::fs::symlink(&real_dir, root.join("link")).expect("create inside symlink");

        let root_str = canonical_root(&root);
        assert!(
            validate_resolved_path(&root_str, "link/data.txt")
                .await
                .is_ok(),
            "a symlink that stays inside the root must be allowed"
        );
    }

    #[tokio::test]
    async fn legitimate_nested_path_passes_resolved_check() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("root");
        std::fs::create_dir_all(root.join("a/b")).expect("create nested dirs");
        std::fs::write(root.join("a/b/c.txt"), b"deep").expect("write file");

        let root_str = canonical_root(&root);
        // Existing nested file: canonicalizes to a descendant of the root.
        assert!(validate_resolved_path(&root_str, "a/b/c.txt").await.is_ok());
        // New file under an existing nested dir: parent canonicalizes inside.
        assert!(
            validate_resolved_path(&root_str, "a/b/new.txt")
                .await
                .is_ok()
        );
        // New file under a NOT-yet-existing nested dir: parent missing, which
        // the backend will create under the root — allowed.
        assert!(validate_resolved_path(&root_str, "x/y/z.txt").await.is_ok());
        // The root indicator itself.
        assert!(validate_resolved_path(&root_str, "/").await.is_ok());
        assert!(validate_resolved_path(&root_str, "").await.is_ok());
    }

    #[tokio::test]
    async fn lexical_escape_is_still_rejected_before_resolution() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).expect("create root");

        let root_str = canonical_root(&root);
        // The cheap lexical gate fires first, before any filesystem touch.
        assert!(
            validate_resolved_path(&root_str, "../escaped.txt")
                .await
                .is_err()
        );
        assert!(
            validate_resolved_path(&root_str, "a/../../b")
                .await
                .is_err()
        );
        assert!(
            validate_resolved_path(&root_str, "/etc/passwd")
                .await
                .is_err()
        );
    }
}
