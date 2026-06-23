//! `when_loaded!` helper macro and `IsRelationLoaded` trait.
//!
//! These are the runtime companions for `#[data(lazy(when_loaded))]`. Users
//! call `when_loaded!` inside a `From<Entity>` (or equivalent) impl to
//! produce a `Prop::Lazy` when the named relation is preloaded, or
//! `Prop::EagerNone` when it is not.
//!
//! ## SeaORM note
//!
//! SeaORM `ModelTrait` carries no per-instance relation-loaded state — loaded
//! relations live on query results, not on the model itself. A generic blanket
//! `impl<M: ModelTrait> IsRelationLoaded for &M` has nothing to consult, so we
//! do **not** provide one. Users implement `IsRelationLoaded` on their own
//! wrapper types (e.g. `struct AlbumWithSongs { model: Album, songs: Vec<Song> }`)
//! and delegate to presence checks there.

/// Implemented by any type that can report whether a named relation has been
/// preloaded. Used by the `when_loaded!` macro (re-exported as
/// `suprnova::when_loaded!`).
///
/// # Example
///
/// ```rust,no_run
/// # use suprnova::data::IsRelationLoaded;
/// # struct Album;
/// # struct Song;
/// struct AlbumWithRelations {
///     pub album: Album,
///     pub songs: Option<Vec<Song>>,
/// }
///
/// impl IsRelationLoaded for AlbumWithRelations {
///     fn is_relation_loaded(&self, name: &str) -> bool {
///         match name {
///             "songs" => self.songs.is_some(),
///             _ => false,
///         }
///     }
/// }
/// ```
pub trait IsRelationLoaded {
    /// Returns `true` when the named relation has been loaded onto
    /// `self` — used by the [`when_loaded!`](crate::when_loaded) macro
    /// to decide whether a `Prop` resolves to `Lazy` or `EagerNone`.
    fn is_relation_loaded(&self, relation_name: &str) -> bool;
}

/// Produce a `Prop::Lazy(closure)` if the named relation is loaded on the
/// entity, or `Prop::EagerNone` if it is not.
///
/// The third argument must be a closure (`|| async { ... }`) that returns a
/// `serde_json::Value`. It is only invoked when the relation is loaded AND
/// the field is requested via `?include=`.
///
/// # Example
///
/// ```rust,no_run
/// use suprnova::when_loaded;
/// # use suprnova::data::IsRelationLoaded;
/// # #[derive(Clone, serde::Serialize)]
/// # struct Song { title: String }
/// # struct Album { songs: Vec<Song> }
/// # impl IsRelationLoaded for Album {
/// #     fn is_relation_loaded(&self, name: &str) -> bool { name == "songs" }
/// # }
/// # let entity = Album { songs: vec![] };
/// // The lazy closure must be `'static`, so it owns its data.
/// let songs = entity.songs.clone();
/// let prop = when_loaded!(&entity, "songs", move || {
///     let songs = songs.clone();
///     async move { serde_json::to_value(&songs).unwrap() }
/// });
/// ```
#[macro_export]
macro_rules! when_loaded {
    ($entity:expr, $relation:expr, $closure:expr) => {{
        use $crate::data::IsRelationLoaded as _;
        if ($entity).is_relation_loaded($relation) {
            $crate::inertia::Prop::lazy($closure)
        } else {
            $crate::inertia::Prop::EagerNone
        }
    }};
}

// Note: `when_loaded!` is available at the crate root as `suprnova::when_loaded!`
// via the `#[macro_export]` annotation above. There is no separate
// `suprnova::data::when_loaded!` path — use the crate-root path.
