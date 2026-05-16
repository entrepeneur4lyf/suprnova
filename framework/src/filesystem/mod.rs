//! Storage facade backed by [`opendal`].
//!
//! Disks are registered once at boot via `Storage::register_*` and looked up
//! by name through [`Storage::disk`]. The lookup returns the underlying
//! [`opendal::Operator`] directly, so consumers get the full streaming surface
//! ([`Operator::writer`], [`Operator::reader`], [`Operator::presign_read`],
//! [`Operator::list`], [`Operator::stat`], …) without us proxying each method.
//!
//! Drivers are first-class peers — there is no "default backend" the others
//! degrade into. `register_fs`, `register_memory`, `register_s3`,
//! `register_azblob`, and `register_gcs` each translate an explicit config
//! struct into the matching `opendal::services::*` builder.
//!
//! # Example
//!
//! ```rust,no_run
//! use suprnova::Storage;
//!
//! # async fn doc() -> Result<(), suprnova::FrameworkError> {
//! Storage::register_fs("local", "./storage")?;
//! let disk = Storage::disk("local")?;
//! disk.write("notes/hello.txt", "hello world").await?;
//! let bytes = disk.read("notes/hello.txt").await?;
//! assert_eq!(&bytes.to_vec(), b"hello world");
//! # Ok(())
//! # }
//! ```

mod registry;
pub mod streaming;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use streaming::copy_between_disks;

use crate::FrameworkError;
use opendal::{services, Operator};
use std::path::Path;

/// Static facade for the named-disk storage system.
///
/// `Storage` itself holds no state; all disks live in a process-global
/// registry populated by the `register_*` constructors. Look one up with
/// [`Storage::disk`] and operate on it through the returned [`Operator`].
pub struct Storage;

/// Configuration for the S3 driver.
///
/// Mirrors `opendal::services::S3` — credentials and region are optional so
/// the underlying SDK can fall back to its credential providers (environment,
/// IMDS, profile chain) when omitted.
#[derive(Debug, Clone, Default)]
pub struct S3Config {
    /// Bucket name. Required.
    pub bucket: String,
    /// AWS region (e.g. `"us-east-1"`).
    pub region: Option<String>,
    /// Custom endpoint, for S3-compatible services (MinIO, R2, etc.).
    pub endpoint: Option<String>,
    /// Static access key id. Leave `None` to use the default provider chain.
    pub access_key_id: Option<String>,
    /// Static secret access key. Leave `None` to use the default provider chain.
    pub secret_access_key: Option<String>,
    /// Root prefix within the bucket. All operations are relative to this prefix.
    pub root: Option<String>,
}

/// Configuration for the Azure Blob Storage driver.
#[derive(Debug, Clone, Default)]
pub struct AzBlobConfig {
    /// Container name. Required.
    pub container: String,
    /// Storage account name.
    pub account_name: String,
    /// Storage account key.
    pub account_key: String,
    /// Custom endpoint, e.g. for Azurite emulator.
    pub endpoint: Option<String>,
    /// Root prefix within the container.
    pub root: Option<String>,
}

/// Configuration for the Google Cloud Storage driver.
#[derive(Debug, Clone, Default)]
pub struct GcsConfig {
    /// Bucket name. Required.
    pub bucket: String,
    /// Inline JSON credential blob. Leave `None` to use ADC / metadata server.
    pub credential: Option<String>,
    /// Path to a service-account JSON file on disk.
    pub credential_path: Option<String>,
    /// Custom endpoint (rare, mainly for fakegcs / testing).
    pub endpoint: Option<String>,
    /// Root prefix within the bucket.
    pub root: Option<String>,
}

impl Storage {
    /// Look up a registered disk by name and return its [`Operator`].
    ///
    /// Returns `Err(FrameworkError::Internal)` if no disk is registered under
    /// `name`. The returned `Operator` is cheap to clone (it is `Arc`-backed).
    pub fn disk(name: &str) -> Result<Operator, FrameworkError> {
        registry::get(name)
    }

