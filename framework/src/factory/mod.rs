//! Model factories — produce randomized model instances for tests and
//! seed data with a Laravel-style fluent builder.
//!
//! ```ignore
//! use suprnova::factory::Factory;
//! use fake::{Fake, Faker};
//!
//! // The minimal hand-written form: pair a marker struct with a
//! // `Factory` impl that knows how to build one instance.
//! struct UserFactory;
//! impl Factory for UserFactory {
//!     type Model = User;
//!     fn definition() -> User {
//!         Faker.fake::<User>()  // assumes `User: fake::Dummy`
//!     }
//! }
//!
//! // Build one
//! let user = UserFactory::new().make();
//!
//! // Build many
//! let users = UserFactory::new().count(10).make_many();
//!
//! // Override per-call
//! let admin = UserFactory::new()
//!     .with(|u| u.is_admin = true)
//!     .make();
//! ```
//!
//! `create` / `create_many` (in [`persist`]) extend the builder with
//! SeaORM persistence. The fluent surface is intentionally close to
//! Laravel's `User::factory()->count(10)->create()` so the mental model
//! ports without translation.
//!
//! For the typical case where a model derives `fake::Dummy`, see
//! `#[derive(Factory)]` which generates the marker struct + impl from
//! a `#[factory(model = "...")]` attribute.

mod persist;
mod sequence;

pub use persist::{Persistable, persist_via_seaorm};
pub use sequence::Sequence;

/// A factory produces randomized instances of `Model`. Each call to
/// `definition()` returns a fresh, independently-randomized value —
/// the trait carries no per-instance state.
///
/// Implementors are typically zero-sized marker types so callers can
/// reach the factory by name (`UserFactory::new()`) without holding a
/// handle.
pub trait Factory {
    type Model;

    /// Build one instance with all default-randomized fields. The
    /// builder's `with(...)` overrides run AFTER this returns, so
    /// implementations should populate every field they want
    /// randomized — overrides correct the parts the test cares about.
    fn definition() -> Self::Model
    where
        Self: Sized;

    /// Start a fluent builder. Default `count` is 1; default override
    /// list is empty.
    fn new() -> FactoryBuilder<Self::Model>
    where
        Self: Sized,
    {
        FactoryBuilder {
            count: 1,
            overrides: Vec::new(),
            factory_fn: Self::definition,
        }
    }

    /// Sugar for `Self::new().count(n)`. Matches Laravel's
    /// `Factory::times(int)` API for the "I want N of these" pattern
    /// without the extra method call.
    fn times(n: usize) -> FactoryBuilder<Self::Model>
    where
        Self: Sized,
    {
        Self::new().count(n)
    }
}

/// Boxed override closure shape — extracted into a type alias so the
/// builder's field reads clean and clippy's `type_complexity` lint is
/// satisfied at the public API boundary.
pub(crate) type Override<M> = Box<dyn Fn(&mut M) + Send + Sync + 'static>;

/// Fluent builder returned by [`Factory::new`]. Owns the per-instance
/// count and the list of override closures.
///
/// Boxed closures are `Send + Sync + 'static` so the builder itself is
/// `Send` — important for the async `create` / `create_many` paths,
/// which capture the builder across an `.await` point on the SeaORM
/// insert.
pub struct FactoryBuilder<M> {
    pub(crate) count: usize,
    pub(crate) overrides: Vec<Override<M>>,
    pub(crate) factory_fn: fn() -> M,
}

impl<M> FactoryBuilder<M> {
    /// Set the number of instances `make_many` / `create_many` will
    /// produce. Has no effect on `make` / `create`, which always
    /// return one instance.
    pub fn count(mut self, n: usize) -> Self {
        self.count = n;
        self
    }

    /// Add an override closure that runs against every produced
    /// instance after `definition()`. Multiple `with` calls compose
    /// in registration order, so a later override can clobber an
    /// earlier one.
    pub fn with<F>(mut self, f: F) -> Self
    where
        F: Fn(&mut M) + Send + Sync + 'static,
    {
        self.overrides.push(Box::new(f));
        self
    }

