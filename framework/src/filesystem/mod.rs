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

mod disk;
mod path_guard;
mod registry;
pub mod streaming;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use disk::{ChecksumAlgorithm, DiskExt};
pub use streaming::copy_between_disks;

use crate::FrameworkError;
use opendal::{Operator, services};
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
///
/// The `Debug` impl masks `secret_access_key` (the only secret-bearing
/// field) as `Some("[REDACTED]")` / `None` so a stray `dbg!()` or
/// `tracing::info!(?config)` does not leak AWS credentials. Pattern
/// mirrors [`crate::EncryptionKey`]'s redacting `Debug`.
#[derive(Clone, Default)]
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

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("access_key_id", &self.access_key_id)
            .field(
                "secret_access_key",
                &self.secret_access_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("root", &self.root)
            .finish()
    }
}

/// Configuration for the Azure Blob Storage driver.
///
/// The `Debug` impl masks `account_key` (the storage account secret)
/// so a stray `dbg!()` or `tracing::info!(?config)` does not leak the
/// shared key.
#[derive(Clone, Default)]
pub struct AzBlobConfig {
    /// Container name. Required.
    pub container: String,
    /// Storage account name.
    pub account_name: String,
    /// Storage account key.
    pub account_key: String,
    /// Custom endpoint (e.g. the Azurite emulator or a sovereign cloud). When
    /// omitted, the standard public endpoint
    /// `https://{account_name}.blob.core.windows.net` is used.
    pub endpoint: Option<String>,
    /// Root prefix within the container.
    pub root: Option<String>,
}

impl std::fmt::Debug for AzBlobConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `account_key` is a String (not Option<String>), so render
        // it as a marker that distinguishes "set" from "empty" without
        // leaking the value.
        let account_key_repr = if self.account_key.is_empty() {
            "[unset]"
        } else {
            "[REDACTED]"
        };
        f.debug_struct("AzBlobConfig")
            .field("container", &self.container)
            .field("account_name", &self.account_name)
            .field("account_key", &account_key_repr)
            .field("endpoint", &self.endpoint)
            .field("root", &self.root)
            .finish()
    }
}

/// Configuration for the Google Cloud Storage driver.
///
/// The `Debug` impl masks `credential` (the inline JSON service-account
/// key) so a stray `dbg!()` or `tracing::info!(?config)` does not leak
/// the JSON key bytes. `credential_path` is NOT redacted because it's a
/// filesystem path, not the credential itself.
#[derive(Clone, Default)]
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

impl std::fmt::Debug for GcsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcsConfig")
            .field("bucket", &self.bucket)
            .field(
                "credential",
                &self.credential.as_ref().map(|_| "[REDACTED]"),
            )
            .field("credential_path", &self.credential_path)
            .field("endpoint", &self.endpoint)
            .field("root", &self.root)
            .finish()
    }
}

