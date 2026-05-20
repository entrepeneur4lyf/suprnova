//! Phase 10B T1 — relation emission for `#[suprnova::model]`.
//!
//! Reads the `relations = { ... }` block parsed by [`super::parse`] and
//! emits, per model:
//!
//! 1. Two auto-injected struct fields (`__eager: EagerLoadCache`,
//!    `__pivot: Option<Arc<dyn Any + Send + Sync>>`). The rewrite
//!    happens in [`super::expand`] before the struct definition is
//!    emitted; this module only emits the impl blocks that need
//!    `__eager` / `__pivot` to exist.
//! 2. Four dispatcher methods on the user struct (`__eager_load`,
//!    `__recurse_eager_load`, `__count_relation`,
//!    `__aggregate_relation`). T1 emits the skeletons with empty
//!    matches — T2-T7 add arms per concrete relation type.
//! 3. The `pivot::<P>()` accessor for reading per-row pivot context
//!    set by `BelongsToMany` loaders.
//! 4. Per declared relation: `<rel>_loaded()` / `<rel>_count()`
//!    accessors that read from `__eager`, plus a
//!    `RelationEntry` inventory submission for Phase 8 enumeration.
//!
//! T2-T7 will add: a concrete `Relation` impl per relation type, the
//! per-kind relation methods (`fn posts(&self) -> HasMany<Self, Post>`),
//! and the per-kind dispatcher arms inside the four methods above.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Result;

use super::parse::{to_snake, ModelInput, RelationDecl, RelationKindAttr, RelationOpt};

/// Top-level entry point. Emits every relation-related artifact for
/// the model (dispatchers + accessors + inventory submissions + the
/// per-kind relation methods).
pub fn emit(input: &ModelInput) -> Result<TokenStream> {
    let struct_ident = &input.item.ident;
    let dispatchers = emit_dispatchers(input)?;
    let pivot_accessor = emit_pivot_accessor(struct_ident);
    let with_helper = emit_with_helper(struct_ident);
    let dispatch_impl = emit_dispatch_impl(struct_ident);

    // Build per-relation accessors + relation methods + inventory
    // submissions. Each lives in its own `impl Self { ... }` block —
    // a subsequent `cargo expand` clearly shows which methods came
    // from which relation declarations.
    let mut relation_methods: Vec<TokenStream> = Vec::new();
    let mut relation_accessors: Vec<TokenStream> = Vec::new();
    let mut relation_inventory: Vec<TokenStream> = Vec::new();
    for rel in input.relations.as_deref().unwrap_or(&[]) {
        relation_methods.push(emit_relation_method(input, rel)?);
        relation_accessors.push(emit_relation_accessors(struct_ident, rel));
        relation_inventory.push(emit_relation_inventory(struct_ident, rel));
    }

    Ok(quote! {
        #dispatchers
        #pivot_accessor
        #with_helper
        #dispatch_impl
        #( #relation_methods )*
        #( #relation_accessors )*
        #( #relation_inventory )*
    })
}

/// Emit the `EagerLoadDispatch` impl that lets `Builder<M>::get` call
/// `M::eager_load(...)` without needing inherent-method access. Each
/// method delegates straight to the matching inherent dispatcher
/// (`__eager_load`, `__count_relation`, `__aggregate_relation`,
/// `__recurse_eager_load`).
///
/// Also emits the `Sealed` supertrait impl. `EagerLoadDispatch` is
/// language-sealed in the framework via a `__sealed::Sealed`
/// supertrait — user code can't write `impl EagerLoadDispatch for X`
/// because it can't write `impl Sealed for X` (the trait is reachable
/// only through the doc-hidden `__private_eloquent` path; reaching it
/// is the explicit "I know what I'm doing" gesture).
fn emit_dispatch_impl(struct_ident: &syn::Ident) -> TokenStream {
    quote! {
        impl ::suprnova::__private_eloquent::Sealed for #struct_ident {}

        impl ::suprnova::EagerLoadDispatch for #struct_ident {
            fn eager_load<'a>(
                relation: &'a str,
                parents: &'a mut [&'a mut Self],
                db: &'a ::suprnova::sea_orm::DatabaseConnection,
                predicate: ::core::option::Option<
                    ::std::boxed::Box<dyn ::std::any::Any + ::core::marker::Send + ::core::marker::Sync>,
                >,
            ) -> ::core::pin::Pin<
                ::std::boxed::Box<
                    dyn ::core::future::Future<
                            Output = ::core::result::Result<(), ::suprnova::FrameworkError>,
                        > + ::core::marker::Send + 'a,
                >,
            > {
                ::std::boxed::Box::pin(Self::__eager_load(relation, parents, db, predicate))
            }

            fn count_relation<'a>(
                relation: &'a str,
                parents: &'a mut [&'a mut Self],
                db: &'a ::suprnova::sea_orm::DatabaseConnection,
            ) -> ::core::pin::Pin<
                ::std::boxed::Box<
                    dyn ::core::future::Future<
                            Output = ::core::result::Result<(), ::suprnova::FrameworkError>,
                        > + ::core::marker::Send + 'a,
                >,
            > {
                ::std::boxed::Box::pin(Self::__count_relation(relation, parents, db))
            }

            fn aggregate_relation<'a>(
                relation: &'a str,
                column: &'a str,
                kind: ::suprnova::AggregateKind,
                parents: &'a mut [&'a mut Self],
                db: &'a ::suprnova::sea_orm::DatabaseConnection,
            ) -> ::core::pin::Pin<
                ::std::boxed::Box<
                    dyn ::core::future::Future<
                            Output = ::core::result::Result<(), ::suprnova::FrameworkError>,
                        > + ::core::marker::Send + 'a,
                >,
            > {
                ::std::boxed::Box::pin(Self::__aggregate_relation(relation, column, kind, parents, db))
            }

            fn recurse_eager_load<'a>(
                &'a mut self,
                relation: &'a str,
                rest: &'a str,
                db: &'a ::suprnova::sea_orm::DatabaseConnection,
            ) -> ::core::pin::Pin<
                ::std::boxed::Box<
                    dyn ::core::future::Future<
                            Output = ::core::result::Result<(), ::suprnova::FrameworkError>,
                        > + ::core::marker::Send + 'a,
                >,
            > {
                ::std::boxed::Box::pin(self.__recurse_eager_load(relation, rest, db))
            }

            fn set_pivot_arc(
                &mut self,
                pivot: ::core::option::Option<
                    ::std::sync::Arc<dyn ::std::any::Any + ::core::marker::Send + ::core::marker::Sync>,
                >,
            ) {
                self.__pivot = pivot;
            }
        }
    }
}

/// Emit the four dispatcher methods + per-relation match arms.
///
/// T1 shipped the skeletons (no-relation error path only); T2 adds
/// the `HasOne` and `BelongsTo` arms. T3-T7 will keep extending the
/// per-relation arm lists as more relation kinds land. The
/// `predicate` parameter on `__eager_load` carries the user's
/// optional `with_where` closure type-erased — concrete arms downcast
/// it before applying (T9 wires the closure plumbing; T2 only fills
/// the `HasOne` / `BelongsTo` arms which ignore the predicate for
/// now).
fn emit_dispatchers(input: &ModelInput) -> Result<TokenStream> {
    let struct_ident = &input.item.ident;

    // Collect per-kind match arms for the four dispatchers.
    let mut eager_arms: Vec<TokenStream> = Vec::new();
    let mut count_arms: Vec<TokenStream> = Vec::new();
    let mut aggregate_arms: Vec<TokenStream> = Vec::new();
    let mut recurse_arms: Vec<TokenStream> = Vec::new();
    for rel in input.relations.as_deref().unwrap_or(&[]) {
        if let Some(arm) = emit_eager_arm(input, rel)? {
            eager_arms.push(arm);
        }
        if let Some(arm) = emit_count_arm(input, rel)? {
            count_arms.push(arm);
        }
        if let Some(arm) = emit_aggregate_arm(input, rel)? {
            aggregate_arms.push(arm);
        }
        if let Some(arm) = emit_recurse_arm(input, rel)? {
            recurse_arms.push(arm);
        }
    }

    Ok(quote! {
        impl #struct_ident {
            /// Eager-load a relation by name. Called by `Builder::with`
            /// (T9) and `Collection::load_missing` (T9) to populate
            /// the per-row `__eager` cache. T1 emits the no-relation
            /// arm only; relation tasks (T2-T7) extend the match.
            ///
            /// The `predicate` carries a type-erased `with_where`
            /// closure — concrete arms downcast to the relation's
            /// `Box<dyn FnOnce(Builder<R>) -> Builder<R>>` and apply
            /// before issuing the IN query. T1 ignores it.
            #[doc(hidden)]
            pub async fn __eager_load(
                relation: &str,
                parents: &mut [&mut Self],
                db: &::suprnova::sea_orm::DatabaseConnection,
                predicate: ::core::option::Option<
                    ::std::boxed::Box<dyn ::std::any::Any + ::core::marker::Send + ::core::marker::Sync>,
                >,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                // Predicate ignored in T2 — `with_where` lands in T9.
                let _ = (db, predicate);
                match relation {
                    #(#eager_arms)*
                    other => ::core::result::Result::Err(
                        ::suprnova::FrameworkError::internal(::std::format!(
                            "model `{}` has no relation `{}`",
                            ::std::any::type_name::<Self>(),
                            other,
                        )),
                    ),
                }
            }

            /// Recurse into an already-loaded relation to load its own
            /// relations. Used by T9's nested-path eager loader
            /// (`with(["posts.comments"])`). T1 emits the skeleton;
            /// T2-T7 add arms; T9's orchestrator calls this after
            /// `__eager_load` for the head segment of a dotted path.
            #[doc(hidden)]
            pub async fn __recurse_eager_load(
                &mut self,
                relation: &str,
                rest: &str,
                db: &::suprnova::sea_orm::DatabaseConnection,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                let _ = (rest, db);
                match relation {
                    #(#recurse_arms)*
                    other => ::core::result::Result::Err(
                        ::suprnova::FrameworkError::internal(::std::format!(
                            "model `{}` has no relation `{}` to recurse into",
                            ::std::any::type_name::<Self>(),
                            other,
                        )),
                    ),
                }
            }

            /// Count rows for a relation (`with_count(["posts"])`).
            /// T1 emits the skeleton; T2-T7 add per-relation arms
            /// running GROUP BY queries.
            #[doc(hidden)]
            pub async fn __count_relation(
                relation: &str,
                parents: &mut [&mut Self],
                db: &::suprnova::sea_orm::DatabaseConnection,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                let _ = db;
                match relation {
                    #(#count_arms)*
                    other => ::core::result::Result::Err(
                        ::suprnova::FrameworkError::internal(::std::format!(
                            "model `{}` has no relation `{}` for with_count",
                            ::std::any::type_name::<Self>(),
                            other,
                        )),
                    ),
                }
            }

            /// Aggregate (SUM/AVG/MIN/MAX) over a relation column.
            /// Called by `with_sum(("posts", "views"))` and friends.
            /// T1 emits skeleton; T2-T7 add arms for the kinds that
            /// have a target column (HasMany / BelongsToMany /
            /// Through / Morph many-to-* — NOT HasOne / BelongsTo).
            #[doc(hidden)]
            pub async fn __aggregate_relation(
                relation: &str,
                column: &str,
                kind: ::suprnova::AggregateKind,
                parents: &mut [&mut Self],
                db: &::suprnova::sea_orm::DatabaseConnection,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                let _ = (column, kind, db);
                match relation {
                    #(#aggregate_arms)*
                    other => ::core::result::Result::Err(
                        ::suprnova::FrameworkError::internal(::std::format!(
                            "model `{}` has no relation `{}` for aggregate",
                            ::std::any::type_name::<Self>(),
                            other,
                        )),
                    ),
                }
            }
        }
    })
}

/// Emit the `pivot::<P>()` accessor. T4 (BelongsToMany) fills
/// `__pivot` on each row at load time; this accessor reads it back.
/// Panics when the row has no pivot context, matching the spec's
/// explicit "clear error message" requirement.
///
/// The accessor distinguishes the two failure modes:
///
/// - `__pivot` is `None` → the row was fetched without a m2m loader
///   (typically via `find()` instead of `BelongsToMany::get()`). The
///   panic message tells the caller to load through the m2m path.
/// - `__pivot` is `Some(_)` but the downcast to `P` fails → the data
///   is there but the caller asked for the wrong pivot type. The panic
///   message names the actual struct and the requested type so the
///   typo is obvious.
fn emit_pivot_accessor(struct_ident: &syn::Ident) -> TokenStream {
    quote! {
        impl #struct_ident {
            /// Read pivot context attached by a `BelongsToMany` load.
            ///
            /// Panics with one of two distinct messages depending on
            /// the failure mode:
            ///
            /// - If `__pivot` is empty, the row wasn't loaded through
            ///   the m2m path — call `BelongsToMany::get()` instead of
            ///   `find()`.
            /// - If `__pivot` is populated but the requested `P` type
            ///   doesn't match what was stored, the call site passed
            ///   the wrong pivot type — fix the turbofish.
            pub fn pivot<P: ::std::any::Any + ::core::marker::Send + ::core::marker::Sync>(&self) -> &P {
                match self.__pivot.as_ref() {
                    ::core::option::Option::None => ::std::panic!(
                        "`{}` row has no pivot context; load via `BelongsToMany::get()`",
                        ::std::any::type_name::<Self>(),
                    ),
                    ::core::option::Option::Some(arc) => match arc.downcast_ref::<P>() {
                        ::core::option::Option::Some(p) => p,
                        ::core::option::Option::None => ::std::panic!(
                            "`{}` row's pivot is not of type `{}` — pass the correct pivot type to `pivot::<P>()`",
                            ::std::any::type_name::<Self>(),
                            ::std::any::type_name::<P>(),
                        ),
                    },
                }
            }
        }
    }
}

/// Emit `<rel>_loaded()` and `<rel>_count()` accessors for one
/// relation. The return type of `<rel>_loaded()` depends on the
/// relation's kind:
///
/// - HasOne / BelongsTo / MorphTo / MorphOne / HasOneThrough →
///   `Option<&Target>` (the cache stores `Option<T>`)
/// - HasMany / BelongsToMany / HasManyThrough / MorphMany /
///   MorphToMany / MorphedByMany → `&[Target]`
///
/// `<rel>_count()` always returns `u64` and panics with a clear
/// message when `with_count(["..."])` wasn't called.
fn emit_relation_accessors(struct_ident: &syn::Ident, rel: &RelationDecl) -> TokenStream {
    let name = &rel.name;
    let name_str = name.to_string();
    let loaded_fn = quote::format_ident!("{}_loaded", name);
    let count_fn = quote::format_ident!("{}_count", name);
    // For Through kinds the parser stores generics left-to-right as
    // `(rel.target, rel.through)` where the first generic is the
    // intermediate B and the second is the final target C. The
    // accessor must surface the FINAL target so user code reads
    // `country.posts_loaded()` as `&[Post]` (not `&[User]`). For all
    // other kinds the parser-side `rel.target` IS the user-facing
    // target.
    let target_ty: &syn::Type = match rel.kind {
        RelationKindAttr::HasManyThrough | RelationKindAttr::HasOneThrough => {
            rel.through.as_ref().unwrap_or(&rel.target)
        }
        _ => &rel.target,
    };

    // The "loaded" accessor — kind-dependent return type.
    let loaded = match rel.kind {
        // Single-value kinds — read via get_one.
        RelationKindAttr::HasOne
        | RelationKindAttr::BelongsTo
        | RelationKindAttr::HasOneThrough
        | RelationKindAttr::MorphOne => quote! {
            #[doc = "Read the eager-loaded row for this relation."]
            #[doc = ""]
            #[doc = "Returns `None` if the relation was not eager-loaded \
                     (call `.with([\"...\"])` on the query builder) OR if \
                     the FK on the parent row was null."]
            pub fn #loaded_fn(&self) -> ::core::option::Option<&#target_ty> {
                self.__eager.get_one::<#target_ty>(#name_str)
            }
        },

        // MorphTo: target is `()` placeholder at T1; the per-family
        // enum lands in T6. The accessor returns the cached unit-typed
        // value via get_one; in T6 the codegen rewrites this to the
        // generated `<Name>Morph` enum type. For T1 it suffices to
        // emit a stub returning `Option<&()>` so the macro compiles
        // even when `MorphTo` is declared.
        RelationKindAttr::MorphTo => quote! {
            #[doc = "Read the eager-loaded `MorphTo` parent. T6 specialises this \
                     to the per-family `<Name>Morph` enum once the morph emitter lands."]
            pub fn #loaded_fn(&self) -> ::core::option::Option<&()> {
                self.__eager.get_one::<()>(#name_str)
            }
        },

        // Collection kinds — read via get_many; panics if not loaded.
        RelationKindAttr::HasMany
        | RelationKindAttr::BelongsToMany
        | RelationKindAttr::HasManyThrough
        | RelationKindAttr::MorphMany
        | RelationKindAttr::MorphToMany
        | RelationKindAttr::MorphedByMany => quote! {
            #[doc = "Read the eager-loaded rows for this relation."]
            #[doc = ""]
            #[doc = "Panics with a clear message if the relation was not \
                     eager-loaded — call `.with([\"...\"])` on the query \
                     builder before iterating."]
            pub fn #loaded_fn(&self) -> &[#target_ty] {
                self.__eager.get_many::<#target_ty>(#name_str)
            }
        },
    };

    quote! {
        impl #struct_ident {
            #loaded

            #[doc = "Read the `with_count(\"...\")` aggregate for this relation."]
            #[doc = ""]
            #[doc = "Panics with a clear message if `with_count` wasn't called \
                     for this relation — the spec requires loud failures over \
                     silent zeros."]
            pub fn #count_fn(&self) -> u64 {
                self.__eager
                    .get_count(#name_str)
                    .unwrap_or_else(|| ::std::panic!(
                        "`{}::{}` requires `with_count([\"{}\"])`",
                        ::std::any::type_name::<Self>(),
                        ::core::stringify!(#count_fn),
                        #name_str,
                    ))
            }
        }
    }
}