    /// Prepend an override closure to the front of the chain. The
    /// override runs BEFORE any other registered override, so
    /// downstream `with(...)` calls win on the same field.
    ///
    /// Mirrors Laravel's `Factory::prependState($state)` — useful
    /// when a state method wants to set a default that a caller can
    /// still override with a later `with(...)`.
    pub fn prepend<F>(mut self, f: F) -> Self
    where
        F: Fn(&mut M) + Send + Sync + 'static,
    {
        self.overrides.insert(0, Box::new(f));
        self
    }

    /// Conditional builder extension — applies `f` to the builder
    /// only if `cond` is true; otherwise returns the builder
    /// unchanged. Mirrors Laravel's `Conditionable::when($cond, $cb)`
    /// for the "thread a flag through a chain" pattern without
    /// breaking the fluent style.
    ///
    /// ```ignore
    /// UserFactory::times(10)
    ///     .with(|u| u.active = true)
    ///     .when(seed_admins, |b| b.with(|u| u.role = "admin".into()))
    ///     .create_many().await?;
    /// ```
    pub fn when<F>(self, cond: bool, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        if cond { f(self) } else { self }
    }

    /// Build a single in-memory instance. Runs every registered
    /// override against the produced value. Does NOT persist —
    /// see [`persist::FactoryBuilder::create`] for the persisted
    /// variant.
    pub fn make(self) -> M {
        let mut model = (self.factory_fn)();
        for o in &self.overrides {
            o(&mut model);
        }
        model
    }

    /// Force-single in-memory build — discards any prior `count(n)`
    /// and produces exactly one instance. Equivalent to
    /// `self.count(1).make()`, but reads cleaner when a shared state
    /// method has set `count` internally and the caller wants one.
    ///
    /// Mirrors Laravel's `Factory::makeOne($attrs)`.
    pub fn make_one(self) -> M {
        self.count(1).make()
    }

    /// Build `count` instances in memory, applying overrides to each.
    /// Each instance is independently randomized via a fresh call to
    /// `definition()`.
    pub fn make_many(self) -> Vec<M> {
        let FactoryBuilder {
            count,
            overrides,
            factory_fn,
        } = self;
        (0..count)
            .map(|_| {
                let mut model = factory_fn();
                for o in &overrides {
                    o(&mut model);
                }
                model
            })
            .collect()
    }
}

/// Persistence-aware builder methods. Available whenever `M:
/// Persistable` — which, thanks to the blanket impl in
/// [`persist`], every SeaORM `Model` satisfies for free.
impl<M> FactoryBuilder<M>
where
    M: Persistable + 'static,
{
    /// Build one instance + persist it through the bound storage.
    /// Returns the canonicalized post-insert model (assigned id,
    /// defaulted columns resolved, etc.).
    pub async fn create(self) -> Result<M, crate::error::FrameworkError> {
        self.make().persist().await
    }

    /// Force-single persisted build — discards any prior `count(n)`
    /// and produces exactly one persisted instance. Equivalent to
    /// `self.count(1).create().await`.
    ///
    /// Mirrors Laravel's `Factory::createOne($attrs)`.
    pub async fn create_one(self) -> Result<M, crate::error::FrameworkError> {
        self.count(1).create().await
    }

    /// Build `count` instances + persist each in turn. Returns every
    /// post-insert model. Persists sequentially — if a later insert
    /// fails, the prior inserts are NOT rolled back (the call site is
    /// expected to wrap a transaction if it needs atomicity).
    pub async fn create_many(self) -> Result<Vec<M>, crate::error::FrameworkError> {
        let models = self.make_many();
        let mut out = Vec::with_capacity(models.len());
        for m in models {
            out.push(m.persist().await?);
        }
        Ok(out)
    }
}