    /// Register a local filesystem disk rooted at `root`.
    ///
    /// The root directory is created if it does not already exist. Paths
    /// passed to subsequent `disk.write(...)`, `disk.read(...)`, etc. are
    /// resolved relative to this root.
    pub fn register_fs(
        name: impl Into<String>,
        root: impl AsRef<Path>,
    ) -> Result<(), FrameworkError> {
        let root_str = root.as_ref().to_string_lossy();
        let builder = services::Fs::default().root(&root_str);
        let op = Operator::new(builder)
            .map_err(|e| FrameworkError::internal(format!("opendal fs init: {e}")))?
            .finish();
        registry::register(name, op);
        Ok(())
    }

    /// Register an in-memory disk. Useful for tests, ephemeral buffers, and
    /// any case where persistence is explicitly not required.
    pub fn register_memory(name: impl Into<String>) {
        let op = Operator::new(services::Memory::default())
            .expect("opendal memory service is infallible")
            .finish();
        registry::register(name, op);
    }

    /// Register an S3 (or S3-compatible) disk.
    pub fn register_s3(
        name: impl Into<String>,
        config: S3Config,
    ) -> Result<(), FrameworkError> {
        let mut builder = services::S3::default().bucket(&config.bucket);
        if let Some(region) = config.region.as_deref() {
            builder = builder.region(region);
        }
        if let Some(endpoint) = config.endpoint.as_deref() {
            builder = builder.endpoint(endpoint);
        }
        if let Some(key) = config.access_key_id.as_deref() {
            builder = builder.access_key_id(key);
        }
        if let Some(secret) = config.secret_access_key.as_deref() {
            builder = builder.secret_access_key(secret);
        }
        if let Some(root) = config.root.as_deref() {
            builder = builder.root(root);
        }
        let op = Operator::new(builder)
            .map_err(|e| FrameworkError::internal(format!("opendal s3 init: {e}")))?
            .finish();
        registry::register(name, op);
        Ok(())
    }

    /// Register an Azure Blob Storage disk.
    pub fn register_azblob(
        name: impl Into<String>,
        config: AzBlobConfig,
    ) -> Result<(), FrameworkError> {
        let mut builder = services::Azblob::default()
            .container(&config.container)
            .account_name(&config.account_name)
            .account_key(&config.account_key);
        if let Some(endpoint) = config.endpoint.as_deref() {
            builder = builder.endpoint(endpoint);
        }
        if let Some(root) = config.root.as_deref() {
            builder = builder.root(root);
        }
        let op = Operator::new(builder)
            .map_err(|e| FrameworkError::internal(format!("opendal azblob init: {e}")))?
            .finish();
        registry::register(name, op);
        Ok(())
    }

    /// Register a Google Cloud Storage disk.
    pub fn register_gcs(
        name: impl Into<String>,
        config: GcsConfig,
    ) -> Result<(), FrameworkError> {
        let mut builder = services::Gcs::default().bucket(&config.bucket);
        if let Some(credential) = config.credential.as_deref() {
            builder = builder.credential(credential);
        }
        if let Some(path) = config.credential_path.as_deref() {
            builder = builder.credential_path(path);
        }
        if let Some(endpoint) = config.endpoint.as_deref() {
            builder = builder.endpoint(endpoint);
        }
        if let Some(root) = config.root.as_deref() {
            builder = builder.root(root);
        }
        let op = Operator::new(builder)
            .map_err(|e| FrameworkError::internal(format!("opendal gcs init: {e}")))?
            .finish();
        registry::register(name, op);
        Ok(())
    }

    /// Install a fake (in-memory, isolated) storage environment for the
    /// duration of a test.
    ///
    /// Returns a [`testing::StorageFakeGuard`] that:
    /// - Serializes against other `Storage::fake()` callers via a process-wide
    ///   `Mutex` (so parallel `#[tokio::test]` cases do not race on the
    ///   registry), and
    /// - Resets the registry on drop.
    ///
    /// A `"default"` memory disk is pre-registered for convenience; tests can
    /// register additional disks under whatever names they like.
    #[cfg(any(test, feature = "testing"))]
    pub fn fake() -> testing::StorageFakeGuard {
        testing::install_fake()
    }
}
