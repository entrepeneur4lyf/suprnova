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
//! // Later (e.g. the `db:seed` console command):
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
//!   `where Self: Sized` clause keeps the trait off the object-safe
//!   path, but the registry stores type-erased fn pointers anyway so
//!   object safety is not needed.
//!
//! # Selective execution (`run_one`)
//!
//! [`run_one`] looks up a single registered seeder by its stable
//! `name()` and runs it without running its peers. This is the
//! engine for `db:seed --class=<Name>` — the Laravel-side
//! `php artisan db:seed --class=UserSeeder` ergonomic. Lookup misses
//! return `Err(FrameworkError::not_found(...))` so the CLI surfaces
//! "no seeder registered for X" rather than silently succeeding.
//!
//! # Model-event muting (`without_events`)
//!
//! [`without_events`] is the Laravel-`WithoutModelEvents` analogue.
//! A `tokio::task_local!` flag is set for the duration of the passed
//! future; [`crate::eloquent::events::dispatch_after`] and
//! [`crate::eloquent::events::dispatch_cancellable`] check the flag
//! and short-circuit to `Ok(())` when it is set. That single check
//! at each chokepoint covers every model lifecycle event (both
//! cancellable and non-cancellable). The effect is task-scoped —
//! only seeders that opt in are affected, and the application's
//! own HTTP request paths continue to fire events normally. Nested
//! calls compose (the inner future inherits the outer flag).
//!
//! **Note:** this only matters for code that goes through the
//! `Model` trait (`Model::create`, `Model::save`, etc.). Factories
//! persist via `ActiveModelTrait::insert` and bypass the model-
//! event dispatch path entirely — there's nothing to mute in that
//! path. See [`without_events`]'s rustdoc for the full
//! when-is-this-useful breakdown.

use crate::error::FrameworkError;
use crate::lock;
use async_trait::async_trait;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use std::future::Future;
use std::sync::RwLock;

/// Function-pointer view of a registered seeder. Captures the type
/// parameter through a closure produced in [`register`].
type SeederFn = fn() -> BoxFuture<'static, Result<(), FrameworkError>>;

static REGISTRY: RwLock<Option<IndexMap<String, SeederFn>>> = RwLock::new(None);

tokio::task_local! {
    /// When set to `true`, [`crate::eloquent::events::dispatch_after`]
    /// and [`crate::eloquent::events::dispatch_cancellable`] short-
    /// circuit to `Ok(())` without invoking listeners. Established by
    /// [`without_events`] for the duration of the passed future.
    pub(crate) static EVENTS_MUTED: bool;
}

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
    match lock::write(&REGISTRY) {
        Ok(mut g) => {
            g.get_or_insert_with(IndexMap::new)
                .insert(S::name().to_string(), f);
        }
        Err(_) => {
            tracing::error!(
                seeder = S::name(),
                "Seeder registry lock poisoned; skipping registration."
            );
        }
    }
}