/// Emit the `inventory::submit!(RelationEntry { ... })` for one
/// declared relation. Phase 8 (Admin) walks this registry to
/// enumerate every relation in the binary. For `MorphTo` declarations
/// the target type is the unit type `()` — the per-family enum that
/// stands in as the "real" target is generated locally by T6.
fn emit_relation_inventory(struct_ident: &syn::Ident, rel: &RelationDecl) -> TokenStream {
    let name_str = rel.name.to_string();
    let parent_type_name = struct_ident.to_string();
    // Mirror `emit_relation_accessors`' kind-aware target choice so
    // Phase 8 admin renders the FINAL target (Post) for Through
    // relations rather than the intermediate (User).
    let target_ty: &syn::Type = match rel.kind {
        RelationKindAttr::HasManyThrough | RelationKindAttr::HasOneThrough => {
            rel.through.as_ref().unwrap_or(&rel.target)
        }
        _ => &rel.target,
    };
    let kind_variant = kind_to_runtime(rel.kind);
    // `RelationEntry::target_type_name` is `&'static str`, so we need
    // a string literal at macro expansion time — `type_name::<T>()`
    // isn't a `const fn` and can't be used in an `inventory::submit!`
    // constant initialiser. We render the `syn::Type` via
    // `TokenStream::to_string()` and strip the spaces that `quote`
    // inserts between tokens, so `Vec<Post>` is stored as
    // `"Vec<Post>"` (not `"Vec < Post >"`) and `Option<i64>` as
    // `"Option<i64>"` — Phase 8 admin renders this in the UI and
    // the padded form is visually wrong.
    let target_type_lit = format_target_type(target_ty);
    let target_type_name = match rel.kind {
        // MorphTo has no single concrete target; T6 emits the
        // per-family enum and overrides this entry. T1 stores
        // `"<morph>"` as a placeholder so admin tooling can render
        // something meaningful even before T6 lands.
        RelationKindAttr::MorphTo => "<morph>".to_string(),
        _ => target_type_lit,
    };

    quote! {
        ::suprnova::inventory::submit! {
            ::suprnova::RelationEntry {
                parent_type: ::std::any::TypeId::of::<#struct_ident>,
                target_type: ::std::any::TypeId::of::<#target_ty>,
                name: #name_str,
                kind: #kind_variant,
                parent_type_name: #parent_type_name,
                target_type_name: #target_type_name,
            }
        }
    }
}

/// Map the parse-time [`RelationKindAttr`] to the runtime
/// `::suprnova::RelationKind` enum value.
fn kind_to_runtime(kind: RelationKindAttr) -> TokenStream {
    match kind {
        RelationKindAttr::HasOne => quote! { ::suprnova::RelationKind::HasOne },
        RelationKindAttr::BelongsTo => quote! { ::suprnova::RelationKind::BelongsTo },
        RelationKindAttr::HasMany => quote! { ::suprnova::RelationKind::HasMany },
        RelationKindAttr::BelongsToMany => quote! { ::suprnova::RelationKind::BelongsToMany },
        RelationKindAttr::HasOneThrough => quote! { ::suprnova::RelationKind::HasOneThrough },
        RelationKindAttr::HasManyThrough => quote! { ::suprnova::RelationKind::HasManyThrough },
        RelationKindAttr::MorphTo => quote! { ::suprnova::RelationKind::MorphTo },
        RelationKindAttr::MorphOne => quote! { ::suprnova::RelationKind::MorphOne },
        RelationKindAttr::MorphMany => quote! { ::suprnova::RelationKind::MorphMany },
        RelationKindAttr::MorphToMany => quote! { ::suprnova::RelationKind::MorphToMany },
        RelationKindAttr::MorphedByMany => quote! { ::suprnova::RelationKind::MorphedByMany },
    }
}

// ---- T2: HasOne / BelongsTo emission helpers ----------------------------
//
// Each helper takes the model `input` plus the parsed `RelationDecl`
// and emits one chunk of code: the relation method, the
// `__eager_load` match arm, or the (currently empty) count /
// aggregate / recurse stubs. T3-T7 extend these with their own kinds;
// the dispatch is `match rel.kind { ... }` so each kind owns its
// own emission branch.

/// Default FK column name for a HasOne / HasMany relation on
/// parent `<P>`. Laravel convention: `<snake(P)>_id`. Override via
/// the inline `fk = "..."` option on the relation declaration.
fn default_has_fk(parent_struct_name: &str) -> String {
    format!("{}_id", to_snake(parent_struct_name))
}

