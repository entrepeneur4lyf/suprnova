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
        // T4-T7 own the rest of the kinds (BelongsToMany, Through,
        // Morph*). For T3 we emit nothing for those so the macro
        // still accepts the declaration — the method just doesn't
        // exist yet, which is fine because no test code calls it.
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

            // Real per-parent GROUP aggregate — distinct from T2's
            // HasOne arm which records a single row's column value
            // (HasOne has 0-or-1 rows per group). HasMany aggregates
            // over the full per-parent slice:
            //
            //   Sum:  Σ col_v
            //   Avg:  Σ col_v / N  (N = group size, > 0)
            //   Min:  min(col_v)
            //   Max:  max(col_v)
            //
            // Per parent we accumulate a single `Vec<f64>` of column
            // values keyed by the FK string. Empty groups fall
            // through the existing Sum|Avg vs Min|Max branch (Sum/Avg
            // → 0.0, Min/Max → Option::None). Non-empty groups always
            // produce Some(value) for Min/Max.
            //
            // Cache key is the relation name only — same caveat as
            // T2's HasOne arm. T9 will widen to
            // <rel>_<kind>_<col> when the `with_<agg>` Builder surface
            // ships so multiple aggregates on the same relation can
            // coexist on a single row without clobbering each other.
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
                    // Per-FK accumulation of column values. We collect
                    // the raw list and reduce on the per-parent pass
                    // below; this keeps the per-row hot path branch-
                    // free regardless of `kind`.
                    let mut by_fk: HashMap<::std::string::String, ::std::vec::Vec<f64>>
                        = HashMap::new();
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
                        by_fk.entry(key).or_default().push(col_val);
                    }
                    for p in parents.iter_mut() {
                        let key = ::suprnova::serde_json::to_value(&p.#pk_ident)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        let vals: ::std::vec::Vec<f64> = by_fk.remove(&key).unwrap_or_default();
                        match kind {
                            ::suprnova::AggregateKind::Sum => {
                                let sum: f64 = vals.iter().sum();
                                p.__eager.set_aggregate::<f64>(#name_str, sum);
                            }
                            ::suprnova::AggregateKind::Avg => {
                                let n = vals.len();
                                let avg: f64 = if n == 0 {
                                    0.0
                                } else {
                                    let sum: f64 = vals.iter().sum();
                                    sum / (n as f64)
                                };
                                p.__eager.set_aggregate::<f64>(#name_str, avg);
                            }
                            ::suprnova::AggregateKind::Min => {
                                let m: ::core::option::Option<f64> = if vals.is_empty() {
                                    ::core::option::Option::None
                                } else {
                                    // f64 lacks Ord — use partial-cmp
                                    // and fall back to f64::INFINITY
                                    // as a guard for NaN. Production
                                    // columns shouldn't contain NaN;
                                    // a guarded fallback beats a
                                    // panic in the dispatcher.
                                    ::core::option::Option::Some(
                                        vals.iter()
                                            .copied()
                                            .fold(f64::INFINITY, f64::min),
                                    )
                                };
                                p.__eager
                                    .set_aggregate::<::core::option::Option<f64>>(#name_str, m);
                            }
                            ::suprnova::AggregateKind::Max => {
                                let m: ::core::option::Option<f64> = if vals.is_empty() {
                                    ::core::option::Option::None
                                } else {
                                    ::core::option::Option::Some(
                                        vals.iter()
                                            .copied()
                                            .fold(f64::NEG_INFINITY, f64::max),
                                    )
                                };
                                p.__eager
                                    .set_aggregate::<::core::option::Option<f64>>(#name_str, m);
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