/// Run every registered seeder in registration order. Stops on the
/// first error — seeders that already ran are NOT rolled back. The
/// `db:seed` console command is the typical caller; tests can also
/// drive this directly after registering seeders.
pub async fn run_all() -> Result<(), FrameworkError> {
    let entries: Vec<(String, SeederFn)> = {
        let g = lock::read(&REGISTRY)?;
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

/// Run a single registered seeder by its [`Seeder::name`].
///
/// This is the engine for `db:seed --class=<Name>`. Behavior:
///
/// - Looks up the seeder by exact name in the registry.
/// - On hit: emits the same `tracing::info!` that [`run_all`] would
///   and awaits the seeder's `run()`.
/// - On miss: returns `Err(FrameworkError::not_found("no seeder
///   registered for {name}"))`. The CLI surfaces this as a non-zero
///   exit and a helpful error message rather than silently no-oping.
///
/// Calling `run_one` does NOT also call other seeders — unlike
/// `run_all`, this is targeted execution. Laravel's
/// `db:seed --class=UserSeeder` does the same.
pub async fn run_one(name: &str) -> Result<(), FrameworkError> {
    let entry = {
        let g = lock::read(&REGISTRY)?;
        g.as_ref().and_then(|m| m.get(name).copied())
    };
    match entry {
        Some(f) => {
            tracing::info!(seeder = %name, "running seeder");
            f().await
        }
        None => Err(FrameworkError::not_found(format!(
            "no seeder registered for `{name}`"
        ))),
    }
}

/// Number of currently-registered seeders. Useful for tests asserting
/// "the bootstrap registered all expected seeders."
///
/// Returns `0` on registry-lock poison after logging an error —
/// matches the "treat poison as empty" pattern used by the
/// registration path.
pub fn count() -> usize {
    match lock::read(&REGISTRY) {
        Ok(g) => g.as_ref().map(|m| m.len()).unwrap_or(0),
        Err(_) => {
            tracing::error!("Seeder registry lock poisoned; reporting count=0.");
            0
        }
    }
}

/// Whether a seeder with the given name is registered.
///
/// Used by `db:seed --class=` argument validation and by tests
/// asserting that bootstrap registered the expected fixtures.
/// Returns `false` on registry-lock poison (matches `count()`).
pub fn is_registered(name: &str) -> bool {
    match lock::read(&REGISTRY) {
        Ok(g) => g.as_ref().is_some_and(|m| m.contains_key(name)),
        Err(_) => {
            tracing::error!("Seeder registry lock poisoned; reporting is_registered=false.");
            false
        }
    }
}

/// Run `fut` with Eloquent model events muted.
///
/// The Laravel-`WithoutModelEvents` analogue. While the future is
/// awaiting, both [`crate::eloquent::events::dispatch_after`] and
/// [`crate::eloquent::events::dispatch_cancellable`] short-circuit
/// to `Ok(())` — covering every model lifecycle event (both
/// cancellable and non-cancellable) at its single chokepoint.
///
/// The effect is **task-scoped**: only the work performed inside
/// `fut` is muted; concurrent work on other tasks (HTTP request
/// handlers, other seeders, queue workers) continues to fire
/// events normally. Nested calls compose — the inner future
/// inherits the outer flag.
///
/// # When is this useful?
///
/// Model-driven inserts. A seeder that calls `User::create(...)` in
/// a loop fires `Creating` / `Saving` / `Created` / `Saved` on every
/// row, which invokes any registered `Observer<User>` and any
/// queued broadcast listeners. Wrapping that loop in
/// `seed::without_events` skips both the per-row cancellable veto
/// path and the per-row after-event fanout — handy for bulk seeds
/// that don't want to wake the broadcaster or trigger downstream
/// jobs.
///
/// **Factory-driven inserts do NOT fire model events.**
/// `UserFactory::new().count(50).create_many()` writes through the
/// `Persistable` impl (`ActiveModelTrait::insert`), which bypasses
/// the `Model` trait's `create`/`save` methods that dispatch
/// lifecycle hooks. There's nothing to mute in that path. Use this
/// helper when you're driving the `Model` trait directly.
///
/// # Example
///
/// ```ignore
/// use suprnova::{seed, async_trait, FrameworkError, Seeder};
///
/// pub struct UsersSeeder;
///
/// #[async_trait]
/// impl Seeder for UsersSeeder {
///     fn name() -> &'static str { "UsersSeeder" }
///     async fn run() -> Result<(), FrameworkError> {
///         seed::without_events(async {
///             // Loop of Model::create calls — each would normally
///             // fire Creating/Saving/Created/Saved. Muted here.
///             for i in 0..50 {
///                 User::create(User {
///                     id: 0, name: format!("user{i}"), ..Default::default()
///                 }).await?;
///             }
///             Ok(())
///         }).await
///     }
/// }
/// ```
pub async fn without_events<F, T>(fut: F) -> T
where
    F: Future<Output = T>,
{
    EVENTS_MUTED.scope(true, fut).await
}

/// Returns `true` if the current task is executing inside a
/// [`without_events`] scope.
///
/// Used by the Eloquent event dispatch sites
/// ([`crate::eloquent::events::dispatch_after`] and
/// [`crate::eloquent::events::dispatch_cancellable`]) to decide
/// whether to short-circuit. Not exposed at the crate root — user
/// code should never need to check this directly; opting into
/// [`without_events`] is the public surface.
pub(crate) fn events_muted() -> bool {
    EVENTS_MUTED.try_with(|m| *m).unwrap_or(false)
}

/// Clear every registered seeder. Test-only helper — production code
/// should never need to call this because the registry is built once
/// at boot.
///
/// Silently no-ops on poison (matches the test-helper-friendly
/// shape `ScopeRegistry::__clear_for_tests` and
/// `ConnectionRegistry::clear` already use).
#[doc(hidden)]
pub fn clear() {
    if let Ok(mut g) = lock::write(&REGISTRY) {
        *g = None;
    }
}
