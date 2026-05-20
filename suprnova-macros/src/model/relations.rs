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

use super::parse::{ModelInput, RelationDecl, RelationKindAttr};

/// Top-level entry point. Emits every relation-related artifact for
/// the model (dispatchers + accessors + inventory submissions).
pub fn emit(input: &ModelInput) -> Result<TokenStream> {
    let struct_ident = &input.item.ident;
    let dispatchers = emit_dispatchers(struct_ident);
    let pivot_accessor = emit_pivot_accessor(struct_ident);

    // Build per-relation accessors + inventory submissions. The
    // accessors live in their own `impl Self { ... }` block, kept
    // separate from the dispatchers so a subsequent
    // `cargo expand` clearly shows which methods came from which
    // relation declarations.
    let (relation_accessors, relation_inventory): (Vec<_>, Vec<_>) = input
        .relations
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|rel| {
            (
                emit_relation_accessors(struct_ident, rel),
                emit_relation_inventory(struct_ident, rel),
            )
        })
        .unzip();

    Ok(quote! {
        #dispatchers
        #pivot_accessor
        #( #relation_accessors )*
        #( #relation_inventory )*
    })
}

/// Emit the four dispatcher methods + skeleton matches. T1 ships them
/// as no-relation error paths; T2-T7 add a `<name> => { ... }` arm
/// each per concrete relation. The `predicate` parameter on
/// `__eager_load` carries the user's optional `with_where` closure
/// type-erased — concrete arms downcast it before applying.
fn emit_dispatchers(struct_ident: &syn::Ident) -> TokenStream {
    quote! {
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
                let _ = (parents, db, predicate);
                match relation {
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
                let _ = (parents, db);
                match relation {
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
                let _ = (column, kind, parents, db);
                match relation {
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
    }
}

/// Emit the `pivot::<P>()` accessor. T4 (BelongsToMany) fills
/// `__pivot` on each row at load time; this accessor reads it back.
/// Panics when the row has no pivot context, matching the spec's
/// explicit "clear error message" requirement.
fn emit_pivot_accessor(struct_ident: &syn::Ident) -> TokenStream {
    quote! {
        impl #struct_ident {
            /// Read pivot context attached by a `BelongsToMany` load.
            ///
            /// Panics if the row has no pivot context — typically
            /// because it was fetched via `find()` instead of through
            /// the m2m loader. T4 wires the loader to fill
            /// `__pivot`; T1 emits the accessor itself.
            pub fn pivot<P: ::std::any::Any + ::core::marker::Send + ::core::marker::Sync>(&self) -> &P {
                self.__pivot
                    .as_ref()
                    .and_then(|arc| arc.downcast_ref::<P>())
                    .unwrap_or_else(|| {
                        ::std::panic!(
                            "`{}` row has no pivot context; load via `BelongsToMany::get()`",
                            ::std::any::type_name::<Self>(),
                        )
                    })
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
    let target_ty = &rel.target;

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
    let target_ty = &rel.target;
    let kind_variant = kind_to_runtime(rel.kind);
    // `RelationEntry::target_type_name` is `&'static str`, so we need
    // a string literal at macro expansion time — `type_name::<T>()`
    // isn't a `const fn` and can't be used in an `inventory::submit!`
    // constant initialiser. `stringify!(#target_ty)` gives us the
    // type as it was written at the declaration site, which is what
    // tooling (Phase 8 admin) wants to show anyway.
    let target_type_lit = quote::quote!(#target_ty).to_string();
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
