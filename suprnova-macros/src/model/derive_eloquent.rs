//! Emits Eloquent trait impls for the user's struct.
//!
//! Task 3 shipped the `EloquentModel` marker — enough to register the
//! type as a Suprnova model and bridge the user's struct name (e.g.
//! `User`) to the inner-module SeaORM `Entity` / `Column` types
//! (`user::Entity`, `user::Column`).
//!
//! Task 4 (this expansion) adds:
//! - `From<#inner::Model> for User` and `From<User> for #inner::Model`
//!   so the trait surface can round-trip between Suprnova and SeaORM
//! - `Default for User` so `replicate` can build a fresh shell
//! - `impl Model for User` with the per-model hook fills
//!   (`primary_key_value`, `apply_attrs_to_active_model`, ...)
//! - `impl ReplicateExt for User` with field-by-field clone
//! - `impl FirstOrCreate for User` with the `from_attrs_unsaved` hook
//!
//! Tasks 6 / 7a-c / 8 / 9 / 10 each extend this file with their slice
//! of the generated impl (fillable, casts, accessors/mutators,
//! timestamps, soft deletes).

use proc_macro2::TokenStream;
use quote::quote;
use syn::Result;

use super::casts;
use super::parse::ModelInput;

pub fn emit(input: &ModelInput) -> Result<TokenStream> {
    let struct_ident = &input.item.ident;
    let module_name = input.module_name();
    // The literal table string from the macro attribute — used in the
    // `const TABLE` initialiser further down. SeaORM 1.1's
    // `EntityName::table_name` isn't `const fn`, so we can't call it
    // here; the parser captured the same value the runtime would
    // return.
    let table = &input.table;
    let pk_name = &input.primary_key;
    let pk_ident = quote::format_ident!("{pk_name}");

    // T6 — wire the parsed `fillable = [...]` / `guarded = [...]`
    // attributes into the per-model `fillable_filter()` impl. Mutual
    // exclusion was already enforced at parse time (parse.rs:67-72), so
    // the `(Some, Some)` arm here is unreachable.
    let fillable_impl = match (&input.fillable, &input.guarded) {
        (Some(list), None) => {
            let lits = list.iter().map(|s| quote! { #s });
            quote! {
                fn fillable_filter() -> ::suprnova::eloquent::Fillable {
                    ::suprnova::eloquent::Fillable::fillable(::std::vec![#(#lits),*])
                }
            }
        }
        (None, Some(list)) => {
            let lits = list.iter().map(|s| quote! { #s });
            quote! {
                fn fillable_filter() -> ::suprnova::eloquent::Fillable {
                    ::suprnova::eloquent::Fillable::guarded(::std::vec![#(#lits),*])
                }
            }
        }
        (None, None) => quote! {
            fn fillable_filter() -> ::suprnova::eloquent::Fillable {
                // T4 default — guard the macro-parsed PK name (NOT a
                // hardcoded "id") so models with `primary_key = "uid"`
                // still have their PK protected from mass assignment.
                ::suprnova::eloquent::Fillable::guarded(::std::vec![#pk_name])
            }
        },
        (Some(_), Some(_)) => unreachable!(
            "fillable / guarded mutual exclusion validated at parse time (parse.rs:67-72)"
        ),
    };

    let fields = match &input.item.fields {
        syn::Fields::Named(named) => &named.named,
        _ => unreachable!("validated in derive_seaorm"),
    };

    let field_idents: Vec<_> = fields
        .iter()
        .map(|f| f.ident.as_ref().expect("named").clone())
        .collect();
    let field_strs: Vec<String> = field_idents.iter().map(|i| i.to_string()).collect();

    // T7a — route per-field through `casts::apply_arm`. Fields with a
    // declared cast (in `input.casts`) decode JSON → Runtime →
    // `Cast::to_storage` → ActiveModel; uncast fields use the same
    // direct `serde_json::from_value` shape as before T7a.
    //
    // T8 — fields listed in `mutators = [...]` take precedence over
    // the cast/direct apply paths. The mutator arm builds a scratch
    // `Self::default()`, calls `scratch.set_<field>(val)?`, then
    // serialises the transformed value back into JSON and feeds it
    // into the same cast/direct apply logic. This means a field can
    // declare both a mutator and a cast; the mutator transforms the
    // runtime value, the cast handles the storage shape.
    let mutators_list = &input.mutators;
    let apply_arms = field_idents.iter().zip(field_strs.iter()).map(|(ident, name)| {
        if mutators_list.iter().any(|m| m == name) {
            casts::mutator_apply_arm(name, ident, input.cast_for_field(name))
        } else {
            casts::apply_arm(name, ident, input.cast_for_field(name))
        }
    });

    let replicate_arms = field_idents
        .iter()
        .zip(field_strs.iter())
        .map(|(ident, name)| {
            let is_pk = name == pk_name;
            if is_pk {
                quote! { #ident: ::core::default::Default::default() }
            } else {
                quote! {
                    #ident: if except.iter().any(|e| e == #name) {
                        ::core::default::Default::default()
                    } else {
                        self.#ident.clone()
                    }
                }
            }
        });

    // T8 — when the field name is in `mutators = [...]`, route
    // through `s.set_<field>(value)?` so unsaved-instance builders
    // (`first_or_new`) apply the same transformation as `create` /
    // `update`. Non-mutator fields keep the direct
    // `serde_json::from_value` apply (which silently swallows decode
    // errors via `unwrap_or_default` — matching the pre-T8
    // first-or-new behaviour for non-mutator fields).
    let from_attrs_unsaved_arms = field_idents.iter().zip(field_strs.iter()).map(|(ident, name)| {
        if mutators_list.iter().any(|m| m == name) {
            let setter = quote::format_ident!("set_{}", ident);
            quote! {
                #name => {
                    s.#setter(v.clone())?;
                }
            }
        } else {
            quote! {
                #name => {
                    s.#ident = ::suprnova::serde_json::from_value(v.clone()).unwrap_or_default();
                }
            }
        }
    });

    // For `save()`, we need every non-PK field marked as Set so SeaORM
    // emits a real UPDATE statement. The SeaORM-derived
    // `IntoActiveModel<ActiveModel> for Model` produces all-Unchanged
    // values (see DeriveActiveModel's `impl From<Model> for ActiveModel`
    // at sea-orm-macros 1.1.20/src/derives/active_model.rs:92), which
    // makes the resulting UPDATE a no-op. So we build the ActiveModel
    // explicitly: PK as Unchanged (it's the WHERE clause), every other
    // field as Set.
    //
    // T7a — cast fields route through `Cast::to_storage` here so the
    // inner Model's Storage-typed field matches what `derive_seaorm`
    // emitted. Without this the ActiveModel write would type-check
    // against the user's Runtime type and miscompile.
    let active_model_for_update_arms = field_idents
        .iter()
        .zip(field_strs.iter())
        .map(|(ident, name)| {
            let is_pk = name == pk_name;
            casts::active_model_update_stmt(ident, is_pk, input.cast_for_field(name))
        });

    // T7a — read-direction arm for `From<inner::Model> for UserStruct`.
    // Cast fields call `Cast::from_storage` to inflate the storage
    // shape back into the runtime type.
    let from_storage_arms = field_idents
        .iter()
        .zip(field_strs.iter())
        .map(|(ident, name)| casts::from_storage_arm(ident, input.cast_for_field(name)));

    // T7a — write-direction arm for `From<UserStruct> for inner::Model`.
    // Cast fields call `Cast::to_storage` to flatten the runtime
    // shape back into the storage type the inner Model expects.
    let to_storage_arms = field_idents
        .iter()
        .zip(field_strs.iter())
        .map(|(ident, name)| casts::to_storage_arm(ident, input.cast_for_field(name)));

    // T8 — `to_json` integration. For every accessor name listed in
    // `appends = [...]`, call the method (declared in user code with
    // `#[accessor]`) and insert the JSON-encoded result under that
    // key. The macro doesn't try to validate that the method exists
    // — if it's missing, the call site produces a clear compiler
    // error pointing at the user's struct.
    let appends = &input.appends;
    let hidden = &input.hidden;
    let visible_opt = &input.visible;
    let append_inserts = appends.iter().map(|name| {
        let method = quote::format_ident!("{}", name);
        quote! {
            out.insert(
                #name.to_string(),
                ::suprnova::serde_json::to_value(&self.#method())
                    .unwrap_or(::suprnova::serde_json::Value::Null),
            );
        }
    });
    // T8 — filter applied to the base struct serialization.
    //
    // - `visible = [...]` is an allowlist: every column NOT in the
    //   list is dropped. Used for tightly-controlled API output.
    // - `hidden = [...]` is a denylist: every column in the list is
    //   dropped. Used for sensitive fields like `password`.
    //
    // Mutual exclusion is enforced in `parse.rs`. Appended accessors
    // bypass both filters (they're inserted after the base map is
    // filtered) — this matches Laravel: `$appends` always serialises.
    let filter_apply: TokenStream = match visible_opt {
        Some(list) => {
            let lits = list.iter().map(|s| quote! { #s });
            quote! {
                let visible_list: &[&str] = &[ #(#lits),* ];
                for (k, v) in map {
                    if visible_list.contains(&k.as_str()) {
                        out.insert(k, v);
                    }
                }
            }
        }
        None => {
            let lits = hidden.iter().map(|s| quote! { #s });
            quote! {
                let hidden_list: &[&str] = &[ #(#lits),* ];
                for (k, v) in map {
                    if !hidden_list.contains(&k.as_str()) {
                        out.insert(k, v);
                    }
                }
            }
        }
    };

    // T8 — `fill` body arms. Mutator-routed fields call
    // `self.set_<field>(value.clone())?`; non-mutator fields
    // direct-assign via `serde_json::from_value(...)`. We skip the
    // duplicate match-pattern hazard by partitioning the lists.
    let fill_mutator_arms = mutators_list.iter().map(|name| {
        let setter = quote::format_ident!("set_{}", name);
        quote! {
            #name => { self.#setter(v.clone())?; }
        }
    });
    let fill_direct_arms = field_idents.iter().zip(field_strs.iter()).filter_map(|(ident, name)| {
        if mutators_list.iter().any(|m| m == name) {
            // Mutator-listed: skip — its arm is emitted above. Emitting
            // both arms would produce duplicate match patterns and a
            // hard rustc error.
            None
        } else {
            Some(quote! {
                #name => {
                    self.#ident = ::suprnova::serde_json::from_value(v.clone()).unwrap_or_default();
                }
            })
        }
    });

    // T9 — auto-managed timestamps + `Touchable` impl + `touches`
    // marker. The macro auto-detects timestamps in `parse.rs` (both
    // columns present → enabled, neither → disabled, partial →
    // compile_error) and auto-injects `AsDateTime` casts for the
    // timestamp columns. Here we wire the runtime injections:
    //
    // - `apply_attrs_to_active_model`: AFTER the for-loop, always
    //   overwrite `updated_at` with NOW so both `create()` and
    //   `update(attrs)` bump it. Then set `created_at` to NOW only if
    //   it's still `NotSet` (create path). On `update(attrs)` it's
    //   `Unchanged(old)` from `into_active_model()`, which the check
    //   correctly leaves alone.
    //
    // - `into_active_model_for_update`: AFTER the per-field arms,
    //   overwrite `updated_at` with NOW so `save()` bumps it. The
    //   arms have already populated `created_at` with the in-memory
    //   value (Set), which we keep.
    //
    // - `impl Touchable for #struct_ident`: builds a minimal
    //   ActiveModel with PK Unchanged + updated_at Set(now), runs an
    //   UPDATE. No other column changes.
    //
    // - `TOUCHES` const: stores the parsed `touches = [...]` list
    //   even on models without timestamps. Phase 10B reads this to
    //   wire parent-touching post-save hooks once relations land.
    //   Today it's just a const — the cascade is a no-op.
    let timestamps_enabled = input.timestamps;
    let created_col_ident = quote::format_ident!("{}", input.created_at);
    let updated_col_ident = quote::format_ident!("{}", input.updated_at);

    let timestamp_inject_apply = if timestamps_enabled {
        quote! {
            // Always bump updated_at — covers create() and
            // update(attrs) in one place.
            let __suprnova_now = ::suprnova::chrono::Utc::now();
            am.#updated_col_ident = ::suprnova::sea_orm::Set(
                <::suprnova::AsDateTime as ::suprnova::eloquent::casts::Cast>::to_storage(
                    &__suprnova_now,
                )?,
            );
            // Set created_at only on first save (NotSet); update()
            // builds the AM from a row, so created_at is
            // Unchanged(old) and the check correctly leaves it alone.
            if matches!(
                am.#created_col_ident,
                ::suprnova::sea_orm::ActiveValue::NotSet
            ) {
                am.#created_col_ident = ::suprnova::sea_orm::Set(
                    <::suprnova::AsDateTime as ::suprnova::eloquent::casts::Cast>::to_storage(
                        &__suprnova_now,
                    )?,
                );
            }
        }
    } else {
        quote! {}
    };

    let timestamp_inject_save = if timestamps_enabled {
        quote! {
            // save() rebuilds the AM from `self`; the arms have
            // already populated updated_at with the in-memory value.
            // Overwrite with NOW so save() bumps the column even when
            // the caller didn't touch it.
            let __suprnova_now = ::suprnova::chrono::Utc::now();
            am.#updated_col_ident = ::suprnova::sea_orm::Set(
                <::suprnova::AsDateTime as ::suprnova::eloquent::casts::Cast>::to_storage(
                    &__suprnova_now,
                )?,
            );
        }
    } else {
        quote! {}
    };

    let touchable_impl = if timestamps_enabled {
        quote! {
            #[::suprnova::__async_trait::async_trait]
            impl ::suprnova::eloquent::Touchable for #struct_ident {
                async fn touch(&self) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                    let now = ::suprnova::chrono::Utc::now();
                    let mut am = <<#module_name::Entity as ::suprnova::EntityTrait>::ActiveModel
                        as ::core::default::Default>::default();
                    am.#pk_ident = ::suprnova::sea_orm::ActiveValue::Unchanged(self.#pk_ident.clone());
                    am.#updated_col_ident = ::suprnova::sea_orm::Set(
                        <::suprnova::AsDateTime as ::suprnova::eloquent::casts::Cast>::to_storage(
                            &now,
                        )?,
                    );
                    let db = ::suprnova::DB::connection()?;
                    <<#module_name::Entity as ::suprnova::EntityTrait>::ActiveModel
                        as ::suprnova::ActiveModelTrait>::update(am, db.inner())
                        .await
                        .map_err(|e| ::suprnova::FrameworkError::database(e.to_string()))?;
                    ::core::result::Result::Ok(())
                }
            }
        }
    } else {
        quote! {}
    };

    let touches_marker = if !input.touches.is_empty() {
        let touches_lits = input.touches.iter().map(|t| quote! { #t });
        quote! {
            impl #struct_ident {
                /// Names of relations whose parent rows should be
                /// "touched" (their `updated_at` bumped) when this
                /// model is saved. Populated by
                /// `#[model(touches = [...])]`.
                ///
                /// Phase 10B reads this to wire post-save hooks once
                /// the relations API lands; in 10A it's a static
                /// metadata only.
                pub const TOUCHES: &'static [&'static str] = &[ #(#touches_lits),* ];
            }
        }
    } else {
        quote! {}
    };

    // T10 — soft deletes. When `#[model(soft_deletes)]` is set:
    //
    // - `Model::query()` overrides to auto-apply `filter_null("deleted_at")`
    //   so default reads skip trashed rows. `with_trashed()` /
    //   `only_trashed()` construct their own unscoped Builder so they
    //   don't need to undo the scope.
    // - `impl SoftDeletes for #struct` exposes the column name + the
    //   `is_trashed()` accessor.
    // - Inherent `delete(self)` / `restore(self)` / `force_delete(self)`
    //   / `trashed(&self)` / `with_trashed()` / `only_trashed()` swap in
    //   the tombstone semantics. The lifecycle methods take `self` by
    //   value to match `Model::delete(self)`'s signature — Rust's
    //   method-resolution prefers an inherent method over a trait
    //   default only when they share auto-ref level, so an inherent
    //   `delete(&self)` override would silently lose to the trait's
    //   `delete(self)`.
    let soft_deletes_enabled = input.soft_deletes;
    let soft_delete_col = &input.soft_deletes_column;
    let soft_delete_col_ident = quote::format_ident!("{}", soft_delete_col);
    let key_type = &input.key_type;

    let query_override = if soft_deletes_enabled {
        quote! {
            fn query() -> ::suprnova::Builder<Self> {
                // Auto-apply the soft_deletes scope so default reads
                // skip trashed rows. with_trashed() / only_trashed()
                // build their own unscoped Builder directly — they
                // don't go through query() — so we don't need a
                // runtime check here. The
                // `without_global_scope("soft_deletes")` tag remains
                // informational for Phase 10C's custom-scope machinery.
                ::suprnova::Builder::<Self>::new().filter_null(#soft_delete_col)
            }
        }
    } else {
        quote! {}
    };

    let soft_deletes_impl = if soft_deletes_enabled {
        quote! {
            impl ::suprnova::eloquent::SoftDeletes for #struct_ident {
                fn deleted_at_column() -> &'static str { #soft_delete_col }
                fn is_trashed(&self) -> bool { self.#soft_delete_col_ident.is_some() }
            }

            impl #struct_ident {
                /// Soft-delete: `UPDATE table SET deleted_at = NOW()
                /// WHERE pk = ?` instead of DELETE. Takes `self` by
                /// value to override `Model::delete(self)` cleanly —
                /// a `&self` inherent override would lose to the
                /// trait default through auto-ref resolution.
                pub async fn delete(self) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                    let now = ::suprnova::chrono::Utc::now().to_rfc3339();
                    let table = <Self as ::suprnova::eloquent::EloquentModel>::TABLE;
                    let pk_name = <Self as ::suprnova::eloquent::Model>::primary_key_name();
                    let sql = ::std::format!(
                        "UPDATE {table} SET {} = ? WHERE {pk_name} = ?",
                        #soft_delete_col,
                    );
                    let db = ::suprnova::DB::connection()?;
                    let backend = <_ as ::suprnova::ConnectionTrait>::get_database_backend(db.inner());
                    <_ as ::suprnova::ConnectionTrait>::execute(
                        db.inner(),
                        ::suprnova::sea_orm::Statement::from_sql_and_values(
                            backend,
                            &sql,
                            ::std::vec![
                                ::suprnova::sea_orm::Value::String(
                                    ::core::option::Option::Some(::std::boxed::Box::new(now)),
                                ),
                                ::suprnova::eloquent::model::json_value_to_sea_value(
                                    &<Self as ::suprnova::eloquent::Model>::primary_key_value_json(&self),
                                ),
                            ],
                        ),
                    )
                    .await
                    .map_err(|e| ::suprnova::FrameworkError::database(e.to_string()))?;
                    ::core::result::Result::Ok(())
                }

                /// Restore from soft delete: `UPDATE table SET
                /// deleted_at = NULL WHERE pk = ?`. Takes `self` for
                /// signature parity with the other lifecycle methods.
                pub async fn restore(self) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                    let table = <Self as ::suprnova::eloquent::EloquentModel>::TABLE;
                    let pk_name = <Self as ::suprnova::eloquent::Model>::primary_key_name();
                    let sql = ::std::format!(
                        "UPDATE {table} SET {} = NULL WHERE {pk_name} = ?",
                        #soft_delete_col,
                    );
                    let db = ::suprnova::DB::connection()?;
                    let backend = <_ as ::suprnova::ConnectionTrait>::get_database_backend(db.inner());
                    <_ as ::suprnova::ConnectionTrait>::execute(
                        db.inner(),
                        ::suprnova::sea_orm::Statement::from_sql_and_values(
                            backend,
                            &sql,
                            ::std::vec![
                                ::suprnova::eloquent::model::json_value_to_sea_value(
                                    &<Self as ::suprnova::eloquent::Model>::primary_key_value_json(&self),
                                ),
                            ],
                        ),
                    )
                    .await
                    .map_err(|e| ::suprnova::FrameworkError::database(e.to_string()))?;
                    ::core::result::Result::Ok(())
                }

                /// Cheap accessor: `deleted_at IS NOT NULL` at row
                /// materialisation time. Does not touch the database.
                pub fn trashed(&self) -> bool {
                    self.#soft_delete_col_ident.is_some()
                }

                /// View including soft-deleted rows. Builds an
                /// unscoped Builder directly so we don't have to
                /// undo the scope `query()` would have applied.
                pub fn with_trashed() -> ::suprnova::Builder<Self> {
                    ::suprnova::Builder::<Self>::new()
                        .without_global_scope("soft_deletes")
                }

                /// View showing only soft-deleted rows.
                pub fn only_trashed() -> ::suprnova::Builder<Self> {
                    ::suprnova::Builder::<Self>::new()
                        .without_global_scope("soft_deletes")
                        .filter_not_null(#soft_delete_col)
                }

                /// Hard-delete this row, bypassing the soft-delete
                /// override. Fully qualifies to `Model::delete(self)`
                /// (which performs the actual `DELETE FROM ...`).
                pub async fn force_delete(self) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                    <Self as ::suprnova::eloquent::Model>::delete(self).await
                }

                /// Look up a row by primary key, honouring the
                /// soft-delete scope. Inherent override of the trait
                /// default — non-soft-delete models still use the
                /// SeaORM `find_by_id` path. Bypass the scope via
                /// `Self::with_trashed().filter(Self::primary_key_name(), id).first()`.
                pub async fn find(
                    id: #key_type,
                ) -> ::core::result::Result<::core::option::Option<Self>, ::suprnova::FrameworkError> {
                    <Self as ::suprnova::eloquent::Model>::query()
                        .filter(<Self as ::suprnova::eloquent::Model>::primary_key_name(), id)
                        .first()
                        .await
                }

                /// Look up a row by primary key, honouring the
                /// soft-delete scope. Returns
                /// `FrameworkError::ModelNotFound` (HTTP 404) when no
                /// row matches (or when the row is trashed).
                pub async fn find_or_fail(
                    id: #key_type,
                ) -> ::core::result::Result<Self, ::suprnova::FrameworkError>
                where
                    #key_type: ::std::fmt::Debug + ::core::marker::Copy,
                {
                    match <Self>::find(id).await? {
                        ::core::option::Option::Some(m) => ::core::result::Result::Ok(m),
                        ::core::option::Option::None => ::core::result::Result::Err(
                            ::suprnova::FrameworkError::not_found(::std::format!(
                                "{} with {} = {:?} not found",
                                ::core::any::type_name::<Self>(),
                                <Self as ::suprnova::eloquent::Model>::primary_key_name(),
                                id,
                            )),
                        ),
                    }
                }

                /// Fetch every alive row (skips trashed). Inherent
                /// override of the trait default.
                pub async fn all() -> ::core::result::Result<::std::vec::Vec<Self>, ::suprnova::FrameworkError> {
                    <Self as ::suprnova::eloquent::Model>::query().get().await
                }

                /// Fetch every alive row whose PK is in `ids` (skips
                /// trashed). Result order is the database's natural
                /// order — does not preserve `ids` order. Use
                /// `Self::with_trashed().where_in(pk, ids).get()` to
                /// include trashed rows.
                pub async fn find_many<I>(
                    ids: I,
                ) -> ::core::result::Result<::std::vec::Vec<Self>, ::suprnova::FrameworkError>
                where
                    I: ::core::iter::IntoIterator<Item = #key_type> + ::core::marker::Send,
                {
                    <Self as ::suprnova::eloquent::Model>::query()
                        .where_in(<Self as ::suprnova::eloquent::Model>::primary_key_name(), ids)
                        .get()
                        .await
                }
            }
        }
    } else {
        quote! {}
    };

    Ok(quote! {
        impl ::suprnova::eloquent::EloquentModel for #struct_ident {
            type Entity = #module_name::Entity;
            type Column = #module_name::Column;
            // Literal table string captured at parse time — see T3
            // for why this is the literal rather than a SeaORM call.
            const TABLE: &'static str = #table;
        }

        // Bridge the user struct <-> SeaORM Model row. The inner
        // module's `Model` is what SeaORM returns from queries; the
        // user struct is what their code names. Cast fields route
        // through `Cast::from_storage` / `Cast::to_storage` so the
        // user's Runtime type matches even when the inner Model
        // stores a different Storage type (e.g. INTEGER for bool).
        impl ::core::convert::From<#module_name::Model> for #struct_ident {
            fn from(row: #module_name::Model) -> Self {
                Self { #( #from_storage_arms ),* }
            }
        }

        impl ::core::convert::From<#struct_ident> for #module_name::Model {
            fn from(s: #struct_ident) -> Self {
                Self { #( #to_storage_arms ),* }
            }
        }

        // `Default` lets `ReplicateExt::replicate_with` build a fresh
        // shell when clearing fields. Field types must individually
        // impl `Default`; we don't try to be clever here.
        impl ::core::default::Default for #struct_ident {
            fn default() -> Self {
                Self { #( #field_idents: ::core::default::Default::default(), )* }
            }
        }

        #[::suprnova::__async_trait::async_trait]
        impl ::suprnova::eloquent::Model for #struct_ident {
            fn primary_key_name() -> &'static str { #pk_name }

            #fillable_impl

            #query_override

            fn primary_key_value(
                &self,
            ) -> <<Self::Entity as ::suprnova::EntityTrait>::PrimaryKey as ::suprnova::PrimaryKeyTrait>::ValueType {
                self.#pk_ident.clone().into()
            }

            fn primary_key_value_json(&self) -> ::suprnova::serde_json::Value {
                ::suprnova::serde_json::to_value(&self.#pk_ident)
                    .unwrap_or(::suprnova::serde_json::Value::Null)
            }

            fn reset_primary_key(&mut self) {
                self.#pk_ident = ::core::default::Default::default();
            }

            fn active_model_from_attrs(
                attrs: ::suprnova::eloquent::Attrs,
            ) -> ::core::result::Result<
                <Self::Entity as ::suprnova::EntityTrait>::ActiveModel,
                ::suprnova::FrameworkError,
            > {
                let mut am = <<Self::Entity as ::suprnova::EntityTrait>::ActiveModel as ::core::default::Default>::default();
                <Self as ::suprnova::eloquent::Model>::apply_attrs_to_active_model(&mut am, attrs)?;
                ::core::result::Result::Ok(am)
            }

            fn apply_attrs_to_active_model(
                am: &mut <Self::Entity as ::suprnova::EntityTrait>::ActiveModel,
                attrs: ::suprnova::eloquent::Attrs,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                for (name, val) in attrs.iter() {
                    match name {
                        #(#apply_arms,)*
                        other => {
                            return ::core::result::Result::Err(
                                ::suprnova::FrameworkError::validation(
                                    other,
                                    ::std::format!(
                                        "unknown column `{other}` on {}",
                                        ::core::stringify!(#struct_ident),
                                    ),
                                ),
                            );
                        }
                    }
                }
                #timestamp_inject_apply
                ::core::result::Result::Ok(())
            }

            fn into_active_model_for_update(
                self,
            ) -> ::core::result::Result<
                <Self::Entity as ::suprnova::EntityTrait>::ActiveModel,
                ::suprnova::FrameworkError,
            > {
                let mut am = <<Self::Entity as ::suprnova::EntityTrait>::ActiveModel
                    as ::core::default::Default>::default();
                #( #active_model_for_update_arms )*
                #timestamp_inject_save
                ::core::result::Result::Ok(am)
            }
        }

        #touchable_impl
        #touches_marker
        #soft_deletes_impl

        impl ::suprnova::eloquent::ReplicateExt for #struct_ident {
            fn replicate_with(&self, except: ::std::vec::Vec<::std::string::String>) -> Self {
                Self { #( #replicate_arms, )* }
            }
        }

        #[::suprnova::__async_trait::async_trait]
        impl ::suprnova::eloquent::FirstOrCreate for #struct_ident {
            fn from_attrs_unsaved(
                attrs: ::suprnova::eloquent::Attrs,
            ) -> ::core::result::Result<Self, ::suprnova::FrameworkError> {
                let mut s = <Self as ::core::default::Default>::default();
                for (name, v) in attrs.iter() {
                    match name {
                        #(#from_attrs_unsaved_arms,)*
                        _ => {}
                    }
                }
                ::core::result::Result::Ok(s)
            }
        }

        // T8 — serialization + fill surface. These methods are
        // inherent on the user's struct (NOT on the `Model` trait)
        // because they reference accessor / mutator names that vary
        // per model, and the trait signature is fixed.
        //
        // Two `impl #struct_ident { ... }` blocks coexist on the same
        // type as long as no method name collides; `to_json`,
        // `to_array`, `fill` don't clash with the static shortcuts
        // (`filter`, `where_in`, `count`, etc.) emitted below.
        impl #struct_ident {
            /// Serialize the model to a JSON object.
            ///
            /// - Fields listed in `#[model(hidden = [...])]` are
            ///   stripped from the output.
            /// - Methods listed in `#[model(appends = [...])]` are
            ///   called and their results inserted under their name.
            ///
            /// The base serialization uses the struct's `Serialize`
            /// impl, so casts have already converted Runtime → JSON
            /// shape by the time `to_json` reads them.
            pub fn to_json(&self) -> ::suprnova::serde_json::Value {
                let mut out = ::suprnova::serde_json::Map::new();
                let base = ::suprnova::serde_json::to_value(self)
                    .unwrap_or(::suprnova::serde_json::Value::Null);
                if let ::suprnova::serde_json::Value::Object(map) = base {
                    #filter_apply
                }
                #(#append_inserts)*
                ::suprnova::serde_json::Value::Object(out)
            }

            /// Alias for [`Self::to_json`] — matches Laravel's
            /// `$model->toArray()` naming.
            pub fn to_array(&self) -> ::suprnova::serde_json::Value {
                self.to_json()
            }

            /// Apply `attrs` to `self`, routing through any matching
            /// mutator (`set_<field>(value)`) before falling back to
            /// direct field assignment. Unknown columns are silently
            /// skipped to match Laravel's `$model->fill($attrs)`
            /// semantics.
            ///
            /// Honours mass-assignment: the `fillable` / `guarded`
            /// filter declared on `#[model]` runs first, dropping any
            /// columns the caller isn't allowed to set. To bypass the
            /// filter, wrap the call in
            /// [`::suprnova::eloquent::unguarded`] — the same
            /// task-local escape hatch `Model::create` / `update`
            /// honour.
            pub fn fill(
                &mut self,
                attrs: ::suprnova::eloquent::Attrs,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                let attrs = <Self as ::suprnova::eloquent::Model>::fillable_filter()
                    .apply(attrs);
                for (k, v) in attrs.iter() {
                    match k {
                        #(#fill_mutator_arms,)*
                        #(#fill_direct_arms,)*
                        _ => {} // unknown column — silently skip (Laravel parity)
                    }
                }
                ::core::result::Result::Ok(())
            }
        }

        // Static-style shortcuts on the user's struct. Each delegates to
        // `<Self as Model>::query()` so the dual-API surface stays in
        // one place. These are the Laravel-shape entry points that
        // make `User::filter("email", ...)` and `User::count()` read
        // the way users expect from PHP.
        impl #struct_ident {
            /// `SELECT COUNT(*) FROM table`.
            pub async fn count() -> ::core::result::Result<i64, ::suprnova::FrameworkError> {
                <Self as ::suprnova::eloquent::Model>::query().count().await
            }

            /// `SELECT COALESCE(SUM(col), 0) FROM table`.
            pub async fn sum<T>(
                col: impl ::suprnova::eloquent::builder::IntoColumn,
            ) -> ::core::result::Result<T, ::suprnova::FrameworkError>
            where
                T: ::suprnova::sea_orm::TryGetable + ::core::default::Default,
            {
                <Self as ::suprnova::eloquent::Model>::query().sum(col).await
            }

            /// `SELECT COALESCE(AVG(col), 0) FROM table`.
            pub async fn avg<T>(
                col: impl ::suprnova::eloquent::builder::IntoColumn,
            ) -> ::core::result::Result<T, ::suprnova::FrameworkError>
            where
                T: ::suprnova::sea_orm::TryGetable + ::core::default::Default,
            {
                <Self as ::suprnova::eloquent::Model>::query().avg(col).await
            }

            /// `SELECT MIN(col) FROM table`.
            pub async fn min<T>(
                col: impl ::suprnova::eloquent::builder::IntoColumn,
            ) -> ::core::result::Result<::core::option::Option<T>, ::suprnova::FrameworkError>
            where
                T: ::suprnova::sea_orm::TryGetable,
            {
                <Self as ::suprnova::eloquent::Model>::query().min(col).await
            }

            /// `SELECT MAX(col) FROM table`.
            pub async fn max<T>(
                col: impl ::suprnova::eloquent::builder::IntoColumn,
            ) -> ::core::result::Result<::core::option::Option<T>, ::suprnova::FrameworkError>
            where
                T: ::suprnova::sea_orm::TryGetable,
            {
                <Self as ::suprnova::eloquent::Model>::query().max(col).await
            }

            /// `SELECT col FROM table` — returns one column from every row.
            pub async fn pluck<T>(
                col: impl ::suprnova::eloquent::builder::IntoColumn,
            ) -> ::core::result::Result<::std::vec::Vec<T>, ::suprnova::FrameworkError>
            where
                T: ::suprnova::sea_orm::TryGetable,
            {
                <Self as ::suprnova::eloquent::Model>::query().pluck(col).await
            }

            /// `SELECT key_col, val_col FROM table` — returns a `HashMap`
            /// keyed by `key_col`, valued by `val_col`.
            pub async fn pluck_keyed<K, V>(
                key_col: impl ::suprnova::eloquent::builder::IntoColumn,
                val_col: impl ::suprnova::eloquent::builder::IntoColumn,
            ) -> ::core::result::Result<
                ::std::collections::HashMap<K, V>,
                ::suprnova::FrameworkError,
            >
            where
                K: ::suprnova::sea_orm::TryGetable + ::core::cmp::Eq + ::std::hash::Hash,
                V: ::suprnova::sea_orm::TryGetable,
            {
                <Self as ::suprnova::eloquent::Model>::query()
                    .pluck_keyed(key_col, val_col)
                    .await
            }

            /// Static-style `filter` — opens a `Builder<Self>` with
            /// one equality WHERE clause already attached.
            #[doc(alias = "db_where")]
            pub fn filter(
                col: impl ::suprnova::eloquent::builder::IntoColumn,
                val: impl ::suprnova::eloquent::builder::IntoVal,
            ) -> ::suprnova::Builder<Self> {
                <Self as ::suprnova::eloquent::Model>::query().filter(col, val)
            }

            /// Laravel-shape alias for [`Self::filter`].
            #[doc(alias = "filter")]
            pub fn db_where(
                col: impl ::suprnova::eloquent::builder::IntoColumn,
                val: impl ::suprnova::eloquent::builder::IntoVal,
            ) -> ::suprnova::Builder<Self> {
                <Self as ::suprnova::eloquent::Model>::query().db_where(col, val)
            }

            /// Static-style `where_in`.
            pub fn where_in<V, I>(
                col: impl ::suprnova::eloquent::builder::IntoColumn,
                vals: I,
            ) -> ::suprnova::Builder<Self>
            where
                I: ::core::iter::IntoIterator<Item = V>,
                V: ::suprnova::eloquent::builder::IntoVal,
            {
                <Self as ::suprnova::eloquent::Model>::query().where_in(col, vals)
            }

            /// Static-style `where_like`.
            pub fn where_like(
                col: impl ::suprnova::eloquent::builder::IntoColumn,
                pattern: impl ::core::convert::Into<::std::string::String>,
            ) -> ::suprnova::Builder<Self> {
                <Self as ::suprnova::eloquent::Model>::query().where_like(col, pattern)
            }

            /// Static-style `latest` — `ORDER BY created_at DESC`. Models
            /// without a `created_at` column will fail at the SQL layer;
            /// timestamp surface lands in T9.
            pub fn latest() -> ::suprnova::Builder<Self> {
                <Self as ::suprnova::eloquent::Model>::query().order_by_desc("created_at")
            }

            /// Static-style `oldest` — `ORDER BY created_at ASC`.
            pub fn oldest() -> ::suprnova::Builder<Self> {
                <Self as ::suprnova::eloquent::Model>::query().order_by_asc("created_at")
            }
        }
    })
}