/// Default resilience layer applied by the cloud convenience constructors
/// ([`Storage::register_s3`], [`Storage::register_azblob`],
/// [`Storage::register_gcs`]).
///
/// Object stores routinely return transient throttling / 5xx errors, so the
/// convenience constructors retry by default. Callers who need a different
/// policy (more retries, timeouts, logging, metrics) use the `_with` variants,
/// which apply no default layer and hand over full control of the stack. Local
/// filesystem and in-memory disks are not wrapped — they have no transient
/// failures worth retrying.
fn default_cloud_resilience(op: Operator) -> Operator {
    op.layer(opendal::layers::RetryLayer::new().with_max_times(3))
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
    ///
    /// Equivalent to [`Storage::register_fs_with`] with an identity closure.
    ///
    /// # Testing
    ///
    /// The disk registry is process-global. Tests that call any `register_*`
    /// method directly race on this shared state when run in parallel — wrap
    /// them in a [`Storage::fake`] guard, which serializes fake-using tests
    /// process-wide and wipes the registry on drop.
    pub fn register_fs(
        name: impl Into<String>,
        root: impl AsRef<Path>,
    ) -> Result<(), FrameworkError> {
        Self::register_fs_with(name, root, |op| op)
    }

    /// Register a local filesystem disk with a custom layer stack applied to
    /// the underlying [`Operator`] before it lands in the registry.
    ///
    /// # Available layers
    ///
    /// Suprnova enables these `opendal::layers::*` types out of the box (each
    /// gated behind one `opendal` feature in `framework/Cargo.toml`):
    ///
    /// - [`RetryLayer`](https://docs.rs/opendal/0.56/opendal/layers/struct.RetryLayer.html) —
    ///   exponential-backoff retries on transient 5xx / throttling.
    /// - [`TimeoutLayer`](https://docs.rs/opendal/0.56/opendal/layers/struct.TimeoutLayer.html) —
    ///   per-operation timeout.
    /// - [`LoggingLayer`](https://docs.rs/opendal/0.56/opendal/layers/struct.LoggingLayer.html) —
    ///   debug-level structured logs for every operation.
    /// - [`TracingLayer`](https://docs.rs/opendal/0.56/opendal/layers/struct.TracingLayer.html) —
    ///   `tracing` spans per operation; bridges to OTel through
    ///   `tracing-opentelemetry` when the framework's `otel` feature is on.
    /// - [`PrometheusClientLayer`](https://docs.rs/opendal/0.56/opendal/layers/struct.PrometheusClientLayer.html) —
    ///   histograms + counters for the `prometheus-client` registry.
    ///
    /// Layer order matters: outermost layer wraps everything inside it. The
    /// idiomatic stack is `RetryLayer → TimeoutLayer → LoggingLayer`, so a
    /// timed-out attempt still logs and a retry covers transport failures.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use opendal::layers::{LoggingLayer, RetryLayer, TimeoutLayer, TracingLayer};
    /// use std::time::Duration;
    /// use suprnova::Storage;
    ///
    /// Storage::register_fs_with("local", "./storage", |op| {
    ///     op.layer(RetryLayer::new().with_max_times(3))
    ///       .layer(TimeoutLayer::new().with_timeout(Duration::from_secs(30)))
    ///       .layer(LoggingLayer::default())
    ///       .layer(TracingLayer::new())
    /// })?;
    /// ```
    pub fn register_fs_with(
        name: impl Into<String>,
        root: impl AsRef<Path>,
        layer_fn: impl FnOnce(Operator) -> Operator,
    ) -> Result<(), FrameworkError> {
        // Reject non-UTF-8 roots rather than silently mangling them with a
        // lossy conversion (which could root the disk at the wrong directory).
        let root_str = root
            .as_ref()
            .to_str()
            .ok_or_else(|| FrameworkError::internal("storage fs root path is not valid UTF-8"))?;
        let builder = services::Fs::default().root(root_str);
        // `PathGuardLayer` is applied to the raw FS operator before the user's
        // `layer_fn` runs, so the traversal guard sits closest to the backend
        // and the caller's own layers (retry, logging, tracing) wrap it. The
        // caller can add layers but cannot strip the guard.
        let guarded = Operator::new(builder)
            .map_err(|e| FrameworkError::internal(format!("opendal fs init: {e}")))?
            .finish()
            .layer(path_guard::PathGuardLayer);
        let layered = layer_fn(guarded);
        registry::register(name, layered);
        Ok(())
    }

    /// Register an in-memory disk. Useful for tests, ephemeral buffers, and
    /// any case where persistence is explicitly not required.
    ///
    /// Equivalent to [`Storage::register_memory_with`] with an identity closure.
    ///
    /// # Testing
    ///
    /// The disk registry is process-global. Tests that call any `register_*`
    /// method directly race on this shared state when run in parallel — wrap
    /// them in a [`Storage::fake`] guard, which serializes fake-using tests
    /// process-wide and wipes the registry on drop.
    pub fn register_memory(name: impl Into<String>) {
        Self::register_memory_with(name, |op| op)
    }

    /// Register an in-memory disk with a custom layer stack.
    ///
    /// Memory backend construction is infallible, so the closure always runs.
    /// Useful for testing layer behaviour without external services.
    ///
    /// See [`Storage::register_fs_with`] for the full list of available
    /// layers (retry, timeout, logging, tracing, prometheus-client).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use opendal::layers::{LoggingLayer, RetryLayer};
    /// use suprnova::Storage;
    ///
    /// Storage::register_memory_with("scratch", |op| {
    ///     op.layer(RetryLayer::new().with_max_times(2))
    ///       .layer(LoggingLayer::default())
    /// });
    /// ```
    pub fn register_memory_with(
        name: impl Into<String>,
        layer_fn: impl FnOnce(Operator) -> Operator,
    ) {
        let raw = Operator::new(services::Memory::default())
            .expect("opendal memory service is infallible")
            .finish();
        let layered = layer_fn(raw);
        registry::register(name, layered);
    }

    /// Register an S3 (or S3-compatible) disk.
    ///
    /// Applies a default [`RetryLayer`](opendal::layers::RetryLayer)
    /// (`with_max_times(3)`) so transient throttling / 5xx errors are retried.
    /// Use [`Storage::register_s3_with`] for full control of the layer stack
    /// (it applies no default layer).
    ///
    /// # Testing
    ///
    /// The disk registry is process-global. Tests that call any `register_*`
    /// method directly race on this shared state when run in parallel — wrap
    /// them in a [`Storage::fake`] guard, which serializes fake-using tests
    /// process-wide and wipes the registry on drop.
    pub fn register_s3(name: impl Into<String>, config: S3Config) -> Result<(), FrameworkError> {
        Self::register_s3_with(name, config, default_cloud_resilience)
    }

    /// Register an S3 disk with a custom layer stack applied to the
    /// [`Operator`] before it lands in the registry.
    ///
    /// Production S3 deployments need retries (for throttling and transient
    /// 5xx), timeouts, and observability. See [`Storage::register_fs_with`]
    /// for the full list of available layers (retry, timeout, logging,
    /// tracing, prometheus-client).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use opendal::layers::{LoggingLayer, PrometheusClientLayer, RetryLayer, TimeoutLayer, TracingLayer};
    /// use prometheus_client::registry::Registry;
    /// use std::time::Duration;
    /// use suprnova::{S3Config, Storage};
    ///
    /// let mut registry = Registry::default();
    /// let metrics_layer = PrometheusClientLayer::new(&mut registry);
    ///
    /// Storage::register_s3_with(
    ///     "uploads",
    ///     S3Config { bucket: "my-bucket".into(), region: Some("us-east-1".into()), ..Default::default() },
    ///     |op| {
    ///         op.layer(RetryLayer::new().with_max_times(3))
    ///           .layer(TimeoutLayer::new().with_timeout(Duration::from_secs(30)))
    ///           .layer(LoggingLayer::default())
    ///           .layer(TracingLayer::new())
    ///           .layer(metrics_layer)
    ///     },
    /// )?;
    /// ```
    pub fn register_s3_with(
        name: impl Into<String>,
        config: S3Config,
        layer_fn: impl FnOnce(Operator) -> Operator,
    ) -> Result<(), FrameworkError> {
        if config.bucket.trim().is_empty() {
            return Err(FrameworkError::internal(
                "S3 storage config requires a non-empty `bucket`",
            ));
        }
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
        let raw = Operator::new(builder)
            .map_err(|e| FrameworkError::internal(format!("opendal s3 init: {e}")))?
            .finish();
        let layered = layer_fn(raw);
        registry::register(name, layered);
        Ok(())
    }

    /// Register an Azure Blob Storage disk.
    ///
    /// Applies a default [`RetryLayer`](opendal::layers::RetryLayer)
    /// (`with_max_times(3)`) so transient throttling / 5xx errors are retried.
    /// Use [`Storage::register_azblob_with`] for full control of the layer
    /// stack (it applies no default layer).
    ///
    /// # Testing
    ///
    /// The disk registry is process-global. Tests that call any `register_*`
    /// method directly race on this shared state when run in parallel — wrap
    /// them in a [`Storage::fake`] guard, which serializes fake-using tests
    /// process-wide and wipes the registry on drop.
    pub fn register_azblob(
        name: impl Into<String>,
        config: AzBlobConfig,
    ) -> Result<(), FrameworkError> {
        Self::register_azblob_with(name, config, default_cloud_resilience)
    }

    /// Register an Azure Blob Storage disk with a custom layer stack applied
    /// to the [`Operator`] before it lands in the registry.
    ///
    /// See [`Storage::register_fs_with`] for the full list of available
    /// layers (retry, timeout, logging, tracing, prometheus-client) and a
    /// canonical ordering example.
    pub fn register_azblob_with(
        name: impl Into<String>,
        config: AzBlobConfig,
        layer_fn: impl FnOnce(Operator) -> Operator,
    ) -> Result<(), FrameworkError> {
        if config.container.trim().is_empty()
            || config.account_name.trim().is_empty()
            || config.account_key.trim().is_empty()
        {
            return Err(FrameworkError::internal(
                "Azure Blob storage config requires non-empty `container`, `account_name`, and `account_key`",
            ));
        }
        // opendal's Azblob backend requires an explicit endpoint. When the
        // caller omits it, derive the standard public Azure Blob endpoint from
        // the account name; an explicit endpoint (e.g. the Azurite emulator or
        // a sovereign cloud) is used as-is.
        let endpoint = config
            .endpoint
            .clone()
            .unwrap_or_else(|| format!("https://{}.blob.core.windows.net", config.account_name));
        let mut builder = services::Azblob::default()
            .container(&config.container)
            .account_name(&config.account_name)
            .account_key(&config.account_key)
            .endpoint(&endpoint);
        if let Some(root) = config.root.as_deref() {
            builder = builder.root(root);
        }
        let raw = Operator::new(builder)
            .map_err(|e| FrameworkError::internal(format!("opendal azblob init: {e}")))?
            .finish();
        let layered = layer_fn(raw);
        registry::register(name, layered);
        Ok(())
    }

    /// Register a Google Cloud Storage disk.
    ///
    /// Applies a default [`RetryLayer`](opendal::layers::RetryLayer)
    /// (`with_max_times(3)`) so transient throttling / 5xx errors are retried.
    /// Use [`Storage::register_gcs_with`] for full control of the layer stack
    /// (it applies no default layer).
    ///
    /// # Testing
    ///
    /// The disk registry is process-global. Tests that call any `register_*`
    /// method directly race on this shared state when run in parallel — wrap
    /// them in a [`Storage::fake`] guard, which serializes fake-using tests
    /// process-wide and wipes the registry on drop.
    pub fn register_gcs(name: impl Into<String>, config: GcsConfig) -> Result<(), FrameworkError> {
        Self::register_gcs_with(name, config, default_cloud_resilience)
    }

    /// Register a Google Cloud Storage disk with a custom layer stack applied
    /// to the [`Operator`] before it lands in the registry.
    ///
    /// See [`Storage::register_fs_with`] for the full list of available
    /// layers (retry, timeout, logging, tracing, prometheus-client) and a
    /// canonical ordering example.
    pub fn register_gcs_with(
        name: impl Into<String>,
        config: GcsConfig,
        layer_fn: impl FnOnce(Operator) -> Operator,
    ) -> Result<(), FrameworkError> {
        if config.bucket.trim().is_empty() {
            return Err(FrameworkError::internal(
                "GCS storage config requires a non-empty `bucket`",
            ));
        }
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
        let raw = Operator::new(builder)
            .map_err(|e| FrameworkError::internal(format!("opendal gcs init: {e}")))?
            .finish();
        let layered = layer_fn(raw);
        registry::register(name, layered);
        Ok(())
    }

    /// Drop a registered disk by name, returning whether it was present.
    ///
    /// Mirrors Laravel's `FilesystemManager::forgetDisk`. Useful for
    /// configuration reloads or tests that need to swap a disk implementation
    /// at runtime without spinning up [`Storage::fake`].
    pub fn forget(name: &str) -> bool {
        registry::forget(name)
    }

    /// Drop every registered disk.
    ///
    /// Mirrors Laravel's `FilesystemManager::purge()` (which clears every
    /// disk when called without arguments). Production code rarely needs
    /// this; tests should prefer [`Storage::fake`], which combines a purge
    /// with a process-wide mutex.
    pub fn purge() {
        registry::purge()
    }

    /// Return the sorted names of every currently-registered disk.
    ///
    /// Handy for diagnostic endpoints, admin dashboards, and tests that need
    /// to assert the boot-time disk set.
    pub fn disks() -> Vec<String> {
        registry::names()
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
