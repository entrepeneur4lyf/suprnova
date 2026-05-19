//! Database seeders — process-global registry of ordered fixture
//! runners.
//!
//! Each seeder is a zero-sized type that implements [`Seeder`]; the
//! framework stores a function pointer per registered seeder and
//! runs them in **registration order** via [`run_all`]. Insertion
//! order matters because seeders typically have implicit dependencies
//! (users before posts, posts before comments).
//!
//! ```ignore
//! use suprnova::{async_trait, FrameworkError, Seeder};
//!
//! pub struct UsersSeeder;
//!
//! #[async_trait]
//! impl Seeder for UsersSeeder {
//!     fn name() -> &'static str { "UsersSeeder" }
//!     async fn run() -> Result<(), FrameworkError> {
//!         UserFactory::new().count(50).create_many().await?;
//!         Ok(())
//!     }
//! }
//!
//! // In bootstrap:
//! suprnova::seed::register::<UsersSeeder>();
//!
//! // Later (e.g. the `db:seed` console command, which lands in 6B):
//! suprnova::seed::run_all().await?;
//! ```
//!
//! # Registry semantics
//!
//! Matches the Phase 5B registries (`register_mailable_factory`,
//! `register_notification_factory`, `register_mail_renderer`):
//!
//! - Backing store is `RwLock<Option<IndexMap<String, SeederFn>>>` —
//!   lazily initialized, kept in registration order, last-write-wins
//!   on the seeder name (re-registering the same name silently
//!   replaces the function pointer so tests can swap stubs).
//! - The trait method `Seeder::run()` is an associated function (no
//!   `&self`) because seeders are stateless side-effect runners; the
//!   `where Self: Sized` clause that introduces lives the trait off
//!   the object-safe path, but the registry stores type-erased fn
//!   pointers anyway so object safety is not needed.

use crate::error::FrameworkError;
use crate::lock;
use async_trait::async_trait;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use std::sync::RwLock;

/// Function-pointer view of a registered seeder. Captures the type
/// parameter through a closure produced in [`register`].
type SeederFn = fn() -> BoxFuture<'static, Result<(), FrameworkError>>;

static REGISTRY: RwLock<Option<IndexMap<String, SeederFn>>> = RwLock::new(None);

/// A database seeder — runs once via [`run_all`] to populate fixture
/// data. Seeders carry no per-instance state; the trait surface is a
/// stable name + an async run method that returns a Result.
#[async_trait]
pub trait Seeder: Send + Sync {
    /// Stable name used as the registry key. Re-registering the same
    /// name silently replaces the prior seeder (last-write-wins) —
    /// matches the Phase 5B factory registries' contract.
    fn name() -> &'static str
    where
        Self: Sized;

    /// Run the seeder. Idempotency is the seeder's responsibility —
    /// `run_all` does not snapshot or roll back, so a seeder that
    /// inserts unconditionally will produce duplicates on re-run.
    async fn run() -> Result<(), FrameworkError>
    where
        Self: Sized;
}

/// Register a seeder type. Inserts it into the global registry under
/// its `name()`. Order matters — `run_all` visits seeders in the
/// order they were registered. Re-registering a name replaces the
/// prior function pointer in-place (IndexMap preserves the original
/// position, so test stubs slot in cleanly).
pub fn register<S: Seeder + 'static>() {
    let f: SeederFn = || Box::pin(S::run());
    let mut g = lock::write(&REGISTRY).expect("seeder registry poisoned");
    g.get_or_insert_with(IndexMap::new)
        .insert(S::name().to_string(), f);
}

/// Run every registered seeder in registration order. Stops on the
/// first error — seeders that already ran are NOT rolled back. The
/// db:seed console command (Phase 6B) is the typical caller; tests
/// can also drive this directly after registering seeders.
pub async fn run_all() -> Result<(), FrameworkError> {
    let entries: Vec<(String, SeederFn)> = {
        let g = lock::read(&REGISTRY).expect("seeder registry poisoned");
        g.as_ref()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), *v)).collect())
            .unwrap_or_default()
    };
    for (name, f) in entries {
        tracing::info!(seeder = %name, "running seeder");
        f().await?;
    }
    Ok(())
}

/// Number of currently-registered seeders. Useful for tests asserting
/// "the bootstrap registered all expected seeders."
pub fn count() -> usize {
    lock::read(&REGISTRY)
        .expect("seeder registry poisoned")
        .as_ref()
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Clear every registered seeder. Test-only helper — production code
/// should never need to call this because the registry is built once
/// at boot.
#[doc(hidden)]
pub fn clear() {
    *lock::write(&REGISTRY).expect("seeder registry poisoned") = None;
}
