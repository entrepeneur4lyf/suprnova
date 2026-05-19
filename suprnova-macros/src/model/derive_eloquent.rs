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

    let apply_arms = field_idents
        .iter()
        .zip(field_strs.iter())
        .map(|(ident, name)| {
            quote! {
                #name => {
                    am.#ident = ::suprnova::sea_orm::Set(
                        ::suprnova::serde_json::from_value(val.clone()).map_err(|e| {
                            ::suprnova::FrameworkError::validation(
                                #name,
                                ::std::format!("cannot decode JSON into column type: {e}"),
                            )
                        })?
                    );
                }
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

    let from_attrs_unsaved_arms = field_idents
        .iter()
        .zip(field_strs.iter())
        .map(|(ident, name)| {
            quote! {
                #name => {
                    s.#ident = ::suprnova::serde_json::from_value(v.clone()).unwrap_or_default();
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
    let active_model_for_update_arms = field_idents
        .iter()
        .zip(field_strs.iter())
        .map(|(ident, name)| {
            let is_pk = name == pk_name;
            if is_pk {
                quote! { am.#ident = ::suprnova::sea_orm::ActiveValue::Unchanged(self.#ident.clone()); }
            } else {
                quote! { am.#ident = ::suprnova::sea_orm::Set(self.#ident.clone()); }
            }
        });

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
        // user struct is what their code names. Same field set, so
        // field-by-field move both ways.
        impl ::core::convert::From<#module_name::Model> for #struct_ident {
            fn from(row: #module_name::Model) -> Self {
                Self { #( #field_idents: row.#field_idents, )* }
            }
        }

        impl ::core::convert::From<#struct_ident> for #module_name::Model {
            fn from(s: #struct_ident) -> Self {
                Self { #( #field_idents: s.#field_idents, )* }
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
                ::core::result::Result::Ok(am)
            }
        }

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
