//! Phase 10C T1 — emit per-model `events::` submodule + macro-driven
//! [`ModelEventHooks`] impl.
//!
//! Two emission helpers:
//!
//! - [`emit_events_module`] builds the `pub mod events { ... }` token
//!   stream that ships INSIDE the existing per-model inner module
//!   (`pub mod user { ... }`). Sixteen lifecycle event structs land
//!   inside, each `impl ::suprnova::Event`-ing itself with a
//!   `"<model_snake>::events::<Name>"` name so log lines and tests
//!   can disambiguate per-model events sharing a Laravel name.
//! - [`emit_event_dispatch_impl`] builds the `impl ::suprnova::ModelEventHooks
//!   for #StructIdent` block that bridges the user struct to the
//!   per-type event structs. Lives OUTSIDE the inner module so it
//!   sits at the same scope as the user struct itself.
//!
//! Cancellable events
//! (`Saving`/`Creating`/`Updating`/`Deleting`/`Restoring`) carry an
//! `Arc<tokio::sync::Mutex<Attrs>>` (or `Arc<Mutex<Self>>` for
//! `Replicating`) so listeners can mutate the in-flight payload
//! before persistence. Non-cancellable events carry the persisted
//! `Model` clone.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Ident;

use super::parse::to_snake;

/// Emit the per-model `pub mod events { ... }` token stream. Mounted
/// inside the inner `pub mod <model_snake>` block by
/// [`super::expand`].
pub fn emit_events_module(struct_ident: &Ident) -> TokenStream {
    let struct_name = struct_ident.to_string();
    let module_snake = to_snake(&struct_name);

    let qualify = |name: &str| format!("{module_snake}::events::{name}");

    let retrieving_name = qualify("Retrieving");
    let retrieved_name = qualify("Retrieved");
    let saving_name = qualify("Saving");
    let creating_name = qualify("Creating");
    let created_name = qualify("Created");
    let updating_name = qualify("Updating");
    let updated_name = qualify("Updated");
    let saved_name = qualify("Saved");
    let deleting_name = qualify("Deleting");
    let deleted_name = qualify("Deleted");
    let trashed_name = qualify("Trashed");
    let restoring_name = qualify("Restoring");
    let restored_name = qualify("Restored");
    let replicating_name = qualify("Replicating");
    let force_deleting_name = qualify("ForceDeleting");
    let force_deleted_name = qualify("ForceDeleted");

    quote! {
        /// Phase 10C T1 — lifecycle event structs. Listeners attach
        /// via [`suprnova::EventFacade::listen`] (non-cancellable) or
        /// [`suprnova::listen_cancellable`] (cancellable). See
        /// `docs/core/eloquent.md` "Lifecycle events" for the matrix.
        pub mod events {
            use super::#struct_ident;
            use ::suprnova::eloquent::attrs::Attrs;
            use ::std::sync::Arc;

            /// Fired once at the start of each `Builder::get` /
            /// `first` / `first_or_fail` call. Use for cache warming
            /// or query-time instrumentation.
            #[derive(Debug, Clone)]
            pub struct Retrieving;
            impl ::suprnova::Event for Retrieving {
                fn event_name() -> &'static str { #retrieving_name }
            }

            /// Fired once per row hydrated from the database. Matches
            /// Laravel's `retrieved` lifecycle hook.
            #[derive(Debug, Clone)]
            pub struct Retrieved { pub model: #struct_ident }
            impl ::suprnova::Event for Retrieved {
                fn event_name() -> &'static str { #retrieved_name }
            }

            /// Fired before both `create` and `save`. Cancellable.
            /// `is_creating` disambiguates the two paths so a single
            /// listener can branch on insert vs update.
            #[derive(Debug, Clone)]
            pub struct Saving {
                pub attrs: Arc<::tokio::sync::Mutex<Attrs>>,
                pub is_creating: bool,
            }
            impl ::suprnova::Event for Saving {
                fn event_name() -> &'static str { #saving_name }
            }

            /// Fired before `create`. Cancellable. Carries the
            /// in-flight `Attrs` map; listeners may mutate before the
            /// INSERT lands.
            #[derive(Debug, Clone)]
            pub struct Creating {
                pub attrs: Arc<::tokio::sync::Mutex<Attrs>>,
            }
            impl ::suprnova::Event for Creating {
                fn event_name() -> &'static str { #creating_name }
            }

            /// Fired after a successful `create`.
            #[derive(Debug, Clone)]
            pub struct Created { pub model: #struct_ident }
            impl ::suprnova::Event for Created {
                fn event_name() -> &'static str { #created_name }
            }

            /// Fired before `save` / `update` on an existing row.
            /// Cancellable. Carries both the pre-update model snapshot
            /// and the in-flight `Attrs` map (mutable through the
            /// `Arc<Mutex<_>>`).
            #[derive(Debug, Clone)]
            pub struct Updating {
                pub previous: #struct_ident,
                pub attrs: Arc<::tokio::sync::Mutex<Attrs>>,
            }
            impl ::suprnova::Event for Updating {
                fn event_name() -> &'static str { #updating_name }
            }

            /// Fired after a successful `save` / `update`. `previous`
            /// is the pre-update snapshot, `current` is the row as
            /// the database has it post-update.
            #[derive(Debug, Clone)]
            pub struct Updated {
                pub previous: #struct_ident,
                pub current: #struct_ident,
            }
            impl ::suprnova::Event for Updated {
                fn event_name() -> &'static str { #updated_name }
            }

            /// Fired after both `create` and `save`.
            #[derive(Debug, Clone)]
            pub struct Saved { pub model: #struct_ident }
            impl ::suprnova::Event for Saved {
                fn event_name() -> &'static str { #saved_name }
            }

            /// Fired before `delete` (soft or hard). Cancellable.
            /// `is_force` is `true` when invoked via `force_delete`
            /// on a soft-delete model — listeners that care about
            /// soft-delete-only behaviour branch on this flag.
            #[derive(Debug, Clone)]
            pub struct Deleting {
                pub model: #struct_ident,
                pub is_force: bool,
            }
            impl ::suprnova::Event for Deleting {
                fn event_name() -> &'static str { #deleting_name }
            }

            /// Fired after a successful `delete`. `is_force` matches
            /// the `Deleting` event's flag.
            #[derive(Debug, Clone)]
            pub struct Deleted {
                pub model: #struct_ident,
                pub is_force: bool,
            }
            impl ::suprnova::Event for Deleted {
                fn event_name() -> &'static str { #deleted_name }
            }

            /// Fired after a soft-delete on a model with
            /// `#[model(soft_deletes)]`. NOT fired by `force_delete`
            /// (which removes the row outright).
            #[derive(Debug, Clone)]
            pub struct Trashed { pub model: #struct_ident }
            impl ::suprnova::Event for Trashed {
                fn event_name() -> &'static str { #trashed_name }
            }

            /// Fired before `restore` on a soft-delete model.
            /// Cancellable — a listener can refuse the un-tombstone
            /// operation.
            #[derive(Debug, Clone)]
            pub struct Restoring { pub model: #struct_ident }
            impl ::suprnova::Event for Restoring {
                fn event_name() -> &'static str { #restoring_name }
            }

            /// Fired after a successful `restore`.
            #[derive(Debug, Clone)]
            pub struct Restored { pub model: #struct_ident }
            impl ::suprnova::Event for Restored {
                fn event_name() -> &'static str { #restored_name }
            }

            /// Fired during `replicate` / `replicate_except` /
            /// `replicate_into`, BEFORE the replica is returned.
            /// `source` is the original; `replica` is the freshly
            /// built clone wrapped in `Arc<Mutex<_>>` so listeners
            /// can clear timestamps, reset flags, etc.
            #[derive(Debug, Clone)]
            pub struct Replicating {
                pub source: #struct_ident,
                pub replica: Arc<::tokio::sync::Mutex<#struct_ident>>,
            }
            impl ::suprnova::Event for Replicating {
                fn event_name() -> &'static str { #replicating_name }
            }

            /// Fired before `force_delete` on a soft-delete model.
            #[derive(Debug, Clone)]
            pub struct ForceDeleting { pub model: #struct_ident }
            impl ::suprnova::Event for ForceDeleting {
                fn event_name() -> &'static str { #force_deleting_name }
            }

            /// Fired after a successful `force_delete` on a
            /// soft-delete model.
            #[derive(Debug, Clone)]
            pub struct ForceDeleted { pub model: #struct_ident }
            impl ::suprnova::Event for ForceDeleted {
                fn event_name() -> &'static str { #force_deleted_name }
            }
        }
    }
}

/// Emit the `impl ::suprnova::ModelEventHooks for #struct` block.
/// Sits OUTSIDE the per-model inner module so it's visible at the
/// same scope as the user struct.
pub fn emit_event_dispatch_impl(struct_ident: &Ident) -> TokenStream {
    let module_name: Ident = syn::parse_str(&to_snake(&struct_ident.to_string()))
        .expect("snake-case struct name parses as ident");

    quote! {
        #[::suprnova::__async_trait::async_trait]
        impl ::suprnova::eloquent::events::ModelEventHooks for #struct_ident {
            async fn __dispatch_creating(
                attrs: ::std::sync::Arc<::tokio::sync::Mutex<::suprnova::eloquent::Attrs>>,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_cancellable(
                    #module_name::events::Creating { attrs },
                ).await
            }

            async fn __dispatch_saving(
                attrs: ::std::sync::Arc<::tokio::sync::Mutex<::suprnova::eloquent::Attrs>>,
                is_creating: bool,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_cancellable(
                    #module_name::events::Saving { attrs, is_creating },
                ).await
            }

            async fn __dispatch_created(
                model: &Self,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_after(
                    #module_name::events::Created { model: ::core::clone::Clone::clone(model) },
                ).await
            }

            async fn __dispatch_saved(
                model: &Self,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_after(
                    #module_name::events::Saved { model: ::core::clone::Clone::clone(model) },
                ).await
            }

            async fn __dispatch_updating(
                previous: &Self,
                attrs: ::std::sync::Arc<::tokio::sync::Mutex<::suprnova::eloquent::Attrs>>,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_cancellable(
                    #module_name::events::Updating {
                        previous: ::core::clone::Clone::clone(previous),
                        attrs,
                    },
                ).await
            }

            async fn __dispatch_updated(
                previous: &Self,
                current: &Self,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_after(
                    #module_name::events::Updated {
                        previous: ::core::clone::Clone::clone(previous),
                        current: ::core::clone::Clone::clone(current),
                    },
                ).await
            }

            async fn __dispatch_deleting(
                model: &Self,
                is_force: bool,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_cancellable(
                    #module_name::events::Deleting {
                        model: ::core::clone::Clone::clone(model),
                        is_force,
                    },
                ).await
            }

            async fn __dispatch_deleted(
                model: &Self,
                is_force: bool,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_after(
                    #module_name::events::Deleted {
                        model: ::core::clone::Clone::clone(model),
                        is_force,
                    },
                ).await
            }

            async fn __dispatch_trashed(
                model: &Self,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_after(
                    #module_name::events::Trashed { model: ::core::clone::Clone::clone(model) },
                ).await
            }

            async fn __dispatch_restoring(
                model: &Self,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_cancellable(
                    #module_name::events::Restoring { model: ::core::clone::Clone::clone(model) },
                ).await
            }

            async fn __dispatch_restored(
                model: &Self,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_after(
                    #module_name::events::Restored { model: ::core::clone::Clone::clone(model) },
                ).await
            }

            async fn __dispatch_force_deleting(
                model: &Self,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_after(
                    #module_name::events::ForceDeleting {
                        model: ::core::clone::Clone::clone(model),
                    },
                ).await
            }

            async fn __dispatch_force_deleted(
                model: &Self,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_after(
                    #module_name::events::ForceDeleted {
                        model: ::core::clone::Clone::clone(model),
                    },
                ).await
            }

            async fn __dispatch_replicating(
                source: &Self,
                replica: ::std::sync::Arc<::tokio::sync::Mutex<Self>>,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_after(
                    #module_name::events::Replicating {
                        source: ::core::clone::Clone::clone(source),
                        replica,
                    },
                ).await
            }

            async fn __dispatch_retrieving(
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_after(
                    #module_name::events::Retrieving,
                ).await
            }

            async fn __dispatch_retrieved(
                model: &Self,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                ::suprnova::eloquent::events::dispatch_after(
                    #module_name::events::Retrieved {
                        model: ::core::clone::Clone::clone(model),
                    },
                ).await
            }
        }
    }
}