/// Default FK column name for a BelongsTo on child `<C>` pointing at
/// parent `<P>`. Laravel convention: `<snake(target_type)>_id`. The
/// `target_type` is the `<P>` in `BelongsTo<P>`. Override via inline
/// `fk = "..."`.
fn default_belongs_to_fk(target_ty: &syn::Type) -> String {
    // Extract the last path segment as a string — covers
    // `Post`, `crate::models::Post`, `super::Post`. Falls back to
    // formatting the whole type if the path is empty.
    let target_name = match target_ty {
        syn::Type::Path(p) => p
            .path
            .segments
            .last()
            .map(|seg| seg.ident.to_string())
            .unwrap_or_else(|| quote::quote!(#target_ty).to_string()),
        _ => quote::quote!(#target_ty).to_string(),
    };
    format!("{}_id", to_snake(&target_name))
}

/// Look up the user-declared `fk = "..."` override on a relation
/// declaration. `None` when the user didn't override.
fn fk_override(rel: &RelationDecl) -> Option<&str> {
    rel.options.iter().find_map(|o| match o {
        RelationOpt::ForeignKey(s) => Some(s.as_str()),
        _ => None,
    })
}

/// Look up the user-declared `lk = "..."` override.
fn lk_override(rel: &RelationDecl) -> Option<&str> {
    rel.options.iter().find_map(|o| match o {
        RelationOpt::LocalKey(s) => Some(s.as_str()),
        _ => None,
    })
}

/// Look up the user-declared `with_default = || ...` closure on a
/// BelongsTo relation. Returns the parsed expression; emission wraps
/// it in `.with_default(<expr>)` at the call site.
fn with_default_expr(rel: &RelationDecl) -> Option<&syn::Expr> {
    rel.options.iter().find_map(|o| match o {
        RelationOpt::WithDefault(e) => Some(e),
        _ => None,
    })
}

/// Look up the user-declared `pivot_table = "..."` override.
/// Returns `None` when the user relies on the pivot type's own
/// `EloquentModel::TABLE` const (the recommended path).
fn pivot_table_override(rel: &RelationDecl) -> Option<&str> {
    rel.options.iter().find_map(|o| match o {
        RelationOpt::PivotTable(s) => Some(s.as_str()),
        _ => None,
    })
}

/// Look up the user-declared `pivot_foreign_key = "..."` override.
fn pivot_fk_override(rel: &RelationDecl) -> Option<&str> {
    rel.options.iter().find_map(|o| match o {
        RelationOpt::PivotForeignKey(s) => Some(s.as_str()),
        _ => None,
    })
}

/// Look up the user-declared `pivot_related_key = "..."` override.
fn pivot_related_override(rel: &RelationDecl) -> Option<&str> {
    rel.options.iter().find_map(|o| match o {
        RelationOpt::PivotRelatedKey(s) => Some(s.as_str()),
        _ => None,
    })
}

/// Look up the user-declared `related_key = "..."` override — the
/// related-side primary-key COLUMN name used by `BelongsToMany`'s
/// `.get()` IN-filter and the aggregate JOIN. Defaults to `"id"` when
/// omitted (matches SeaORM convention).
fn related_key_override(rel: &RelationDecl) -> Option<&str> {
    rel.options.iter().find_map(|o| match o {
        RelationOpt::RelatedKey(s) => Some(s.as_str()),
        _ => None,
    })
}

/// Look up `with_pivot = ["col1", ...]` extra columns. Returns an
/// empty slice when omitted.
fn with_pivot_cols(rel: &RelationDecl) -> &[String] {
    for o in &rel.options {
        if let RelationOpt::WithPivot(cols) = o {
            return cols.as_slice();
        }
    }
    &[]
}

/// Look up the user-declared `first_key = "..."` override for
/// `HasOneThrough` / `HasManyThrough` — the column on the intermediate
/// `B` table that points at the parent `A`. Default:
/// `<snake(parent_struct)>_id`.
fn first_key_override(rel: &RelationDecl) -> Option<&str> {
    rel.options.iter().find_map(|o| match o {
        RelationOpt::FirstKey(s) => Some(s.as_str()),
        _ => None,
    })
}

/// Look up the user-declared `second_key = "..."` override for
/// `HasOneThrough` / `HasManyThrough` — the column on the target `C`
/// table that points at the intermediate `B`. Default:
/// `<snake(through_type)>_id`.
fn second_key_override(rel: &RelationDecl) -> Option<&str> {
    rel.options.iter().find_map(|o| match o {
        RelationOpt::SecondKey(s) => Some(s.as_str()),
        _ => None,
    })
}

/// Look up the user-declared `second_local_key = "..."` override for
/// `HasOneThrough` / `HasManyThrough` — the column on the intermediate
/// `B` matched by `second_key`. Defaults to `"id"`. Required when the
/// intermediate model declares `#[model(primary_key = "...")]` with a
/// non-`id` PK.
fn second_local_key_override(rel: &RelationDecl) -> Option<&str> {
    rel.options.iter().find_map(|o| match o {
        RelationOpt::SecondLocalKey(s) => Some(s.as_str()),
        _ => None,
    })
}

/// True when `with_timestamps` (bare flag or `= true`) is declared.
fn with_timestamps_flag(rel: &RelationDecl) -> bool {
    rel.options
        .iter()
        .any(|o| matches!(o, RelationOpt::WithTimestamps))
}

/// Look up the user-declared `name = "..."` morph-family override.
/// Defaults to the relation name itself when omitted — e.g. a relation
/// declared as `commentable: MorphTo { targets = [...] }` derives a
/// morph-name of `"commentable"` without needing the redundant
/// `name = "commentable"` option.
fn morph_name_or_default(rel: &RelationDecl) -> String {
    rel.options
        .iter()
        .find_map(|o| match o {
            RelationOpt::MorphName(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_else(|| rel.name.to_string())
}

/// Look up the user-declared `targets = [...]` list on a `MorphTo`
/// declaration. The parser guarantees this option is present for
/// `MorphTo` declarations (see `parse_one_relation`); the `expect`
/// / `ok_or_else` callers handle the unreachable case.
fn morph_targets(rel: &RelationDecl) -> Option<&[syn::Type]> {
    rel.options.iter().find_map(|o| match o {
        RelationOpt::MorphTargets(types) => Some(types.as_slice()),
        _ => None,
    })
}

/// The morph-type string a model registers under. Read from the
/// model's `morph_type = "..."` attribute when present; defaults to
/// `to_snake(struct_name)` otherwise (Laravel convention — `Post`
/// becomes `"post"`).
///
/// This is the string the parent puts into the child's
/// `<morph_name>_type` column at insert time, and the string the
/// `MorphMany` / `MorphOne` runtime uses to filter the child table
/// by. Per the brief: T8's morph registry adds a runtime warn-log
/// cross-check; T6 trusts the per-model attribute + Laravel default.
fn morph_type_of(input: &ModelInput) -> String {
    input
        .morph_type
        .clone()
        .unwrap_or_else(|| to_snake(&input.item.ident.to_string()))
}

/// Generate the set of candidate morph-type match keys for one target
/// in a `MorphTo`'s per-family fetch helper. Since the macro for the
/// `MorphTo` declaration site can't introspect another struct's
/// `morph_type = "..."` attribute (it lives in a separate macro
/// expansion), we emit ALL plausible match keys so a target declared
/// with the default OR with one of the obvious shortenings still
/// dispatches.
///
/// For a target named `MorphPost`, this yields:
/// - `"morph_post"` — `to_snake(TargetTypeName)` (the macro default)
/// - `"morphpost"` — no-underscore form
/// - `"post"` — Laravel convention (struct name minus a `Morph`
///   prefix when one exists; if there's no obvious prefix this falls
///   through to the snake form, deduped via `sort + dedup`).
///
/// For a target named `Post` (no prefix), the result collapses to
/// `["post"]` — `to_snake`, no-underscore, and the no-prefix branch
/// all produce the same string.
///
/// T8's registry adds a runtime warn-log if a target's actual stored
/// `morph_type` value doesn't match any of these candidates.
///
/// Exposed to `parse.rs` so the parser can detect overlapping dispatch
/// keys across a `MorphTo`'s declared targets at declaration time
/// (e.g. `targets = [MorphPost, Post]` — both produce `"post"` and
/// the second match arm would be unreachable, with stored `"post"`
/// silently dispatching to `MorphPost`).
pub(super) fn morph_target_keys(ty: &syn::Type) -> Vec<String> {
    let name = match ty {
        syn::Type::Path(p) => p
            .path
            .segments
            .last()
            .map(|seg| seg.ident.to_string())
            .unwrap_or_else(|| quote::quote!(#ty).to_string()),
        _ => quote::quote!(#ty).to_string(),
    };
    let snake = to_snake(&name);
    let no_underscore = snake.replace('_', "");
    // Laravel convention: strip a leading `Morph` prefix when present
    // so `MorphPost` → `"post"`. Falls back to the snake form when no
    // prefix is found.
    let stripped = if let Some(rest) = name.strip_prefix("Morph") {
        if !rest.is_empty() {
            to_snake(rest)
        } else {
            snake.clone()
        }
    } else {
        snake.clone()
    };
    let mut out = vec![snake.clone(), no_underscore, stripped];
    out.sort();
    out.dedup();
    out
}

/// Extract the last path segment of a type ident, e.g. `Post` from
/// `crate::models::Post`. Used for default pivot-key derivation
/// (`<snake(name)>_id`) when the user omits `pivot_foreign_key`
/// / `pivot_related_key`. Falls back to the full token stream
/// rendering when the type isn't a path (rare; mostly defensive).
fn last_segment_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(p) => p
            .path
            .segments
            .last()
            .map(|seg| seg.ident.to_string())
            .unwrap_or_else(|| quote::quote!(#ty).to_string()),
        _ => quote::quote!(#ty).to_string(),
    }
}

/// Whether the named field on the user struct has type `Option<T>`.
/// Used by BelongsTo emission to decide between
/// `Some(serde_json::to_value(&self.<fk>).ok()?)` (non-Option) and
/// `self.<fk>.as_ref().map(|v| serde_json::to_value(v).ok()).flatten()`
/// (Option). Looks at the last path segment of the field type — same
/// shape as `classify_datetime` in `parse.rs`.
fn field_is_optional(input: &ModelInput, field_name: &str) -> bool {
    let fields = match &input.item.fields {
        syn::Fields::Named(named) => &named.named,
        _ => return false,
    };
    for f in fields {
        let ident = match f.ident.as_ref() {
            Some(i) => i,
            None => continue,
        };
        if ident == field_name {
            return matches!(
                &f.ty,
                syn::Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Option")
            );
        }
    }
    false
}

/// Emit the relation method (`fn profile(&self) -> HasOne<Self, Profile>`)
/// per declared HasOne / BelongsTo. Other kinds will land in T3-T7;
/// T2 returns an empty stream for them so the macro compiles for
/// users who declared a kind T2 doesn't own yet (e.g. T1's smoke
/// tests with `relations = {}`).
fn emit_relation_method(input: &ModelInput, rel: &RelationDecl) -> Result<TokenStream> {
    let struct_ident = &input.item.ident;
    let parent_name = struct_ident.to_string();
    let pk_name = &input.primary_key;
    let pk_ident = quote::format_ident!("{pk_name}");
    let method_ident = &rel.name;
    let target_ty = &rel.target;

    match rel.kind {
        RelationKindAttr::HasOne => {
            // FK on the child = <snake(parent_struct)>_id by default.
            // LK on the parent = the parent's PK by default ("id").
            let fk = fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| default_has_fk(&parent_name));
            let lk = lk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| pk_name.clone());

            Ok(quote! {
                impl #struct_ident {
                    #[doc = "Construct a `HasOne` relation builder for this row."]
                    #[doc = ""]
                    #[doc = "Chainable — `user.profile().filter(...).first().await?`."]
                    pub fn #method_ident(&self) -> ::suprnova::HasOne<Self, #target_ty> {
                        let parent_value = ::suprnova::serde_json::to_value(&self.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        ::suprnova::HasOne::<Self, #target_ty>::__new(
                            parent_value,
                            ::std::string::String::from(#fk),
                            ::std::string::String::from(#lk),
                        )
                    }
                }
            })
        }
        RelationKindAttr::BelongsTo => {
            // FK on this child row = <snake(target)>_id by default.
            // The child's FK column on `self` is what the macro reads
            // to build the lookup; the resulting field access is
            // `self.<fk_ident>` (e.g. `self.user_id`).
            let fk = fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| default_belongs_to_fk(target_ty));
            // owner key on parent = parent's PK by default ("id").
            // T2: BelongsTo's parent PK isn't introspectable from this
            // macro (the parent struct lives in a different `#[model]`
            // invocation), so we default to "id" + honour an explicit
            // `lk = "..."` override.
            let owner_key = lk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "id".to_string());

            let fk_ident = quote::format_ident!("{}", fk);
            // Inspect the FK field type on the child struct. If
            // `Option<T>`, emit a flat_map over `.as_ref()`; otherwise
            // emit `Some(serde_json::to_value(&self.<fk>)...)`.
            let parent_value_expr = if field_is_optional(input, &fk) {
                quote! {
                    self.#fk_ident
                        .as_ref()
                        .and_then(|v| ::suprnova::serde_json::to_value(v).ok())
                }
            } else {
                quote! {
                    ::core::option::Option::Some(
                        ::suprnova::serde_json::to_value(&self.#fk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null)
                    )
                }
            };

            let with_default_chain = match with_default_expr(rel) {
                Some(expr) => quote! { .with_default(#expr) },
                None => quote! {},
            };

            Ok(quote! {
                impl #struct_ident {
                    #[doc = "Construct a `BelongsTo` relation lookup for this row."]
                    #[doc = ""]
                    #[doc = "Looks up the parent identified by this row's foreign-key \
                             column. Honours `with_default(closure)` declared inline \
                             on the relation."]
                    pub fn #method_ident(&self) -> ::suprnova::BelongsTo<Self, #target_ty> {
                        let parent_value: ::core::option::Option<::suprnova::serde_json::Value>
                            = #parent_value_expr;
                        ::suprnova::BelongsTo::<Self, #target_ty>::__new(
                            parent_value,
                            ::std::string::String::from(#fk),
                            ::std::string::String::from(#owner_key),
                        )#with_default_chain
                    }
                }
            })
        }
        RelationKindAttr::HasMany => {
            // FK on the child table = <snake(parent_struct)>_id by
            // default — same default as HasOne. LK = parent's PK by
            // default ("id"), configurable via `lk = "..."`.
            let fk = fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| default_has_fk(&parent_name));
            let lk = lk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| pk_name.clone());

            Ok(quote! {
                impl #struct_ident {
                    #[doc = "Construct a `HasMany` relation builder for this row."]
                    #[doc = ""]
                    #[doc = "Chainable — `user.posts().latest().take(5).get().await?`."]
                    pub fn #method_ident(&self) -> ::suprnova::HasMany<Self, #target_ty> {
                        let parent_value = ::suprnova::serde_json::to_value(&self.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        ::suprnova::HasMany::<Self, #target_ty>::__new(
                            parent_value,
                            ::std::string::String::from(#fk),
                            ::std::string::String::from(#lk),
                        )
                    }
                }
            })
        }
        RelationKindAttr::BelongsToMany => {
            // Pivot model — the user wrote `BelongsToMany<R, P>`,
            // parsed into `rel.through`. The parser already validates
            // that BelongsToMany requires a second generic argument,
            // so the `expect` is unreachable on the happy path.
            let pivot_ty = rel.through.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(
                    method_ident,
                    "BelongsToMany requires a pivot type (see parse-time validation)",
                )
            })?;

            // pivot_foreign_key default: <snake(parent_struct)>_id.
            let pivot_fk = pivot_fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}_id", to_snake(&parent_name)));
            // pivot_related_key default: <snake(target_struct_name)>_id.
            let pivot_related = pivot_related_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    format!("{}_id", to_snake(&last_segment_name(target_ty)))
                });
            // pivot_table: either the user-supplied literal, or — at
            // runtime — `<P as EloquentModel>::TABLE` so the pivot
            // struct's own `#[suprnova::model(table = "...")]` declaration
            // is the single source of truth.
            let pivot_table_expr: TokenStream = match pivot_table_override(rel) {
                Some(t) => quote! { ::std::string::String::from(#t) },
                None => quote! {
                    <#pivot_ty as ::suprnova::eloquent::EloquentModel>::TABLE.to_string()
                },
            };
            // Local key (parent's PK column name). Defaults to the
            // model's declared primary_key.
            let lk = lk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| pk_name.clone());

            // `with_pivot([...])` and `.with_timestamps()` chain calls.
            let pivot_extras = with_pivot_cols(rel);
            let with_pivot_chain = if pivot_extras.is_empty() {
                quote! {}
            } else {
                let lits = pivot_extras
                    .iter()
                    .map(|c| quote! { #c })
                    .collect::<Vec<_>>();
                quote! { .with_pivot(::std::vec![#(#lits),*]) }
            };
            let with_timestamps_chain = if with_timestamps_flag(rel) {
                quote! { .with_timestamps() }
            } else {
                quote! {}
            };
            let local_key_chain = if lk == "id" {
                quote! {}
            } else {
                quote! { .local_key(#lk) }
            };
            // Related-side PK column. Defaults to `"id"`. Chained as
            // `.related_pk(#rk)` so the runtime IN-filter (`.get()`)
            // and aggregate JOIN read the correct column when the
            // related model declares a non-`id` primary key.
            let related_key_chain = match related_key_override(rel) {
                Some(rk) if rk != "id" => quote! { .related_pk(#rk) },
                _ => quote! {},
            };

            Ok(quote! {
                impl #struct_ident {
                    #[doc = "Construct a `BelongsToMany` relation for this row."]
                    #[doc = ""]
                    #[doc = "Use `.attach(id)` / `.detach(id)` / `.sync([...])` to \
                             mutate the pivot, `.get()` to load related rows with \
                             pivot context."]
                    pub fn #method_ident(&self) -> ::suprnova::BelongsToMany<Self, #target_ty, #pivot_ty> {
                        let parent_value = ::suprnova::serde_json::to_value(&self.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        ::suprnova::BelongsToMany::<Self, #target_ty, #pivot_ty>::__new(
                            parent_value,
                            #pivot_table_expr,
                            ::std::string::String::from(#pivot_fk),
                            ::std::string::String::from(#pivot_related),
                        )
                        #local_key_chain
                        #related_key_chain
                        #with_pivot_chain
                        #with_timestamps_chain
                    }
                }
            })
        }
        RelationKindAttr::HasManyThrough | RelationKindAttr::HasOneThrough => {
            // The user wrote `HasManyThrough<B, C>` where `B` is the
            // intermediate model and `C` is the final target. The
            // parser stores generics left-to-right as
            // `(rel.target, rel.through)` — so for Through kinds, the
            // semantic mapping is:
            //
            //   rel.target  = first generic = B (intermediate)
            //   rel.through = second generic = C (final target)
            //
            // This is intentionally different from `BelongsToMany<R, P>`
            // where `rel.target = R` (final) and `rel.through = P`
            // (pivot). Through relations declare the chain in traversal
            // order; m2m declares target-then-pivot. The macro absorbs
            // the inconsistency so user code reads naturally for each
            // kind.
            let through_ty = &rel.target; // intermediate B
            let final_target_ty = rel.through.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(
                    method_ident,
                    "HasOneThrough / HasManyThrough require a final target type, e.g. \
                     `HasManyThrough<Intermediate, Target>` (parser bug if reached)",
                )
            })?; // final target C

            // first_key default: <snake(parent_struct)>_id.
            let first_key = first_key_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}_id", to_snake(&parent_name)));
            // second_key default: <snake(last_segment(through_ty))>_id
            // — column on the FINAL target table pointing at the
            // intermediate.
            let second_key = second_key_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    format!("{}_id", to_snake(&last_segment_name(through_ty)))
                });
            // Local key (parent's PK column name). Defaults to the
            // model's declared primary_key. Honoured via the runtime
            // `.local_key(...)` setter so the metadata stays on the
            // Relation impl.
            let lk = lk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| pk_name.clone());
            let local_key_chain = if lk == "id" {
                quote! {}
            } else {
                quote! { .local_key(#lk) }
            };
            // Second local key — column on the intermediate `B`
            // matched by `second_key`. Defaults to `"id"`. Chained as
            // `.second_local_key(...)` so the runtime JOIN reads the
            // right column for intermediates declaring a non-`id` PK.
            let second_local_key = second_local_key_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "id".to_string());
            let second_local_key_chain = if second_local_key == "id" {
                quote! {}
            } else {
                quote! { .second_local_key(#second_local_key) }
            };

            // Pick the runtime struct name based on the kind. Both
            // wrappers share the same `__new` shape.
            let wrapper = match rel.kind {
                RelationKindAttr::HasManyThrough => quote! { HasManyThrough },
                RelationKindAttr::HasOneThrough => quote! { HasOneThrough },
                _ => unreachable!("guarded by outer match arm"),
            };
            let doc_kind = match rel.kind {
                RelationKindAttr::HasManyThrough => "HasManyThrough",
                RelationKindAttr::HasOneThrough => "HasOneThrough",
                _ => unreachable!("guarded by outer match arm"),
            };
            let doc_str = format!("Construct a `{doc_kind}` relation for this row.");

            Ok(quote! {
                impl #struct_ident {
                    #[doc = #doc_str]
                    #[doc = ""]
                    #[doc = "Two-hop traversal via the intermediate model — \
                             `.get()` issues a single `INNER JOIN` query."]
                    pub fn #method_ident(&self) -> ::suprnova::#wrapper<Self, #through_ty, #final_target_ty> {
                        let parent_value = ::suprnova::serde_json::to_value(&self.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        ::suprnova::#wrapper::<Self, #through_ty, #final_target_ty>::__new(
                            parent_value,
                            ::std::string::String::from(#first_key),
                            ::std::string::String::from(#second_key),
                        )
                        #local_key_chain
                        #second_local_key_chain
                    }
                }
            })
        }
        RelationKindAttr::MorphMany | RelationKindAttr::MorphOne => {
            // Polymorphic one-to-many / one-to-one. The relation
            // declaration lives on the PARENT side (e.g.
            // `comments: MorphMany<Comment> { name = "commentable" }`
            // on Post + Video). The macro emits a method that returns
            // a runtime `MorphMany<Self, Comment>` (or `MorphOne<...>`)
            // pre-filtered with both `commentable_id = self.id` and
            // `commentable_type = "post"`.
            //
            // Two pieces of metadata flow from the model attributes:
            //
            // 1. `morph_name` — controls the `<name>_id` /
            //    `<name>_type` column names on the child table.
            //    Defaults to the relation name itself.
            //
            // 2. `morph_type_value` — the parent's `morph_type = "..."`
            //    attribute (defaulted to `to_snake(struct_name)`).
            //    This is the string the child's `*_type` column has
            //    to equal for the row to belong to this parent.
            let morph_name = morph_name_or_default(rel);
            let morph_type_value = morph_type_of(input);
            let wrapper = match rel.kind {
                RelationKindAttr::MorphMany => quote! { MorphMany },
                RelationKindAttr::MorphOne => quote! { MorphOne },
                _ => unreachable!("guarded by outer match arm"),
            };
            let doc_kind = match rel.kind {
                RelationKindAttr::MorphMany => "MorphMany",
                RelationKindAttr::MorphOne => "MorphOne",
                _ => unreachable!("guarded by outer match arm"),
            };
            let doc_str = format!(
                "Construct a `{doc_kind}` polymorphic relation builder for this row."
            );
            Ok(quote! {
                impl #struct_ident {
                    #[doc = #doc_str]
                    #[doc = ""]
                    #[doc = "Chainable — both `<morph_name>_id` and `<morph_name>_type` \
                             predicates are pre-applied. Children pointing at OTHER \
                             parents (different `*_type` values) never appear in \
                             results."]
                    pub fn #method_ident(&self) -> ::suprnova::#wrapper<Self, #target_ty> {
                        let parent_value = ::suprnova::serde_json::to_value(&self.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        ::suprnova::#wrapper::<Self, #target_ty>::__new(
                            parent_value,
                            ::std::string::String::from(#morph_name),
                            ::std::string::String::from(#morph_type_value),
                        )
                    }
                }
            })
        }
        RelationKindAttr::MorphTo => {
            // `MorphTo` is the inverse side — the user declared
            // `commentable: MorphTo { name = "commentable",
            //  targets = [MorphPost, MorphVideo] }` on the morph-table
            // model (Comment). The macro emits THREE things at this
            // declaration site:
            //
            // 1. A per-family enum `<Name>Morph` with one variant per
            //    target + `Unknown(String, i64)` for legacy rows.
            // 2. A per-family fetch helper `<Name>MorphFetch` carrying
            //    the FK + type-string, with a `.get()` method that
            //    matches the type-string against per-target candidate
            //    keys and returns the per-family enum.
            // 3. An inherent method on the morph-table model that
            //    constructs the fetch helper from the row's
            //    `<name>_id` + `<name>_type` columns.
            //
            // The user's call site reads:
            //
            //     match comment.commentable().get().await? {
            //         CommentableMorph::MorphPost(p) => ...,
            //         CommentableMorph::MorphVideo(v) => ...,
            //         CommentableMorph::Unknown(t, id) => ...,
            //     }
            //
            // No runtime `MorphTo<C>` instance is built at the call
            // site — `MorphTo<C>` is purely metadata for the relation
            // registry + a re-export users can name in turbofish.
            let morph_name = morph_name_or_default(rel);
            let id_field = quote::format_ident!("{morph_name}_id");
            let type_field = quote::format_ident!("{morph_name}_type");
            // Enum + fetch struct names — `commentable` →
            // `CommentableMorph` / `CommentableMorphFetch`.
            let enum_ident = {
                let s = rel.name.to_string();
                let mut chars = s.chars();
                let first = chars
                    .next()
                    .map(|c| c.to_ascii_uppercase().to_string())
                    .unwrap_or_default();
                // Strip underscores + capitalise each segment so
                // `something_polymorphic` becomes
                // `SomethingPolymorphicMorph`.
                let mut camel = String::with_capacity(s.len());
                camel.push_str(&first);
                let mut upper_next = false;
                for c in chars {
                    if c == '_' {
                        upper_next = true;
                    } else if upper_next {
                        camel.push(c.to_ascii_uppercase());
                        upper_next = false;
                    } else {
                        camel.push(c);
                    }
                }
                quote::format_ident!("{camel}Morph")
            };
            let fetch_ident = quote::format_ident!("{enum_ident}Fetch");

            let targets = morph_targets(rel).ok_or_else(|| {
                syn::Error::new_spanned(
                    method_ident,
                    "MorphTo requires `targets = [...]` (parser bug if reached)",
                )
            })?;

            // Variant idents = the target's last path segment (e.g.
            // `MorphPost` from `crate::models::MorphPost`). Mechanically
            // required — enum variants name the user type, not a
            // generic placeholder.
            let variant_idents: Vec<syn::Ident> = targets
                .iter()
                .map(|ty| {
                    let name = match ty {
                        syn::Type::Path(p) => p
                            .path
                            .segments
                            .last()
                            .map(|seg| seg.ident.to_string())
                            .unwrap_or_else(|| quote::quote!(#ty).to_string()),
                        _ => quote::quote!(#ty).to_string(),
                    };
                    quote::format_ident!("{name}")
                })
                .collect();

            // Per-target match arms inside `<Name>MorphFetch::get()`.
            // Each arm enumerates all plausible morph-type keys for
            // the target (snake / no-underscore / Laravel short form)
            // and on a hit, calls `Target::find(id)` through the
            // standard Eloquent CRUD path. Misses (None) and unknown
            // type-strings both fall through to the `Unknown` variant.
            let mut fetch_arms: Vec<TokenStream> = Vec::with_capacity(targets.len());
            for (ty, variant) in targets.iter().zip(variant_idents.iter()) {
                let keys = morph_target_keys(ty);
                let key_lits = keys
                    .iter()
                    .map(|k| quote! { #k })
                    .collect::<Vec<_>>();
                fetch_arms.push(quote! {
                    #( #key_lits )|* => {
                        let row: ::core::option::Option<#ty> =
                            <#ty as ::suprnova::eloquent::Model>::find(self.morph_id).await?;
                        match row {
                            ::core::option::Option::Some(r) => {
                                ::core::result::Result::Ok(#enum_ident::#variant(r))
                            }
                            ::core::option::Option::None => {
                                ::core::result::Result::Ok(#enum_ident::Unknown(
                                    self.morph_type,
                                    self.morph_id,
                                ))
                            }
                        }
                    }
                });
            }

            Ok(quote! {
                /// Per-family morph enum generated by the
                /// `#[suprnova::model(relations = { ...: MorphTo {
                /// targets = [...] } })]` declaration on the
                /// morph-table struct. One variant per declared target
                /// + `Unknown(type_string, id)` for legacy rows whose
                /// `<name>_type` column doesn't match any registered
                /// target.
                #[derive(::std::fmt::Debug, ::core::clone::Clone)]
                pub enum #enum_ident {
                    #(
                        #variant_idents(#targets),
                    )*
                    /// Row's `<morph_name>_type` column didn't match any
                    /// registered target. Carries the unmatched type
                    /// string + the FK value so callers can log or
                    /// migrate the stale data.
                    Unknown(::std::string::String, i64),
                }

                /// Fetch helper that dispatches into the per-family
                /// enum. Built by the morph-table model's
                /// `<rel>()` method; calling `.get().await?` resolves
                /// the parent row via the standard Eloquent CRUD path.
                pub struct #fetch_ident {
                    morph_id: i64,
                    morph_type: ::std::string::String,
                }

                impl #fetch_ident {
                    /// Resolve the polymorphic parent. Returns the
                    /// per-family enum's `Unknown` variant when the
                    /// `<name>_type` column doesn't match any declared
                    /// target OR when the looked-up row is absent
                    /// (legacy / soft-deleted / renamed model).
                    pub async fn get(
                        self,
                    ) -> ::core::result::Result<#enum_ident, ::suprnova::FrameworkError> {
                        match self.morph_type.as_str() {
                            #(#fetch_arms)*
                            _ => ::core::result::Result::Ok(#enum_ident::Unknown(
                                self.morph_type,
                                self.morph_id,
                            )),
                        }
                    }
                }

                impl #struct_ident {
                    #[doc = "Construct a `MorphTo` fetch helper for this row."]
                    #[doc = ""]
                    #[doc = "Resolves the polymorphic parent via the row's \
                             `<morph_name>_id` + `<morph_name>_type` columns. \
                             Awaiting `.get()` returns the per-family enum with \
                             one variant per declared target."]
                    pub fn #method_ident(&self) -> #fetch_ident {
                        #fetch_ident {
                            morph_id: self.#id_field,
                            morph_type: self.#type_field.clone(),
                        }
                    }
                }
            })
        }
        // T7 owns the remaining morph m2m kinds. We emit nothing for
        // them so the macro still accepts the declaration.
        _ => Ok(TokenStream::new()),
    }
}

/// Emit a `<name> => { ... }` arm for `__eager_load`. T2 owns HasOne
/// and BelongsTo; other kinds return `None` (no arm).
fn emit_eager_arm(input: &ModelInput, rel: &RelationDecl) -> Result<Option<TokenStream>> {
    let struct_ident = &input.item.ident;
    let name_str = rel.name.to_string();
    let pk_name = &input.primary_key;
    let pk_ident = quote::format_ident!("{pk_name}");
    let target_ty = &rel.target;
    let parent_name = struct_ident.to_string();

    match rel.kind {
        RelationKindAttr::HasOne => {
            // FK column on the child table.
            let fk = fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| default_has_fk(&parent_name));

            // Build a JSON Vec of parent PK values, issue an
            // `IN (...)` against the child table, group by FK on each
            // returned row, and stuff into each parent's `__eager`.
            //
            // The FK is read off the target row via
            // `serde_json::to_value(&r).get(#fk)` rather than
            // `r.<fk_ident>` field access. The field-access form
            // would force the macro to assume the target struct
            // declared a field by exactly that ident, which it can't
            // verify (the target's `#[model]` invocation is a
            // separate macro expansion). JSON-pluck works uniformly
            // for any field name the user wrote on the target.
            //
            // PK values use `serde_json::to_value(&p.<pk>)`
            // serialisation as `HashMap` keys so the lookup is total
            // across PK shapes (i64 / String / Uuid-via-string).
            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }
                    let pk_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();
                    let rows: ::std::vec::Vec<#target_ty> =
                        <#target_ty as ::suprnova::eloquent::Model>::query()
                            .filter_in(#fk, pk_values)
                            .get()
                            .await?;
                    use ::std::collections::HashMap;
                    let mut by_fk: HashMap<::std::string::String, #target_ty> = HashMap::new();
                    for r in rows.into_iter() {
                        let row_json = ::suprnova::serde_json::to_value(&r)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        let key = row_json
                            .get(#fk)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        by_fk.insert(key, r);
                    }
                    for p in parents.iter_mut() {
                        let key = ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        let row = by_fk.remove(&key);
                        p.__eager.set_one::<#target_ty>(#name_str, row);
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::BelongsTo => {
            // FK on the child = <snake(target)>_id by default.
            let fk = fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| default_belongs_to_fk(target_ty));
            let fk_ident = quote::format_ident!("{}", fk);
            // Owner key on the parent.
            let owner_key = lk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "id".to_string());
            let fk_is_optional = field_is_optional(input, &fk);
            let with_default_chain = match with_default_expr(rel) {
                Some(expr) => quote! { .with_default(#expr) },
                None => quote! {},
            };

            // For Option<T> FKs the per-row JSON extraction unwraps
            // the inner value; for non-Option FKs it's always present.
            let per_parent_key_expr = if fk_is_optional {
                quote! {
                    p.#fk_ident
                        .as_ref()
                        .and_then(|v| ::suprnova::serde_json::to_value(v).ok())
                }
            } else {
                quote! {
                    ::core::option::Option::Some(
                        ::suprnova::serde_json::to_value(&p.#fk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null),
                    )
                }
            };

            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }
                    // Distinct FK values to query (skip null FKs).
                    let fk_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .filter_map(|p| {
                            let v: ::core::option::Option<::suprnova::serde_json::Value> =
                                #per_parent_key_expr;
                            v
                        })
                        .collect();
                    let parent_rows: ::std::vec::Vec<#target_ty> = if fk_values.is_empty() {
                        ::std::vec::Vec::new()
                    } else {
                        <#target_ty as ::suprnova::eloquent::Model>::query()
                            .filter_in(#owner_key, fk_values)
                            .get()
                            .await?
                    };
                    use ::std::collections::HashMap;
                    // Group parents by their PK (which is matched by
                    // the BelongsTo's owner_key) as JSON-encoded string.
                    // The target's owner-key column resolution at
                    // emission time uses the primary_key field name
                    // unless the user overrode `lk = "..."`. T2 names
                    // the parent's PK field via `<owner_key>` directly
                    // as an ident, which assumes the parent struct
                    // declared a field by that name. Models with a
                    // non-`id` PK can use `lk = "<pk>"` to align.
                    let mut by_pk: HashMap<::std::string::String, #target_ty> = HashMap::new();
                    for row in parent_rows.into_iter() {
                        // The owner-key column is read out of the parent
                        // target by serialising the whole row to JSON
                        // and plucking the key — works uniformly for
                        // any field name the user wrote, without
                        // requiring the macro here to know the parent
                        // struct's field layout.
                        let row_json = ::suprnova::serde_json::to_value(&row)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        let key = row_json
                            .get(#owner_key)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        by_pk.insert(key, row);
                    }
                    // Per parent row: look up the parent by FK; if
                    // missing OR FK was null, invoke the
                    // `with_default` closure (if installed). The
                    // lookup is `.get().cloned()` rather than
                    // `.remove()` because multiple children can share
                    // the same FK and each needs its own copy.
                    for p in parents.iter_mut() {
                        let p_fk_json: ::core::option::Option<::suprnova::serde_json::Value> =
                            #per_parent_key_expr;
                        let parent_row: ::core::option::Option<#target_ty> = match &p_fk_json {
                            ::core::option::Option::Some(v) => {
                                by_pk.get(&v.to_string()).cloned().or_else(|| {
                                    // Parent missing — invoke
                                    // `with_default` closure if
                                    // installed.
                                    let tmpl: ::suprnova::BelongsTo<Self, #target_ty> =
                                        ::suprnova::BelongsTo::<Self, #target_ty>::__new(
                                            ::core::option::Option::None,
                                            ::std::string::String::from(#fk),
                                            ::std::string::String::from(#owner_key),
                                        )#with_default_chain;
                                    tmpl.__default_fn().map(|f| f())
                                })
                            }
                            ::core::option::Option::None => {
                                // FK is null — same `with_default` path.
                                let tmpl: ::suprnova::BelongsTo<Self, #target_ty> =
                                    ::suprnova::BelongsTo::<Self, #target_ty>::__new(
                                        ::core::option::Option::None,
                                        ::std::string::String::from(#fk),
                                        ::std::string::String::from(#owner_key),
                                    )#with_default_chain;
                                tmpl.__default_fn().map(|f| f())
                            }
                        };
                        p.__eager.set_one::<#target_ty>(#name_str, parent_row);
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::HasMany => {
            // FK column on the child table — same default as HasOne.
            let fk = fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| default_has_fk(&parent_name));

            // Same JSON-pluck FK-reading pattern as HasOne's eager
            // arm — see the long-form comment there for why we don't
            // do field-access on the target struct. The difference is
            // we accumulate into `HashMap<key, Vec<R>>` rather than
            // `HashMap<key, R>`, and stuff via `set_many` instead of
            // `set_one`. Parents whose group is empty still get an
            // explicit `set_many(name, Vec::new())` so the loaded
            // accessor returns `&[]` (not a panic).
            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }
                    let pk_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();
                    let rows: ::std::vec::Vec<#target_ty> =
                        <#target_ty as ::suprnova::eloquent::Model>::query()
                            .filter_in(#fk, pk_values)
                            .get()
                            .await?;
                    use ::std::collections::HashMap;
                    let mut by_fk: HashMap<::std::string::String, ::std::vec::Vec<#target_ty>>
                        = HashMap::new();
                    for r in rows.into_iter() {
                        let row_json = ::suprnova::serde_json::to_value(&r)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        let key = row_json
                            .get(#fk)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        by_fk.entry(key).or_default().push(r);
                    }
                    for p in parents.iter_mut() {
                        let key = ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        let group = by_fk.remove(&key).unwrap_or_default();
                        p.__eager.set_many::<#target_ty>(#name_str, group);
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::BelongsToMany => {
            // Pivot model + key names. The parser already validates a
            // pivot type exists for BelongsToMany; the `expect` is
            // unreachable on the happy path but defensive.
            let pivot_ty = rel.through.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(
                    &rel.name,
                    "BelongsToMany requires a pivot type (parser bug if reached)",
                )
            })?;
            let pivot_fk = pivot_fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}_id", to_snake(&parent_name)));
            let pivot_related = pivot_related_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    format!("{}_id", to_snake(&last_segment_name(target_ty)))
                });
            // Two-query strategy:
            //
            // 1. Fetch all pivot rows whose FK points at any of the
            //    parent PKs in this batch.
            // 2. Fetch all related rows whose PK is in the set of
            //    pivot.related_key values.
            // 3. Walk each pivot row, look up the matching related
            //    row, clone it, stamp `__pivot = Some(Arc::new(pivot))`
            //    onto the clone, and push into the per-parent vec keyed
            //    by pivot.foreign_key.
            //
            // The clone-per-attachment is load-bearing: when a single
            // R is attached to multiple Ls via different pivot rows,
            // each L's copy must carry its OWN pivot context. The
            // `Model: Clone` supertrait makes this cheap (no new
            // bounds needed on this arm).
            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }
                    let pk_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();

                    // Step 1: pivot rows where FK ∈ pk_values.
                    let pivots: ::std::vec::Vec<#pivot_ty> =
                        <#pivot_ty as ::suprnova::eloquent::Model>::query()
                            .filter_in(#pivot_fk, pk_values.clone())
                            .get()
                            .await?;

                    if pivots.is_empty() {
                        // Every parent gets an empty slice so the
                        // loaded accessor returns `&[]` instead of
                        // panicking.
                        for p in parents.iter_mut() {
                            p.__eager.set_many::<#target_ty>(
                                #name_str,
                                ::std::vec::Vec::<#target_ty>::new(),
                            );
                        }
                        return ::core::result::Result::Ok(());
                    }

                    // Collect the distinct related-key values for the
                    // IN query.
                    use ::std::collections::HashMap;
                    let mut related_ids: ::std::vec::Vec<::suprnova::serde_json::Value>
                        = ::std::vec::Vec::with_capacity(pivots.len());
                    let mut seen_rel: ::std::collections::HashSet<::std::string::String>
                        = ::std::collections::HashSet::new();
                    for pv in pivots.iter() {
                        let pj = ::suprnova::serde_json::to_value(pv)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        if let ::core::option::Option::Some(v) = pj.get(#pivot_related) {
                            let s = v.to_string();
                            if seen_rel.insert(s) {
                                related_ids.push(v.clone());
                            }
                        }
                    }

                    // Step 2: related rows where PK ∈ related_ids.
                    // `id` is the default related-key column; the
                    // `.local_key()` override on the relation surface
                    // is not currently honoured here because the
                    // eager dispatcher uses Model::query() which keys
                    // off the model's declared primary key. T9's
                    // with_where surface can extend this if non-default
                    // related keys land in practice.
                    let related_rows: ::std::vec::Vec<#target_ty> = if related_ids.is_empty() {
                        ::std::vec::Vec::new()
                    } else {
                        <#target_ty as ::suprnova::eloquent::Model>::query()
                            .filter_in("id", related_ids)
                            .get()
                            .await?
                    };

                    // Index related rows by their `id` field (JSON-
                    // string form) for fast lookup.
                    let mut by_related_id: HashMap<::std::string::String, #target_ty>
                        = HashMap::new();
                    for r in related_rows.into_iter() {
                        let rj = ::suprnova::serde_json::to_value(&r)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        let key = rj
                            .get("id")
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        by_related_id.insert(key, r);
                    }

                    // Step 3: per pivot row, clone the matching
                    // related row, stamp __pivot, and append to the
                    // per-parent vec.
                    let mut by_parent: HashMap<
                        ::std::string::String,
                        ::std::vec::Vec<#target_ty>,
                    > = HashMap::new();
                    for pv in pivots.into_iter() {
                        let pj = ::suprnova::serde_json::to_value(&pv)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        let parent_key = pj
                            .get(#pivot_fk)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        let related_key = pj
                            .get(#pivot_related)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        if let ::core::option::Option::Some(template)
                            = by_related_id.get(&related_key)
                        {
                            let mut row: #target_ty = template.clone();
                            row.__pivot = ::core::option::Option::Some(
                                ::std::sync::Arc::new(pv),
                            );
                            by_parent.entry(parent_key).or_default().push(row);
                        }
                    }

                    // Distribute per parent. Parents with no
                    // attachments get an explicit empty slice.
                    for p in parents.iter_mut() {
                        let key = ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        let group = by_parent.remove(&key).unwrap_or_default();
                        p.__eager.set_many::<#target_ty>(#name_str, group);
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::HasManyThrough | RelationKindAttr::HasOneThrough => {
            // Two-query eager-load strategy (cleaner than a single
            // JOIN-with-extra-column because we get to reuse the
            // existing `Builder<C>` SeaORM deserialisation path):
            //
            // 1. Raw SQL: `SELECT id, {first_key} FROM B WHERE
            //    {first_key} IN (parent_ids)` — build a map
            //    `b_id -> parent_id`.
            // 2. `<C as Model>::query().filter_in({second_key}, b_ids)
            //    .get()` — uses the existing model pipeline so C
            //    deserialises correctly even with casts / accessors.
            // 3. Group C by `row.{second_key}` (which is a B.id) →
            //    look up the parent_id via the map → distribute via
            //    `set_many` (HasManyThrough) or `set_one`
            //    (HasOneThrough — first row wins per parent).
            //
            // Type rebinding: for Through kinds the parser stores
            // `(rel.target, rel.through)` as `(B, C)` — same swap as
            // `emit_relation_accessors`. We shadow the function-scope
            // `target_ty` (which would be `B`) with the final target
            // `C` taken from `rel.through`.
            let through_ty = &rel.target; // intermediate B
            let target_ty = rel.through.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(
                    &rel.name,
                    "HasOneThrough / HasManyThrough require a final target type \
                     (parser bug if reached)",
                )
            })?; // final target C
            let first_key = first_key_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}_id", to_snake(&parent_name)));
            let second_key = second_key_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    format!("{}_id", to_snake(&last_segment_name(through_ty)))
                });
            // Column on B matched by `second_key`. Defaults to "id";
            // overridable for intermediates declaring a non-`id` PK
            // via `second_local_key = "..."`. Query 1 below `SELECT`s
            // this column as `__sn_b_id` so the b->parent map keys
            // off the correct join target.
            let second_local_key = second_local_key_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "id".to_string());
            let is_one = matches!(rel.kind, RelationKindAttr::HasOneThrough);
            // Distribute branch: HasOneThrough stores `set_one`
            // (None if no row); HasManyThrough stores `set_many`
            // (empty Vec if no rows). Per-parent group reduction
            // happens client-side over the already-grouped HashMap.
            //
            // Key shape: `__sn_parent_key_to_match_cast` (declared in
            // the outer arm body below) unwraps `Value::String(s)` to
            // raw `s` rather than the JSON-quoted form, so String PKs
            // line up with the `CAST(... AS TEXT)` column on the
            // `b_to_parent` lookup. The count and aggregate arms use
            // the same helper for the same reason — the eager arm
            // previously used `to_value(...).to_string()` directly,
            // which produced `"\"abc\""` for String PKs and silently
            // missed the lookup.
            let distribute = if is_one {
                quote! {
                    for p in parents.iter_mut() {
                        let pk_str = __sn_parent_key_to_match_cast(
                            ::suprnova::serde_json::to_value(&p.#pk_ident)
                                .unwrap_or(::suprnova::serde_json::Value::Null),
                        );
                        let row: ::core::option::Option<#target_ty> =
                            by_parent.remove(&pk_str).and_then(|mut g| g.pop());
                        p.__eager.set_one::<#target_ty>(#name_str, row);
                    }
                }
            } else {
                quote! {
                    for p in parents.iter_mut() {
                        let pk_str = __sn_parent_key_to_match_cast(
                            ::suprnova::serde_json::to_value(&p.#pk_ident)
                                .unwrap_or(::suprnova::serde_json::Value::Null),
                        );
                        let group = by_parent.remove(&pk_str).unwrap_or_default();
                        p.__eager.set_many::<#target_ty>(#name_str, group);
                    }
                }
            };

            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }

                    // Per-parent FK-key derivation — matches the SQL
                    // CAST output of Query 1 below. `Value::String(s)`
                    // unwraps to raw `s` rather than the JSON-quoted
                    // form so the String PK case lines up with the raw
                    // `CAST(... AS TEXT)` result on the b->parent map.
                    // Mirrors the helper in the count and aggregate
                    // arms; spliced into both `#distribute` branches.
                    fn __sn_parent_key_to_match_cast(
                        v: ::suprnova::serde_json::Value,
                    ) -> ::std::string::String {
                        match v {
                            ::suprnova::serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        }
                    }

                    let pk_json_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();

                    let db_backend = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::get_database_backend(db);

                    // Backend-aware placeholder rendering for the
                    // IN-list on the intermediate table query.
                    let mut placeholders: ::std::vec::Vec<::std::string::String> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    let mut binds: ::std::vec::Vec<::suprnova::sea_orm::Value> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    for (i, v) in pk_json_values.iter().enumerate() {
                        let ph = match db_backend {
                            ::suprnova::sea_orm::DatabaseBackend::Postgres => {
                                ::std::format!("${}", i + 1)
                            }
                            _ => ::std::string::String::from("?"),
                        };
                        placeholders.push(ph);
                        binds.push(
                            ::suprnova::eloquent::model::json_value_to_sea_value(v),
                        );
                    }

                    let __sn_b_table = <#through_ty as
                        ::suprnova::eloquent::EloquentModel>::TABLE;

                    // Query 1 — pull (b_id, parent_id) mapping. We
                    // CAST both columns to TEXT/CHAR so the
                    // HashMap key shape lines up regardless of the
                    // underlying integer vs string column type. The
                    // `Value::String(s) => s` normalisation on the
                    // parent side matches what HasMany's count arm
                    // does.
                    let __sn_cast_kw = match db_backend {
                        ::suprnova::sea_orm::DatabaseBackend::MySql => "CHAR",
                        _ => "TEXT",
                    };
                    let __sn_map_sql = ::std::format!(
                        "SELECT CAST({slk} AS {cast}) AS __sn_b_id, \
                                CAST({fk} AS {cast}) AS __sn_parent_id \
                           FROM {table} \
                          WHERE {fk} IN ({phs})",
                        cast = __sn_cast_kw,
                        fk = #first_key,
                        slk = #second_local_key,
                        table = __sn_b_table,
                        phs = placeholders.join(", "),
                    );
                    let __sn_map_stmt = ::suprnova::sea_orm::Statement::from_sql_and_values(
                        db_backend,
                        &__sn_map_sql,
                        binds,
                    );
                    let __sn_map_rows = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::query_all(db, __sn_map_stmt)
                        .await
                        .map_err(|e| ::suprnova::FrameworkError::database(e.to_string()))?;

                    use ::std::collections::HashMap;
                    // b_id -> parent_id (both as string keys).
                    let mut b_to_parent: HashMap<::std::string::String, ::std::string::String>
                        = HashMap::new();
                    // The IN-set of B's `id`s — we re-issue Query 2 on
                    // C with these, keeping the existing model-level
                    // typed deserialisation.
                    let mut b_ids: ::std::vec::Vec<::suprnova::serde_json::Value>
                        = ::std::vec::Vec::with_capacity(__sn_map_rows.len());
                    for r in __sn_map_rows.iter() {
                        let b_id = r
                            .try_get::<::std::string::String>("", "__sn_b_id")
                            .unwrap_or_default();
                        let parent_id = r
                            .try_get::<::std::string::String>("", "__sn_parent_id")
                            .unwrap_or_default();
                        if b_id.is_empty() { continue; }
                        b_ids.push(::suprnova::serde_json::Value::from(b_id.clone()));
                        b_to_parent.insert(b_id, parent_id);
                    }

                    // Group container declared up-front so the
                    // `#distribute` block (which `.remove()`s per
                    // parent key) compiles for both the empty-set
                    // short-circuit AND the populated path. Empty
                    // `b_ids` means no rows go in; per-parent
                    // distribution still runs so every parent gets
                    // an explicit empty cache entry (not a panic).
                    let mut by_parent: HashMap<
                        ::std::string::String,
                        ::std::vec::Vec<#target_ty>,
                    > = HashMap::new();

                    // Short-circuit when no intermediate rows match —
                    // every parent gets an empty / None entry so the
                    // loaded accessor doesn't panic on "you forgot
                    // `with([\"...\"])`".
                    if b_ids.is_empty() {
                        #distribute
                        return ::core::result::Result::Ok(());
                    }

                    // Query 2 — pull C rows via the existing
                    // Model::query() pipeline. `filter_in` runs the
                    // same bind / placeholder / typed-deserialisation
                    // path the rest of the framework uses.
                    let c_rows: ::std::vec::Vec<#target_ty> =
                        <#target_ty as ::suprnova::eloquent::Model>::query()
                            .filter_in(#second_key, b_ids)
                            .get()
                            .await?;

                    // Group C rows by parent_id, via the b->parent
                    // map. The per-row C.second_key is JSON-plucked
                    // (same pattern as HasMany's eager arm) and
                    // normalised to the raw string form so it lines
                    // up with the CAST-as-TEXT keys in `b_to_parent`.
                    for r in c_rows.into_iter() {
                        let row_json = ::suprnova::serde_json::to_value(&r)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        let b_id_key = match row_json.get(#second_key) {
                            ::core::option::Option::Some(
                                ::suprnova::serde_json::Value::String(s),
                            ) => s.clone(),
                            ::core::option::Option::Some(other) => other.to_string(),
                            ::core::option::Option::None => ::std::string::String::new(),
                        };
                        if let ::core::option::Option::Some(parent_id)
                            = b_to_parent.get(&b_id_key)
                        {
                            by_parent
                                .entry(parent_id.clone())
                                .or_default()
                                .push(r);
                        }
                    }

                    // Distribute — branches on HasOne vs HasMany
                    // through the `#distribute` token block above.
                    #distribute
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::MorphMany | RelationKindAttr::MorphOne => {
            // Polymorphic eager-load. Same shape as the HasMany arm
            // (IN-query by parent IDs, group by FK, distribute into
            // each parent's `__eager` cache) but with an additional
            // `<name>_type = '<morph_type>'` predicate so children of
            // OTHER morph families (e.g. comments on Video when the
            // parent is Post) are excluded.
            //
            // The id column on the child = `<morph_name>_id` and the
            // type column = `<morph_name>_type` — both baked from
            // `morph_name` (which defaults to the relation name).
            //
            // MorphOne distributes via `set_one` (first row wins per
            // parent); MorphMany distributes via `set_many`.
            let morph_name = morph_name_or_default(rel);
            let morph_type_value = morph_type_of(input);
            let id_col = format!("{morph_name}_id");
            let type_col = format!("{morph_name}_type");
            let is_one = matches!(rel.kind, RelationKindAttr::MorphOne);
            let distribute = if is_one {
                quote! {
                    for p in parents.iter_mut() {
                        let key = ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        // First row wins (per HasOne semantics) — we
                        // sort by id ASC implicitly via the order the
                        // groups were built, but explicit `LIMIT 1`
                        // logic isn't worth a separate dispatch path
                        // because MorphOne is by contract 0-or-1 row.
                        let row = by_fk.remove(&key).and_then(|mut v| v.pop());
                        p.__eager.set_one::<#target_ty>(#name_str, row);
                    }
                }
            } else {
                quote! {
                    for p in parents.iter_mut() {
                        let key = ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        let group = by_fk.remove(&key).unwrap_or_default();
                        p.__eager.set_many::<#target_ty>(#name_str, group);
                    }
                }
            };
            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }
                    let pk_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();
                    // Type-string predicate goes through the
                    // standard `filter` path; the inner builder
                    // serialises it to a bind parameter via
                    // `IntoVal`. We pre-wrap it in a JSON `String`
                    // value so the WhereTerm storage stays
                    // homogeneous with the IN-list above.
                    let morph_type_predicate =
                        ::suprnova::serde_json::Value::String(
                            ::std::string::String::from(#morph_type_value),
                        );
                    let rows: ::std::vec::Vec<#target_ty> =
                        <#target_ty as ::suprnova::eloquent::Model>::query()
                            .filter_in(#id_col, pk_values)
                            .filter(#type_col, morph_type_predicate)
                            .get()
                            .await?;
                    use ::std::collections::HashMap;
                    let mut by_fk: HashMap<::std::string::String, ::std::vec::Vec<#target_ty>>
                        = HashMap::new();
                    for r in rows.into_iter() {
                        // JSON-pluck the morph-id column off the
                        // returned row — same pattern as the HasMany
                        // arm. Avoids requiring the macro at THIS
                        // expansion site to know the target struct's
                        // field layout.
                        let row_json = ::suprnova::serde_json::to_value(&r)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        let key = row_json
                            .get(#id_col)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        by_fk.entry(key).or_default().push(r);
                    }
                    #distribute
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        _ => Ok(None),
    }
}

/// `__count_relation` arm for one relation. HasOne / BelongsTo both
/// produce 0-or-1 row counts; T2 wires both to keep the API uniform
/// (the spec lets `with_count(["profile"])` return 0 or 1). T3+ will
/// extend this for HasMany / BelongsToMany where COUNT actually
/// branches into real GROUP BY queries.
fn emit_count_arm(input: &ModelInput, rel: &RelationDecl) -> Result<Option<TokenStream>> {
    let struct_ident = &input.item.ident;
    let name_str = rel.name.to_string();
    let pk_name = &input.primary_key;
    let pk_ident = quote::format_ident!("{pk_name}");
    let target_ty = &rel.target;
    let parent_name = struct_ident.to_string();

    match rel.kind {
        RelationKindAttr::HasOne => {
            let fk = fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| default_has_fk(&parent_name));
            // Same shape as `__eager_load`: run an IN query, group by
            // FK (via JSON-pluck — see eager arm for why), store the
            // per-parent count via `set_count`.
            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }
                    let pk_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();
                    let rows: ::std::vec::Vec<#target_ty> =
                        <#target_ty as ::suprnova::eloquent::Model>::query()
                            .filter_in(#fk, pk_values)
                            .get()
                            .await?;
                    use ::std::collections::HashMap;
                    let mut counts: HashMap<::std::string::String, u64> = HashMap::new();
                    for r in rows.iter() {
                        let row_json = ::suprnova::serde_json::to_value(r)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        let key = row_json
                            .get(#fk)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        *counts.entry(key).or_insert(0) += 1;
                    }
                    for p in parents.iter_mut() {
                        let key = ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        p.__eager.set_count(#name_str, *counts.get(&key).unwrap_or(&0));
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::BelongsTo => {
            let fk = fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| default_belongs_to_fk(target_ty));
            let owner_key = lk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "id".to_string());
            let fk_ident = quote::format_ident!("{}", fk);
            let fk_is_optional = field_is_optional(input, &fk);
            let per_parent_key_expr = if fk_is_optional {
                quote! {
                    p.#fk_ident
                        .as_ref()
                        .and_then(|v| ::suprnova::serde_json::to_value(v).ok())
                }
            } else {
                quote! {
                    ::core::option::Option::Some(
                        ::suprnova::serde_json::to_value(&p.#fk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null),
                    )
                }
            };
            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }
                    let fk_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .filter_map(|p| {
                            let v: ::core::option::Option<::suprnova::serde_json::Value> =
                                #per_parent_key_expr;
                            v
                        })
                        .collect();
                    let parent_rows: ::std::vec::Vec<#target_ty> = if fk_values.is_empty() {
                        ::std::vec::Vec::new()
                    } else {
                        <#target_ty as ::suprnova::eloquent::Model>::query()
                            .filter_in(#owner_key, fk_values)
                            .get()
                            .await?
                    };
                    use ::std::collections::HashSet;
                    let mut existing_keys: HashSet<::std::string::String> = HashSet::new();
                    for r in parent_rows.iter() {
                        let row_json = ::suprnova::serde_json::to_value(r)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        if let ::core::option::Option::Some(v) = row_json.get(#owner_key) {
                            existing_keys.insert(v.to_string());
                        }
                    }
                    for p in parents.iter_mut() {
                        let v: ::core::option::Option<::suprnova::serde_json::Value> =
                            #per_parent_key_expr;
                        let count: u64 = match &v {
                            ::core::option::Option::Some(jv) => {
                                if existing_keys.contains(&jv.to_string()) { 1 } else { 0 }
                            }
                            ::core::option::Option::None => 0,
                        };
                        p.__eager.set_count(#name_str, count);
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::HasMany => {
            let fk = fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| default_has_fk(&parent_name));

            // Server-side `GROUP BY` count — one round trip regardless
            // of fan-out. The previous implementation fetched every
            // child row into memory and counted client-side via a
            // HashMap; at 10K children per parent that's 10K rows over
            // the wire just to learn the count. This arm issues:
            //
            //   SELECT CAST(<fk> AS TEXT) AS __sn_fk_key,
            //          COUNT(*)           AS __sn_count
            //     FROM <child_table>
            //    WHERE <fk> IN (?, ?, ...)
            //    GROUP BY <fk>
            //
            // and distributes the per-FK counts into each parent's
            // `__eager.set_count(name, n)`. Parents whose PK didn't
            // appear in any child row get 0 — set explicitly so the
            // `<rel>_count()` accessor doesn't panic on "you forgot
            // `with_count`".
            //
            // ## FK key matching
            //
            // The SQL `CAST(... AS TEXT)` form produces the raw
            // stringified column value for both integer FKs (`"42"`)
            // and string FKs (`"abc"`) on SQLite + Postgres; MySQL's
            // `CAST(... AS CHAR)` produces the same shape and the
            // backend branch below picks the right form. The
            // parent-side key is derived to MATCH that raw form: a
            // `serde_json::Value::String("abc")` is unwrapped to its
            // inner `String` rather than rendered as `"\"abc\""` via
            // `Value::to_string()`. This is internal to the dispatcher
            // — the cache key for `set_count` is the relation name,
            // not the FK key, so internal consistency is all that
            // matters.
            //
            // T4-T7's count arms should follow the same server-side
            // pattern. The HasMany aggregate arm above still reduces
            // client-side; converting it lands under its own task
            // because the aggregate dispatcher signature carries the
            // `kind` branch and a different SQL shape per aggregate.
            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }

                    // Per-parent FK-key derivation — matches the SQL
                    // CAST output below. `Value::String(s)` unwraps to
                    // raw `s` rather than the JSON-quoted form so the
                    // string FK case lines up with the raw CAST result.
                    fn __sn_parent_key_to_match_cast(
                        v: ::suprnova::serde_json::Value,
                    ) -> ::std::string::String {
                        match v {
                            ::suprnova::serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        }
                    }

                    let pk_json_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();

                    let db_backend = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::get_database_backend(db);

                    // Build the placeholder list. Per-backend dialect
                    // matches the inner `Builder` renderer: Postgres
                    // uses `$N`, others use `?`. `parents.is_empty()`
                    // already short-circuited above so the bind list
                    // is non-empty here.
                    let mut placeholders: ::std::vec::Vec<::std::string::String> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    let mut binds: ::std::vec::Vec<::suprnova::sea_orm::Value> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    for (i, v) in pk_json_values.iter().enumerate() {
                        let ph = match db_backend {
                            ::suprnova::sea_orm::DatabaseBackend::Postgres => {
                                ::std::format!("${}", i + 1)
                            }
                            _ => ::std::string::String::from("?"),
                        };
                        placeholders.push(ph);
                        binds.push(
                            ::suprnova::eloquent::model::json_value_to_sea_value(v),
                        );
                    }

                    // `CAST(... AS CHAR)` on MySQL, `CAST(... AS TEXT)`
                    // elsewhere — both yield the raw stringified column
                    // value the parent-side key derivation matches.
                    let __sn_cast_kw = match db_backend {
                        ::suprnova::sea_orm::DatabaseBackend::MySql => "CHAR",
                        _ => "TEXT",
                    };
                    let __sn_table = <#target_ty as
                        ::suprnova::eloquent::EloquentModel>::TABLE;
                    let __sn_sql = ::std::format!(
                        "SELECT CAST({fk} AS {cast}) AS __sn_fk_key, \
                                COUNT(*) AS __sn_count \
                           FROM {table} \
                          WHERE {fk} IN ({phs}) \
                          GROUP BY {fk}",
                        fk = #fk,
                        cast = __sn_cast_kw,
                        table = __sn_table,
                        phs = placeholders.join(", "),
                    );

                    let stmt = ::suprnova::sea_orm::Statement::from_sql_and_values(
                        db_backend,
                        &__sn_sql,
                        binds,
                    );
                    let rows = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::query_all(db, stmt)
                        .await
                        .map_err(|e| ::suprnova::FrameworkError::database(e.to_string()))?;

                    use ::std::collections::HashMap;
                    let mut counts: HashMap<::std::string::String, u64> = HashMap::new();
                    for r in rows.iter() {
                        // Both columns come back via `try_get` against
                        // their declared aliases. COUNT(*) is a 64-bit
                        // signed integer on every backend SeaORM
                        // supports here.
                        let key: ::std::string::String = r
                            .try_get::<::std::string::String>("", "__sn_fk_key")
                            .unwrap_or_default();
                        let n: i64 = r.try_get::<i64>("", "__sn_count").unwrap_or(0);
                        // Negative COUNT shouldn't happen on real
                        // backends, but the saturating cast guards
                        // against pathological drivers without
                        // panicking the dispatcher.
                        counts.insert(key, n.max(0) as u64);
                    }

                    for p in parents.iter_mut() {
                        let key = __sn_parent_key_to_match_cast(
                            ::suprnova::serde_json::to_value(&p.#pk_ident)
                                .unwrap_or(::suprnova::serde_json::Value::Null),
                        );
                        p.__eager.set_count(#name_str, *counts.get(&key).unwrap_or(&0));
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::BelongsToMany => {
            let pivot_ty = rel.through.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(
                    &rel.name,
                    "BelongsToMany requires a pivot type (parser bug if reached)",
                )
            })?;
            let pivot_fk = pivot_fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}_id", to_snake(&parent_name)));
            let pivot_table_expr: TokenStream = match pivot_table_override(rel) {
                Some(t) => {
                    let lit = syn::LitStr::new(t, proc_macro2::Span::call_site());
                    quote! { #lit }
                }
                None => quote! {
                    <#pivot_ty as ::suprnova::eloquent::EloquentModel>::TABLE
                },
            };

            // Server-side GROUP BY count over the pivot table — one
            // round trip regardless of fan-out. Identical pattern to
            // the HasMany count arm, except the GROUP-BY target is the
            // pivot's FK column and the source table is the pivot.
            // See the HasMany arm's long-form comment for the
            // CAST-as-text key-matching contract.
            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }

                    fn __sn_parent_key_to_match_cast(
                        v: ::suprnova::serde_json::Value,
                    ) -> ::std::string::String {
                        match v {
                            ::suprnova::serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        }
                    }

                    let pk_json_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();

                    let db_backend = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::get_database_backend(db);

                    let mut placeholders: ::std::vec::Vec<::std::string::String> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    let mut binds: ::std::vec::Vec<::suprnova::sea_orm::Value> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    for (i, v) in pk_json_values.iter().enumerate() {
                        let ph = match db_backend {
                            ::suprnova::sea_orm::DatabaseBackend::Postgres => {
                                ::std::format!("${}", i + 1)
                            }
                            _ => ::std::string::String::from("?"),
                        };
                        placeholders.push(ph);
                        binds.push(
                            ::suprnova::eloquent::model::json_value_to_sea_value(v),
                        );
                    }

                    let __sn_cast_kw = match db_backend {
                        ::suprnova::sea_orm::DatabaseBackend::MySql => "CHAR",
                        _ => "TEXT",
                    };
                    let __sn_table = #pivot_table_expr;
                    let __sn_sql = ::std::format!(
                        "SELECT CAST({fk} AS {cast}) AS __sn_fk_key, \
                                COUNT(*) AS __sn_count \
                           FROM {table} \
                          WHERE {fk} IN ({phs}) \
                          GROUP BY {fk}",
                        fk = #pivot_fk,
                        cast = __sn_cast_kw,
                        table = __sn_table,
                        phs = placeholders.join(", "),
                    );

                    let stmt = ::suprnova::sea_orm::Statement::from_sql_and_values(
                        db_backend,
                        &__sn_sql,
                        binds,
                    );
                    let rows = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::query_all(db, stmt)
                        .await
                        .map_err(|e| ::suprnova::FrameworkError::database(e.to_string()))?;

                    use ::std::collections::HashMap;
                    let mut counts: HashMap<::std::string::String, u64> = HashMap::new();
                    for r in rows.iter() {
                        let key: ::std::string::String = r
                            .try_get::<::std::string::String>("", "__sn_fk_key")
                            .unwrap_or_default();
                        let n: i64 = r.try_get::<i64>("", "__sn_count").unwrap_or(0);
                        counts.insert(key, n.max(0) as u64);
                    }

                    for p in parents.iter_mut() {
                        let key = __sn_parent_key_to_match_cast(
                            ::suprnova::serde_json::to_value(&p.#pk_ident)
                                .unwrap_or(::suprnova::serde_json::Value::Null),
                        );
                        p.__eager.set_count(#name_str, *counts.get(&key).unwrap_or(&0));
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::HasManyThrough | RelationKindAttr::HasOneThrough => {
            // Server-side GROUP BY count via the two-hop JOIN. One
            // round trip regardless of fan-out across the C table.
            // Mirrors HasMany's count arm but the COUNT source is
            // `<C> INNER JOIN <B>` and we group by `B.<first_key>`
            // (which is the parent's PK value, normalised via
            // CAST AS TEXT/CHAR).
            //
            //   SELECT CAST(b.<first_key> AS TEXT|CHAR) AS __sn_fk_key,
            //          COUNT(*) AS __sn_count
            //     FROM <C> c
            //     JOIN <B> b ON c.<second_key> = b.<second_local_key>
            //    WHERE b.<first_key> IN (?, ?, ...)
            //    GROUP BY b.<first_key>
            //
            // HasOneThrough reports the real COUNT(*) here — the JOIN
            // itself can return multiple C rows per parent if the HasOne
            // contract is violated, and we'd rather surface the real
            // count + let tests catch a malformed dataset than silently
            // truncate at the SQL layer.
            //
            // Type rebinding: same swap as the eager arm — for
            // Through kinds the parser stores `(B, C)` as
            // `(rel.target, rel.through)`. Shadow the function-scope
            // `target_ty` with the final target `C`.
            let through_ty = &rel.target; // intermediate B
            let target_ty = rel.through.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(
                    &rel.name,
                    "HasOneThrough / HasManyThrough require a final target type \
                     (parser bug if reached)",
                )
            })?; // final target C
            let first_key = first_key_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}_id", to_snake(&parent_name)));
            let second_key = second_key_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    format!("{}_id", to_snake(&last_segment_name(through_ty)))
                });
            // JOIN-target column on B. Defaults to `"id"`; overridable
            // via `second_local_key = "..."` for intermediates with a
            // non-`id` PK.
            let second_local_key = second_local_key_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "id".to_string());

            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }

                    fn __sn_parent_key_to_match_cast(
                        v: ::suprnova::serde_json::Value,
                    ) -> ::std::string::String {
                        match v {
                            ::suprnova::serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        }
                    }

                    let pk_json_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();

                    let db_backend = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::get_database_backend(db);

                    let mut placeholders: ::std::vec::Vec<::std::string::String> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    let mut binds: ::std::vec::Vec<::suprnova::sea_orm::Value> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    for (i, v) in pk_json_values.iter().enumerate() {
                        let ph = match db_backend {
                            ::suprnova::sea_orm::DatabaseBackend::Postgres => {
                                ::std::format!("${}", i + 1)
                            }
                            _ => ::std::string::String::from("?"),
                        };
                        placeholders.push(ph);
                        binds.push(
                            ::suprnova::eloquent::model::json_value_to_sea_value(v),
                        );
                    }

                    let __sn_cast_kw = match db_backend {
                        ::suprnova::sea_orm::DatabaseBackend::MySql => "CHAR",
                        _ => "TEXT",
                    };
                    let __sn_b_table = <#through_ty as
                        ::suprnova::eloquent::EloquentModel>::TABLE;
                    let __sn_c_table = <#target_ty as
                        ::suprnova::eloquent::EloquentModel>::TABLE;
                    let __sn_sql = ::std::format!(
                        "SELECT CAST(__sn_b.{fk} AS {cast}) AS __sn_fk_key, \
                                COUNT(*) AS __sn_count \
                           FROM {c_table} __sn_c \
                           JOIN {b_table} __sn_b \
                             ON __sn_c.{second_key} = __sn_b.{slk} \
                          WHERE __sn_b.{fk} IN ({phs}) \
                          GROUP BY __sn_b.{fk}",
                        fk = #first_key,
                        second_key = #second_key,
                        slk = #second_local_key,
                        cast = __sn_cast_kw,
                        c_table = __sn_c_table,
                        b_table = __sn_b_table,
                        phs = placeholders.join(", "),
                    );

                    let stmt = ::suprnova::sea_orm::Statement::from_sql_and_values(
                        db_backend,
                        &__sn_sql,
                        binds,
                    );
                    let rows = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::query_all(db, stmt)
                        .await
                        .map_err(|e| ::suprnova::FrameworkError::database(e.to_string()))?;

                    use ::std::collections::HashMap;
                    let mut counts: HashMap<::std::string::String, u64> = HashMap::new();
                    for r in rows.iter() {
                        let key: ::std::string::String = r
                            .try_get::<::std::string::String>("", "__sn_fk_key")
                            .unwrap_or_default();
                        let n: i64 = r.try_get::<i64>("", "__sn_count").unwrap_or(0);
                        counts.insert(key, n.max(0) as u64);
                    }

                    for p in parents.iter_mut() {
                        let key = __sn_parent_key_to_match_cast(
                            ::suprnova::serde_json::to_value(&p.#pk_ident)
                                .unwrap_or(::suprnova::serde_json::Value::Null),
                        );
                        p.__eager.set_count(#name_str, *counts.get(&key).unwrap_or(&0));
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::MorphMany | RelationKindAttr::MorphOne => {
            // Server-side GROUP BY count over the child table, with
            // both the `<name>_id IN (...)` and
            // `<name>_type = '<morph_type>'` predicates applied so
            // children of other morph families are excluded from the
            // count.
            //
            //   SELECT CAST(<id_col> AS TEXT|CHAR) AS __sn_fk_key,
            //          COUNT(*)                   AS __sn_count
            //     FROM <child_table>
            //    WHERE <id_col> IN (?, ?, ...)
            //      AND <type_col> = ?
            //    GROUP BY <id_col>
            //
            // Same CAST-as-text key-matching contract as the HasMany
            // count arm — see that arm for the long-form rationale.
            //
            // MorphOne's count surface is 0-or-1 in practice (the
            // contract says one child per parent), so the real count
            // is reported here even when violated upstream — tests
            // catch the malformed dataset rather than silently
            // truncating at the SQL layer.
            let morph_name = morph_name_or_default(rel);
            let morph_type_value = morph_type_of(input);
            let id_col = format!("{morph_name}_id");
            let type_col = format!("{morph_name}_type");
            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }

                    fn __sn_parent_key_to_match_cast(
                        v: ::suprnova::serde_json::Value,
                    ) -> ::std::string::String {
                        match v {
                            ::suprnova::serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        }
                    }

                    let pk_json_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();

                    let db_backend = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::get_database_backend(db);

                    let mut placeholders: ::std::vec::Vec<::std::string::String> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    let mut binds: ::std::vec::Vec<::suprnova::sea_orm::Value> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len() + 1);
                    for (i, v) in pk_json_values.iter().enumerate() {
                        let ph = match db_backend {
                            ::suprnova::sea_orm::DatabaseBackend::Postgres => {
                                ::std::format!("${}", i + 1)
                            }
                            _ => ::std::string::String::from("?"),
                        };
                        placeholders.push(ph);
                        binds.push(
                            ::suprnova::eloquent::model::json_value_to_sea_value(v),
                        );
                    }
                    // The morph-type predicate gets the next sequential
                    // Postgres placeholder ($N+1) or `?` on the
                    // remaining backends. Bound after the IN-list.
                    let type_ph = match db_backend {
                        ::suprnova::sea_orm::DatabaseBackend::Postgres => {
                            ::std::format!("${}", pk_json_values.len() + 1)
                        }
                        _ => ::std::string::String::from("?"),
                    };
                    binds.push(::suprnova::sea_orm::Value::from(#morph_type_value));

                    let __sn_cast_kw = match db_backend {
                        ::suprnova::sea_orm::DatabaseBackend::MySql => "CHAR",
                        _ => "TEXT",
                    };
                    let __sn_table = <#target_ty as
                        ::suprnova::eloquent::EloquentModel>::TABLE;
                    let __sn_sql = ::std::format!(
                        "SELECT CAST({id} AS {cast}) AS __sn_fk_key, \
                                COUNT(*) AS __sn_count \
                           FROM {table} \
                          WHERE {id} IN ({phs}) \
                            AND {type_col} = {type_ph} \
                          GROUP BY {id}",
                        id = #id_col,
                        cast = __sn_cast_kw,
                        table = __sn_table,
                        type_col = #type_col,
                        type_ph = type_ph,
                        phs = placeholders.join(", "),
                    );

                    let stmt = ::suprnova::sea_orm::Statement::from_sql_and_values(
                        db_backend,
                        &__sn_sql,
                        binds,
                    );
                    let rows = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::query_all(db, stmt)
                        .await
                        .map_err(|e| ::suprnova::FrameworkError::database(e.to_string()))?;

                    use ::std::collections::HashMap;
                    let mut counts: HashMap<::std::string::String, u64> = HashMap::new();
                    for r in rows.iter() {
                        let key: ::std::string::String = r
                            .try_get::<::std::string::String>("", "__sn_fk_key")
                            .unwrap_or_default();
                        let n: i64 = r.try_get::<i64>("", "__sn_count").unwrap_or(0);
                        counts.insert(key, n.max(0) as u64);
                    }

                    for p in parents.iter_mut() {
                        let key = __sn_parent_key_to_match_cast(
                            ::suprnova::serde_json::to_value(&p.#pk_ident)
                                .unwrap_or(::suprnova::serde_json::Value::Null),
                        );
                        p.__eager.set_count(#name_str, *counts.get(&key).unwrap_or(&0));
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        _ => Ok(None),
    }
}

/// `__aggregate_relation` arm for HasOne / BelongsTo. Same shape as
/// count — we run the IN query, then per parent pick a single row (or
/// none) and apply the SUM/AVG/MIN/MAX, which over 0-or-1 row is
/// either the column value itself or 0 / null. For T2 the column is
/// stored as `f64` for SUM/AVG (matching `with_sum`'s usual signature)
/// and we honour the same for MIN/MAX. T9 may extend the shape for
/// non-numeric MIN/MAX once the eager loading orchestrator lands.
///
/// NB: HasOne / BelongsTo `with_sum`/`avg`/`min`/`max` rarely make
/// sense in practice (the result is over at most one row), but the
/// spec lets users call them, so we wire the path here for parity.
/// Users querying real aggregates use HasMany (T3).
fn emit_aggregate_arm(input: &ModelInput, rel: &RelationDecl) -> Result<Option<TokenStream>> {
    let struct_ident = &input.item.ident;
    let name_str = rel.name.to_string();
    let pk_name = &input.primary_key;
    let pk_ident = quote::format_ident!("{pk_name}");
    let target_ty = &rel.target;
    let parent_name = struct_ident.to_string();

    match rel.kind {
        RelationKindAttr::HasOne => {
            let fk = fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| default_has_fk(&parent_name));
            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }
                    let pk_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();
                    let rows: ::std::vec::Vec<#target_ty> =
                        <#target_ty as ::suprnova::eloquent::Model>::query()
                            .filter_in(#fk, pk_values)
                            .get()
                            .await?;
                    use ::std::collections::HashMap;
                    let mut by_fk: HashMap<::std::string::String, f64> = HashMap::new();
                    for r in rows.iter() {
                        let row_json = ::suprnova::serde_json::to_value(r)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        let key = row_json
                            .get(#fk)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        let col_val = row_json
                            .get(column)
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        // Each parent's group has 0-or-1 row, so the
                        // aggregate function is the same on every kind
                        // — just record the column value.
                        by_fk.insert(key, col_val);
                    }
                    // T3-T7 aggregate arms must apply the same
                    // Sum|Avg vs Min|Max branch — see the T2
                    // quality-fix commit. Sum/Avg over an empty
                    // group stores 0.0 (consistent with the
                    // framework's COALESCE behaviour). Min/Max over
                    // an empty group stores Option::<f64>::None
                    // (matches SQL's NULL-on-empty semantics + the
                    // existing Builder::min/max Option<T> return
                    // type). Non-empty groups always store
                    // Some(value) for Min/Max.
                    //
                    // Cache key is the relation name only; T9 widens
                    // to <rel>_<kind>_<col> when the user-facing
                    // Builder::with_<agg> surface ships so a single
                    // builder can carry multiple aggregates on the
                    // same relation without colliding on this cell.
                    for p in parents.iter_mut() {
                        let key = ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        let opt_v: ::core::option::Option<f64> = by_fk.get(&key).copied();
                        match kind {
                            ::suprnova::AggregateKind::Sum
                            | ::suprnova::AggregateKind::Avg => {
                                p.__eager.set_aggregate::<f64>(
                                    #name_str,
                                    opt_v.unwrap_or(0.0),
                                );
                            }
                            ::suprnova::AggregateKind::Min
                            | ::suprnova::AggregateKind::Max => {
                                p.__eager.set_aggregate::<::core::option::Option<f64>>(
                                    #name_str,
                                    opt_v,
                                );
                            }
                        }
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::BelongsTo => {
            let fk = fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| default_belongs_to_fk(target_ty));
            let owner_key = lk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "id".to_string());
            let fk_ident = quote::format_ident!("{}", fk);
            let fk_is_optional = field_is_optional(input, &fk);
            let per_parent_key_expr = if fk_is_optional {
                quote! {
                    p.#fk_ident
                        .as_ref()
                        .and_then(|v| ::suprnova::serde_json::to_value(v).ok())
                }
            } else {
                quote! {
                    ::core::option::Option::Some(
                        ::suprnova::serde_json::to_value(&p.#fk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null),
                    )
                }
            };
            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }
                    let fk_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .filter_map(|p| {
                            let v: ::core::option::Option<::suprnova::serde_json::Value> =
                                #per_parent_key_expr;
                            v
                        })
                        .collect();
                    let parent_rows: ::std::vec::Vec<#target_ty> = if fk_values.is_empty() {
                        ::std::vec::Vec::new()
                    } else {
                        <#target_ty as ::suprnova::eloquent::Model>::query()
                            .filter_in(#owner_key, fk_values)
                            .get()
                            .await?
                    };
                    use ::std::collections::HashMap;
                    let mut by_pk: HashMap<::std::string::String, f64> = HashMap::new();
                    for r in parent_rows.iter() {
                        let row_json = ::suprnova::serde_json::to_value(r)
                            .unwrap_or(::suprnova::serde_json::Value::Null);
                        let key = row_json
                            .get(#owner_key)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        let col_val = row_json
                            .get(column)
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        by_pk.insert(key, col_val);
                    }
                    // T3-T7 aggregate arms must apply the same
                    // Sum|Avg vs Min|Max branch — see the T2
                    // quality-fix commit. Sum/Avg over an empty
                    // group stores 0.0 (consistent with the
                    // framework's COALESCE behaviour). Min/Max over
                    // an empty group stores Option::<f64>::None
                    // (matches SQL's NULL-on-empty semantics + the
                    // existing Builder::min/max Option<T> return
                    // type). Non-empty groups always store
                    // Some(value) for Min/Max.
                    //
                    // Cache key is the relation name only; T9 widens
                    // to <rel>_<kind>_<col> when the user-facing
                    // Builder::with_<agg> surface ships so a single
                    // builder can carry multiple aggregates on the
                    // same relation without colliding on this cell.
                    for p in parents.iter_mut() {
                        let v: ::core::option::Option<::suprnova::serde_json::Value> =
                            #per_parent_key_expr;
                        let opt_v: ::core::option::Option<f64> = match &v {
                            ::core::option::Option::Some(jv) => {
                                by_pk.get(&jv.to_string()).copied()
                            }
                            ::core::option::Option::None => ::core::option::Option::None,
                        };
                        match kind {
                            ::suprnova::AggregateKind::Sum
                            | ::suprnova::AggregateKind::Avg => {
                                p.__eager.set_aggregate::<f64>(
                                    #name_str,
                                    opt_v.unwrap_or(0.0),
                                );
                            }
                            ::suprnova::AggregateKind::Min
                            | ::suprnova::AggregateKind::Max => {
                                p.__eager.set_aggregate::<::core::option::Option<f64>>(
                                    #name_str,
                                    opt_v,
                                );
                            }
                        }
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::HasMany => {
            let fk = fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| default_has_fk(&parent_name));

            // Server-side GROUP BY query, one round-trip per aggregate
            // kind invocation. Mirrors the count arm's pattern: build
            // a `SELECT CAST(<fk> AS TEXT|CHAR) AS __sn_fk_key,
            // <AGG>(<col>) AS __sn_agg FROM <table> WHERE <fk> IN (...)
            // GROUP BY <fk>` statement, then distribute per-FK results
            // into each parent's `__eager.set_aggregate` cell.
            //
            // The aggregate expression is picked at runtime from the
            // dispatcher's `kind` arg — Sum/Avg/Min/Max each map to the
            // corresponding SQL function. Sum/Avg over an empty group
            // store 0.0 (matches the framework's COALESCE behaviour);
            // Min/Max over an empty group store `Option::None` (matches
            // SQL's NULL-on-empty + the Builder::min/max Option<T>
            // return type). Non-empty groups always produce Some(value)
            // for Min/Max.
            //
            // `__sn_agg` is read as `Option<f64>` because AVG over an
            // empty group is NULL in SQL — and SUM is too, even though
            // our user-facing default is 0.0; the None vs Some(v)
            // branch below normalises that.
            //
            // Cache key is the relation name only — same caveat as
            // T2's HasOne arm. T9 will widen to
            // <rel>_<kind>_<col> when the `with_<agg>` Builder surface
            // ships so multiple aggregates on the same relation can
            // coexist on a single row without clobbering each other.
            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }

                    // Per-parent FK-key derivation — matches the SQL
                    // CAST output below. `Value::String(s)` unwraps to
                    // raw `s` rather than the JSON-quoted form so the
                    // string FK case lines up with the raw CAST result.
                    // Identical to the count arm's helper.
                    fn __sn_parent_key_to_match_cast(
                        v: ::suprnova::serde_json::Value,
                    ) -> ::std::string::String {
                        match v {
                            ::suprnova::serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        }
                    }

                    let pk_json_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();

                    let db_backend = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::get_database_backend(db);

                    // Build the placeholder list. Per-backend dialect
                    // matches the inner `Builder` renderer: Postgres
                    // uses `$N`, others use `?`. `parents.is_empty()`
                    // already short-circuited above so the bind list
                    // is non-empty here.
                    let mut placeholders: ::std::vec::Vec<::std::string::String> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    let mut binds: ::std::vec::Vec<::suprnova::sea_orm::Value> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    for (i, v) in pk_json_values.iter().enumerate() {
                        let ph = match db_backend {
                            ::suprnova::sea_orm::DatabaseBackend::Postgres => {
                                ::std::format!("${}", i + 1)
                            }
                            _ => ::std::string::String::from("?"),
                        };
                        placeholders.push(ph);
                        binds.push(
                            ::suprnova::eloquent::model::json_value_to_sea_value(v),
                        );
                    }

                    // `CAST(... AS CHAR)` on MySQL, `CAST(... AS TEXT)`
                    // elsewhere — both yield the raw stringified column
                    // value the parent-side key derivation matches.
                    let __sn_cast_kw = match db_backend {
                        ::suprnova::sea_orm::DatabaseBackend::MySql => "CHAR",
                        _ => "TEXT",
                    };
                    let __sn_table = <#target_ty as
                        ::suprnova::eloquent::EloquentModel>::TABLE;

                    // The aggregate expression is selected at runtime
                    // from `kind`. The `column` arg flows untyped into
                    // SQL — identical concern to `#fk` in the count
                    // arm; T9's user-facing Builder surface owns column
                    // validation, the dispatcher doesn't widen the
                    // contract.
                    let __sn_agg_expr: ::std::string::String = match kind {
                        ::suprnova::AggregateKind::Sum => {
                            ::std::format!("SUM({})", column)
                        }
                        ::suprnova::AggregateKind::Avg => {
                            ::std::format!("AVG({})", column)
                        }
                        ::suprnova::AggregateKind::Min => {
                            ::std::format!("MIN({})", column)
                        }
                        ::suprnova::AggregateKind::Max => {
                            ::std::format!("MAX({})", column)
                        }
                    };

                    let __sn_sql = ::std::format!(
                        "SELECT CAST({fk} AS {cast}) AS __sn_fk_key, \
                                {agg} AS __sn_agg \
                           FROM {table} \
                          WHERE {fk} IN ({phs}) \
                          GROUP BY {fk}",
                        fk = #fk,
                        cast = __sn_cast_kw,
                        agg = __sn_agg_expr,
                        table = __sn_table,
                        phs = placeholders.join(", "),
                    );

                    let stmt = ::suprnova::sea_orm::Statement::from_sql_and_values(
                        db_backend,
                        &__sn_sql,
                        binds,
                    );
                    let rows = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::query_all(db, stmt)
                        .await
                        .map_err(|e| ::suprnova::FrameworkError::database(e.to_string()))?;

                    use ::std::collections::HashMap;
                    // `Option<f64>` so AVG (and SUM) over zero rows —
                    // which manifests as SQL NULL — survives the read
                    // and falls through the Sum|Avg vs Min|Max branch
                    // at distribution time. For HasMany the row map is
                    // only populated for parents with at least one
                    // child row, so empty groups never even appear as
                    // a `Some(_)` here; the missing-key path on the
                    // parent loop handles those.
                    //
                    // The `__sn_agg` column may come back as an integer
                    // on SQLite when the source column is INTEGER
                    // (e.g. SUM(views) on INTEGER yields INTEGER, not
                    // REAL). `try_get::<Option<f64>>` would silently
                    // fail that coercion and the dispatcher would
                    // store 0.0 for every parent. Try `f64` first,
                    // then `i64` widened to f64 as a fallback.
                    let mut by_fk: HashMap<
                        ::std::string::String,
                        ::core::option::Option<f64>,
                    > = HashMap::new();
                    for r in rows.iter() {
                        let key: ::std::string::String = r
                            .try_get::<::std::string::String>("", "__sn_fk_key")
                            .unwrap_or_default();
                        let agg: ::core::option::Option<f64> = r
                            .try_get::<::core::option::Option<f64>>("", "__sn_agg")
                            .ok()
                            .flatten()
                            .or_else(|| {
                                r.try_get::<::core::option::Option<i64>>("", "__sn_agg")
                                    .ok()
                                    .flatten()
                                    .map(|n| n as f64)
                            });
                        by_fk.insert(key, agg);
                    }

                    for p in parents.iter_mut() {
                        let key = __sn_parent_key_to_match_cast(
                            ::suprnova::serde_json::to_value(&p.#pk_ident)
                                .unwrap_or(::suprnova::serde_json::Value::Null),
                        );
                        // Missing-key (parent had no child rows) and
                        // present-but-NULL collapse into the same
                        // `None` branch for the per-kind distribution.
                        let agg: ::core::option::Option<f64> = by_fk
                            .get(&key)
                            .copied()
                            .unwrap_or(::core::option::Option::None);
                        match kind {
                            ::suprnova::AggregateKind::Sum
                            | ::suprnova::AggregateKind::Avg => {
                                p.__eager.set_aggregate::<f64>(
                                    #name_str,
                                    agg.unwrap_or(0.0),
                                );
                            }
                            ::suprnova::AggregateKind::Min
                            | ::suprnova::AggregateKind::Max => {
                                p.__eager.set_aggregate::<::core::option::Option<f64>>(
                                    #name_str,
                                    agg,
                                );
                            }
                        }
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::BelongsToMany => {
            // BelongsToMany aggregate is over the RELATED table's
            // columns (Laravel parity — users typically aggregate over
            // role.weight, not pivot.assigned_at). The dispatcher JOINs
            // the pivot to the related table and groups by the pivot's
            // FK column.
            //
            //   SELECT CAST(p.fk AS TEXT|CHAR) AS __sn_fk_key,
            //          AGG(r.col)              AS __sn_agg
            //     FROM <pivot_table> p
            //     JOIN <related_table> r ON r.id = p.<related_key>
            //    WHERE p.<fk> IN (...)
            //    GROUP BY p.<fk>
            //
            // Sum/Avg → f64 with 0.0 empty default. Min/Max →
            // Option<f64> with None empty default. Matches HasMany's
            // contract.
            let pivot_ty = rel.through.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(
                    &rel.name,
                    "BelongsToMany requires a pivot type (parser bug if reached)",
                )
            })?;
            let pivot_fk = pivot_fk_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}_id", to_snake(&parent_name)));
            let pivot_related = pivot_related_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    format!("{}_id", to_snake(&last_segment_name(target_ty)))
                });
            // Related-side PK column. Defaults to `"id"`. When the user
            // declares `related_key = "uuid"` on the relation, the JOIN
            // reads `__sn_r.uuid = __sn_p.{rk}` instead of the broken
            // `__sn_r.id = ...` form.
            let related_pk = related_key_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "id".to_string());
            let pivot_table_expr: TokenStream = match pivot_table_override(rel) {
                Some(t) => {
                    let lit = syn::LitStr::new(t, proc_macro2::Span::call_site());
                    quote! { #lit }
                }
                None => quote! {
                    <#pivot_ty as ::suprnova::eloquent::EloquentModel>::TABLE
                },
            };

            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }

                    fn __sn_parent_key_to_match_cast(
                        v: ::suprnova::serde_json::Value,
                    ) -> ::std::string::String {
                        match v {
                            ::suprnova::serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        }
                    }

                    let pk_json_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();

                    let db_backend = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::get_database_backend(db);

                    let mut placeholders: ::std::vec::Vec<::std::string::String> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    let mut binds: ::std::vec::Vec<::suprnova::sea_orm::Value> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    for (i, v) in pk_json_values.iter().enumerate() {
                        let ph = match db_backend {
                            ::suprnova::sea_orm::DatabaseBackend::Postgres => {
                                ::std::format!("${}", i + 1)
                            }
                            _ => ::std::string::String::from("?"),
                        };
                        placeholders.push(ph);
                        binds.push(
                            ::suprnova::eloquent::model::json_value_to_sea_value(v),
                        );
                    }

                    let __sn_cast_kw = match db_backend {
                        ::suprnova::sea_orm::DatabaseBackend::MySql => "CHAR",
                        _ => "TEXT",
                    };
                    let __sn_pivot = #pivot_table_expr;
                    let __sn_related = <#target_ty as
                        ::suprnova::eloquent::EloquentModel>::TABLE;

                    let __sn_agg_expr: ::std::string::String = match kind {
                        ::suprnova::AggregateKind::Sum => {
                            ::std::format!("SUM(__sn_r.{})", column)
                        }
                        ::suprnova::AggregateKind::Avg => {
                            ::std::format!("AVG(__sn_r.{})", column)
                        }
                        ::suprnova::AggregateKind::Min => {
                            ::std::format!("MIN(__sn_r.{})", column)
                        }
                        ::suprnova::AggregateKind::Max => {
                            ::std::format!("MAX(__sn_r.{})", column)
                        }
                    };

                    let __sn_sql = ::std::format!(
                        "SELECT CAST(__sn_p.{fk} AS {cast}) AS __sn_fk_key, \
                                {agg} AS __sn_agg \
                           FROM {pivot} __sn_p \
                           JOIN {related} __sn_r ON __sn_r.{related_pk} = __sn_p.{rk} \
                          WHERE __sn_p.{fk} IN ({phs}) \
                          GROUP BY __sn_p.{fk}",
                        fk = #pivot_fk,
                        rk = #pivot_related,
                        related_pk = #related_pk,
                        cast = __sn_cast_kw,
                        agg = __sn_agg_expr,
                        pivot = __sn_pivot,
                        related = __sn_related,
                        phs = placeholders.join(", "),
                    );

                    let stmt = ::suprnova::sea_orm::Statement::from_sql_and_values(
                        db_backend,
                        &__sn_sql,
                        binds,
                    );
                    let rows = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::query_all(db, stmt)
                        .await
                        .map_err(|e| ::suprnova::FrameworkError::database(e.to_string()))?;

                    use ::std::collections::HashMap;
                    let mut by_fk: HashMap<
                        ::std::string::String,
                        ::core::option::Option<f64>,
                    > = HashMap::new();
                    for r in rows.iter() {
                        let key: ::std::string::String = r
                            .try_get::<::std::string::String>("", "__sn_fk_key")
                            .unwrap_or_default();
                        let agg: ::core::option::Option<f64> = r
                            .try_get::<::core::option::Option<f64>>("", "__sn_agg")
                            .ok()
                            .flatten()
                            .or_else(|| {
                                r.try_get::<::core::option::Option<i64>>("", "__sn_agg")
                                    .ok()
                                    .flatten()
                                    .map(|n| n as f64)
                            });
                        by_fk.insert(key, agg);
                    }

                    for p in parents.iter_mut() {
                        let key = __sn_parent_key_to_match_cast(
                            ::suprnova::serde_json::to_value(&p.#pk_ident)
                                .unwrap_or(::suprnova::serde_json::Value::Null),
                        );
                        let agg: ::core::option::Option<f64> = by_fk
                            .get(&key)
                            .copied()
                            .unwrap_or(::core::option::Option::None);
                        match kind {
                            ::suprnova::AggregateKind::Sum
                            | ::suprnova::AggregateKind::Avg => {
                                p.__eager.set_aggregate::<f64>(
                                    #name_str,
                                    agg.unwrap_or(0.0),
                                );
                            }
                            ::suprnova::AggregateKind::Min
                            | ::suprnova::AggregateKind::Max => {
                                p.__eager.set_aggregate::<::core::option::Option<f64>>(
                                    #name_str,
                                    agg,
                                );
                            }
                        }
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::HasManyThrough | RelationKindAttr::HasOneThrough => {
            // Through aggregate is over the TARGET (C) table's
            // columns. The dispatcher JOINs C to B and groups by the
            // intermediate's first_key column. Same SQL skeleton as
            // BelongsToMany's aggregate, except the JOIN connects
            // C.{second_key} to B.id (not a pivot table's two FKs).
            //
            //   SELECT CAST(b.<first_key> AS TEXT|CHAR) AS __sn_fk_key,
            //          AGG(c.<col>)                     AS __sn_agg
            //     FROM <C> __sn_c
            //     JOIN <B> __sn_b ON __sn_c.<second_key> = __sn_b.id
            //    WHERE __sn_b.<first_key> IN (...)
            //    GROUP BY __sn_b.<first_key>
            //
            // Sum/Avg → f64 with 0.0 empty default. Min/Max →
            // Option<f64> with None empty default. Matches the
            // HasMany / BelongsToMany contract.
            //
            // Type rebinding: same swap as the eager + count arms —
            // for Through kinds `(rel.target, rel.through)` is
            // `(B, C)`. Shadow the function-scope `target_ty` with
            // the final target `C`.
            let through_ty = &rel.target; // intermediate B
            let target_ty = rel.through.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(
                    &rel.name,
                    "HasOneThrough / HasManyThrough require a final target type \
                     (parser bug if reached)",
                )
            })?; // final target C
            let first_key = first_key_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}_id", to_snake(&parent_name)));
            let second_key = second_key_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    format!("{}_id", to_snake(&last_segment_name(through_ty)))
                });
            // JOIN-target column on B. Defaults to `"id"`; overridable
            // via `second_local_key = "..."` for intermediates with a
            // non-`id` PK.
            let second_local_key = second_local_key_override(rel)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "id".to_string());

            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }

                    fn __sn_parent_key_to_match_cast(
                        v: ::suprnova::serde_json::Value,
                    ) -> ::std::string::String {
                        match v {
                            ::suprnova::serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        }
                    }

                    let pk_json_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();

                    let db_backend = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::get_database_backend(db);

                    let mut placeholders: ::std::vec::Vec<::std::string::String> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    let mut binds: ::std::vec::Vec<::suprnova::sea_orm::Value> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    for (i, v) in pk_json_values.iter().enumerate() {
                        let ph = match db_backend {
                            ::suprnova::sea_orm::DatabaseBackend::Postgres => {
                                ::std::format!("${}", i + 1)
                            }
                            _ => ::std::string::String::from("?"),
                        };
                        placeholders.push(ph);
                        binds.push(
                            ::suprnova::eloquent::model::json_value_to_sea_value(v),
                        );
                    }

                    let __sn_cast_kw = match db_backend {
                        ::suprnova::sea_orm::DatabaseBackend::MySql => "CHAR",
                        _ => "TEXT",
                    };
                    let __sn_b_table = <#through_ty as
                        ::suprnova::eloquent::EloquentModel>::TABLE;
                    let __sn_c_table = <#target_ty as
                        ::suprnova::eloquent::EloquentModel>::TABLE;

                    let __sn_agg_expr: ::std::string::String = match kind {
                        ::suprnova::AggregateKind::Sum => {
                            ::std::format!("SUM(__sn_c.{})", column)
                        }
                        ::suprnova::AggregateKind::Avg => {
                            ::std::format!("AVG(__sn_c.{})", column)
                        }
                        ::suprnova::AggregateKind::Min => {
                            ::std::format!("MIN(__sn_c.{})", column)
                        }
                        ::suprnova::AggregateKind::Max => {
                            ::std::format!("MAX(__sn_c.{})", column)
                        }
                    };

                    let __sn_sql = ::std::format!(
                        "SELECT CAST(__sn_b.{fk} AS {cast}) AS __sn_fk_key, \
                                {agg} AS __sn_agg \
                           FROM {c_table} __sn_c \
                           JOIN {b_table} __sn_b \
                             ON __sn_c.{second_key} = __sn_b.{slk} \
                          WHERE __sn_b.{fk} IN ({phs}) \
                          GROUP BY __sn_b.{fk}",
                        fk = #first_key,
                        second_key = #second_key,
                        slk = #second_local_key,
                        cast = __sn_cast_kw,
                        agg = __sn_agg_expr,
                        c_table = __sn_c_table,
                        b_table = __sn_b_table,
                        phs = placeholders.join(", "),
                    );

                    let stmt = ::suprnova::sea_orm::Statement::from_sql_and_values(
                        db_backend,
                        &__sn_sql,
                        binds,
                    );
                    let rows = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::query_all(db, stmt)
                        .await
                        .map_err(|e| ::suprnova::FrameworkError::database(e.to_string()))?;

                    use ::std::collections::HashMap;
                    let mut by_fk: HashMap<
                        ::std::string::String,
                        ::core::option::Option<f64>,
                    > = HashMap::new();
                    for r in rows.iter() {
                        let key: ::std::string::String = r
                            .try_get::<::std::string::String>("", "__sn_fk_key")
                            .unwrap_or_default();
                        let agg: ::core::option::Option<f64> = r
                            .try_get::<::core::option::Option<f64>>("", "__sn_agg")
                            .ok()
                            .flatten()
                            .or_else(|| {
                                r.try_get::<::core::option::Option<i64>>("", "__sn_agg")
                                    .ok()
                                    .flatten()
                                    .map(|n| n as f64)
                            });
                        by_fk.insert(key, agg);
                    }

                    for p in parents.iter_mut() {
                        let key = __sn_parent_key_to_match_cast(
                            ::suprnova::serde_json::to_value(&p.#pk_ident)
                                .unwrap_or(::suprnova::serde_json::Value::Null),
                        );
                        let agg: ::core::option::Option<f64> = by_fk
                            .get(&key)
                            .copied()
                            .unwrap_or(::core::option::Option::None);
                        match kind {
                            ::suprnova::AggregateKind::Sum
                            | ::suprnova::AggregateKind::Avg => {
                                p.__eager.set_aggregate::<f64>(
                                    #name_str,
                                    agg.unwrap_or(0.0),
                                );
                            }
                            ::suprnova::AggregateKind::Min
                            | ::suprnova::AggregateKind::Max => {
                                p.__eager.set_aggregate::<::core::option::Option<f64>>(
                                    #name_str,
                                    agg,
                                );
                            }
                        }
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        RelationKindAttr::MorphMany | RelationKindAttr::MorphOne => {
            // Server-side GROUP BY aggregate over the child table.
            // Same SQL skeleton as the HasMany aggregate arm but with
            // the extra `<name>_type = '<morph_type>'` predicate so
            // aggregates of children pointing at OTHER morph families
            // are excluded.
            //
            //   SELECT CAST(<id_col> AS TEXT|CHAR) AS __sn_fk_key,
            //          <AGG>(<col>)                AS __sn_agg
            //     FROM <child_table>
            //    WHERE <id_col> IN (?, ?, ...)
            //      AND <type_col> = ?
            //    GROUP BY <id_col>
            //
            // Sum/Avg → f64 with 0.0 empty default. Min/Max →
            // Option<f64> with None empty default. Matches the
            // HasMany contract.
            //
            // MorphOne aggregates work the same way — the per-parent
            // group is 0-or-1 row by contract; the server-side GROUP
            // BY collapses to the single row's column value (or NULL
            // when no row matches, which falls through the Sum|Avg vs
            // Min|Max branch).
            let morph_name = morph_name_or_default(rel);
            let morph_type_value = morph_type_of(input);
            let id_col = format!("{morph_name}_id");
            let type_col = format!("{morph_name}_type");
            Ok(Some(quote! {
                #name_str => {
                    if parents.is_empty() { return ::core::result::Result::Ok(()); }

                    fn __sn_parent_key_to_match_cast(
                        v: ::suprnova::serde_json::Value,
                    ) -> ::std::string::String {
                        match v {
                            ::suprnova::serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        }
                    }

                    let pk_json_values: ::std::vec::Vec<::suprnova::serde_json::Value> = parents
                        .iter()
                        .map(|p| ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .unwrap_or(::suprnova::serde_json::Value::Null))
                        .collect();

                    let db_backend = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::get_database_backend(db);

                    let mut placeholders: ::std::vec::Vec<::std::string::String> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len());
                    let mut binds: ::std::vec::Vec<::suprnova::sea_orm::Value> =
                        ::std::vec::Vec::with_capacity(pk_json_values.len() + 1);
                    for (i, v) in pk_json_values.iter().enumerate() {
                        let ph = match db_backend {
                            ::suprnova::sea_orm::DatabaseBackend::Postgres => {
                                ::std::format!("${}", i + 1)
                            }
                            _ => ::std::string::String::from("?"),
                        };
                        placeholders.push(ph);
                        binds.push(
                            ::suprnova::eloquent::model::json_value_to_sea_value(v),
                        );
                    }
                    let type_ph = match db_backend {
                        ::suprnova::sea_orm::DatabaseBackend::Postgres => {
                            ::std::format!("${}", pk_json_values.len() + 1)
                        }
                        _ => ::std::string::String::from("?"),
                    };
                    binds.push(::suprnova::sea_orm::Value::from(#morph_type_value));

                    let __sn_cast_kw = match db_backend {
                        ::suprnova::sea_orm::DatabaseBackend::MySql => "CHAR",
                        _ => "TEXT",
                    };
                    let __sn_table = <#target_ty as
                        ::suprnova::eloquent::EloquentModel>::TABLE;

                    let __sn_agg_expr: ::std::string::String = match kind {
                        ::suprnova::AggregateKind::Sum => {
                            ::std::format!("SUM({})", column)
                        }
                        ::suprnova::AggregateKind::Avg => {
                            ::std::format!("AVG({})", column)
                        }
                        ::suprnova::AggregateKind::Min => {
                            ::std::format!("MIN({})", column)
                        }
                        ::suprnova::AggregateKind::Max => {
                            ::std::format!("MAX({})", column)
                        }
                    };

                    let __sn_sql = ::std::format!(
                        "SELECT CAST({id} AS {cast}) AS __sn_fk_key, \
                                {agg} AS __sn_agg \
                           FROM {table} \
                          WHERE {id} IN ({phs}) \
                            AND {type_col} = {type_ph} \
                          GROUP BY {id}",
                        id = #id_col,
                        cast = __sn_cast_kw,
                        agg = __sn_agg_expr,
                        table = __sn_table,
                        type_col = #type_col,
                        type_ph = type_ph,
                        phs = placeholders.join(", "),
                    );

                    let stmt = ::suprnova::sea_orm::Statement::from_sql_and_values(
                        db_backend,
                        &__sn_sql,
                        binds,
                    );
                    let rows = <::suprnova::sea_orm::DatabaseConnection as
                        ::suprnova::sea_orm::ConnectionTrait>::query_all(db, stmt)
                        .await
                        .map_err(|e| ::suprnova::FrameworkError::database(e.to_string()))?;

                    use ::std::collections::HashMap;
                    let mut by_fk: HashMap<
                        ::std::string::String,
                        ::core::option::Option<f64>,
                    > = HashMap::new();
                    for r in rows.iter() {
                        let key: ::std::string::String = r
                            .try_get::<::std::string::String>("", "__sn_fk_key")
                            .unwrap_or_default();
                        let agg: ::core::option::Option<f64> = r
                            .try_get::<::core::option::Option<f64>>("", "__sn_agg")
                            .ok()
                            .flatten()
                            .or_else(|| {
                                r.try_get::<::core::option::Option<i64>>("", "__sn_agg")
                                    .ok()
                                    .flatten()
                                    .map(|n| n as f64)
                            });
                        by_fk.insert(key, agg);
                    }

                    for p in parents.iter_mut() {
                        let key = __sn_parent_key_to_match_cast(
                            ::suprnova::serde_json::to_value(&p.#pk_ident)
                                .unwrap_or(::suprnova::serde_json::Value::Null),
                        );
                        let agg: ::core::option::Option<f64> = by_fk
                            .get(&key)
                            .copied()
                            .unwrap_or(::core::option::Option::None);
                        match kind {
                            ::suprnova::AggregateKind::Sum
                            | ::suprnova::AggregateKind::Avg => {
                                p.__eager.set_aggregate::<f64>(
                                    #name_str,
                                    agg.unwrap_or(0.0),
                                );
                            }
                            ::suprnova::AggregateKind::Min
                            | ::suprnova::AggregateKind::Max => {
                                p.__eager.set_aggregate::<::core::option::Option<f64>>(
                                    #name_str,
                                    agg,
                                );
                            }
                        }
                    }
                    return ::core::result::Result::Ok(());
                }
            }))
        }
        _ => Ok(None),
    }
}

/// `__recurse_eager_load` arm — T2 doesn't ship nested eager loading
/// (T9 owns the orchestrator). Returning `None` keeps the dispatcher
/// quiet on the head segment; nested paths through HasOne / BelongsTo
/// will land in T9 when the full nested-path resolver does.
fn emit_recurse_arm(_input: &ModelInput, _rel: &RelationDecl) -> Result<Option<TokenStream>> {
    Ok(None)
}

/// Emit `Self::with([...])` — the minimal eager-load entrypoint T2
/// ships so the eager-load test in `eloquent_relations_one_to_one.rs`
/// can run. T9 will expand this with `with_count` / `with_sum`-`max`
/// / `with_where` / nested-path resolution. For T2 we only need the
/// flat list of relation names + a `Builder<Self>` that invokes the
/// per-model `__eager_load` dispatcher for each name at fetch time.
///
/// The wired path:
///
/// 1. `Self::with(["profile"])` returns a `Builder<Self>` with an
///    eager spec list attached.
/// 2. `Builder::get` (on a builder carrying eager specs) issues the
///    base SELECT, calls `M::__eager_load(name, &mut [&mut row, ...], db, None)`
///    for each spec, and returns the rows with their `__eager` cache
///    populated.
fn emit_with_helper(struct_ident: &syn::Ident) -> TokenStream {
    quote! {
        impl #struct_ident {
            #[doc = "Open a `Builder<Self>` that eager-loads the listed relations."]
            #[doc = ""]
            #[doc = "Each name is resolved against the model's `__eager_load` dispatcher \
                     at fetch time. T9 extends this with `with_count` / `with_sum`-`max` \
                     / `with_where` / nested-path (`\"posts.comments\"`) resolution; T2 \
                     only ships the flat-list form."]
            pub fn with<I, S>(relations: I) -> ::suprnova::Builder<Self>
            where
                I: ::core::iter::IntoIterator<Item = S>,
                S: ::core::convert::Into<::std::string::String>,
            {
                <Self as ::suprnova::eloquent::Model>::query()
                    .with(relations)
            }
        }
    }
}

/// Render a [`syn::Type`] back to its compact source form for the
/// inventory's `target_type_name` literal.
///
/// `proc_macro2::TokenStream::to_string()` inserts spaces between
/// every adjacent token pair, so a type written as `Vec<Post>` round
/// trips through `quote!(#ty).to_string()` as `"Vec < Post >"`. Phase
/// 8 admin renders this string in the relation listing UI — the
/// padded form is visually wrong. Stripping every space yields the
/// compact `"Vec<Post>"` / `"Option<i64>"` form users actually wrote.
///
/// This is correct for the common cases (single idents, generic
/// applications, qualified paths). The rare case of a function-typed
/// target (`Box<dyn Fn(i32) -> bool>`) would have its inner spaces
/// stripped too — but relation targets are model structs, not closure
/// types, so the trade-off is fine. If we ever need fancier formatting
/// we can swap this for a `syn::Type` walker.
fn format_target_type(ty: &syn::Type) -> String {
    quote::quote!(#ty).to_string().replace(' ', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_target_type_strips_spaces_around_generics() {
        // Bare ident — pass-through.
        let ty: syn::Type = syn::parse_str("Post").unwrap();
        assert_eq!(format_target_type(&ty), "Post");
    }

    #[test]
    fn format_target_type_vec_target_round_trips_without_spaces() {
        // `Vec<Post>` is the common collection-of-models shape — must
        // never appear as `"Vec < Post >"` in the admin UI.
        let ty: syn::Type = syn::parse_str("Vec<Post>").unwrap();
        assert_eq!(format_target_type(&ty), "Vec<Post>");
    }

    #[test]
    fn format_target_type_option_target_round_trips_without_spaces() {
        // `Option<i64>` is what nullable FK fields would surface as if
        // ever used as a target ident. Same no-padding rule.
        let ty: syn::Type = syn::parse_str("Option<i64>").unwrap();
        assert_eq!(format_target_type(&ty), "Option<i64>");
    }

    #[test]
    fn format_target_type_qualified_path_round_trips_without_spaces() {
        // Fully qualified `crate::models::Post` should keep its colons
        // and lose any `quote!`-inserted padding.
        let ty: syn::Type = syn::parse_str("crate::models::Post").unwrap();
        assert_eq!(format_target_type(&ty), "crate::models::Post");
    }

    #[test]
    fn format_target_type_nested_generics_round_trip_without_spaces() {
        // Nested generic — pivot models that are themselves generic
        // round-trip cleanly.
        let ty: syn::Type = syn::parse_str("Vec<Option<Post>>").unwrap();
        assert_eq!(format_target_type(&ty), "Vec<Option<Post>>");
    }
}
