//! Phase 10C T2c — parse `observers = [...]` attribute on `#[model]`
//! and emit the per-model `Self::observe()` runtime registration shim.
//!
//! Two emissions:
//!
//! - [`emit_observers_attestation`] — compile-time validation that each
//!   listed observer type resolves. Actual listener registration happens
//!   via the `#[observer(M)]` inventory pathway (T2b); this attribute is
//!   compile-check + documentation only.
//!
//! - [`emit_per_model_observe_shim`] — runtime registration path that
//!   complements the inventory pathway. Each call to
//!   `User::observe(MyObserver)` registers all 16 listener adapters at
//!   call time. Mirrors Laravel's `User::observe(MyObserver::class)`
//!   manual entrypoint.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Expr, Ident, punctuated::Punctuated, token::Comma};

use super::parse::to_snake;

/// Parsed `observers = [Type1, Type2, ...]` attribute body.
///
/// Each expression is normally a path to a struct type, but the parser
/// accepts any `syn::Expr` so qualified paths like
/// `crate::observers::AuditObserver` round-trip cleanly. The compile-time
/// validation simply references each expression through
/// `::std::any::type_name::<T>`; rustc validates the path resolves to a
/// real type during type-check.
pub type ObserversAttr = Punctuated<Expr, Comma>;

/// Emit a `const _: fn() = || { ... };` validation block that references
/// each listed observer type. Catches typos at the model declaration
/// site without touching the runtime install path.
pub fn emit_observers_attestation(attr: &ObserversAttr) -> TokenStream {
    if attr.is_empty() {
        return TokenStream::new();
    }
    let idents = attr.iter();
    quote! {
        const _: fn() = || {
            #(let _ = ::std::any::type_name::<#idents>;)*
        };
    }
}

/// Lifecycle methods that return `Result<(), FrameworkError>` and
/// register through `EventFacade::listen`. Mirrors T2b's
/// `NON_CANCELLABLE_METHODS` so the per-model shim emits the same
/// adapter shape at runtime.
const NON_CANCELLABLE_METHODS: &[(&str, &str)] = &[
    ("retrieving", "Retrieving"),
    ("retrieved", "Retrieved"),
    ("created", "Created"),
    ("updated", "Updated"),
    ("saved", "Saved"),
    ("deleted", "Deleted"),
    ("trashed", "Trashed"),
    ("restored", "Restored"),
    ("replicating", "Replicating"),
    ("force_deleting", "ForceDeleting"),
    ("force_deleted", "ForceDeleted"),
];

/// Lifecycle methods that return `EventResult` and register through
/// `listen_cancellable`. Mirrors T2b's `CANCELLABLE_METHODS`.
const CANCELLABLE_METHODS: &[(&str, &str)] = &[
    ("saving", "Saving"),
    ("creating", "Creating"),
    ("updating", "Updating"),
    ("deleting", "Deleting"),
    ("restoring", "Restoring"),
];

/// Emit the per-model `pub async fn observe<O>(observer: O)` shim.
///
/// Registers all 16 listener adapters at call time. Each adapter is a
/// generic struct that stores a clone of the observer and delegates
/// into the trait method. Unlike T2b's inventory pathway, the shim
/// emits the full set of 16 listeners — the trait defaults make
/// non-overridden methods cheap no-ops, and there's no parse-time impl
/// block to walk for "which methods did the user override?".
///
/// Idempotency is the caller's concern. Calling `User::observe(MyObs)`
/// twice registers twice — matches Laravel's manual semantics. Tests
/// that exercise this shim with the inventory pathway also active
/// should be aware that the inventory observer fires in addition to
/// the manually-installed one.
pub fn emit_per_model_observe_shim(struct_ident: &Ident) -> TokenStream {
    let module_name: Ident = quote::format_ident!("{}", to_snake(&struct_ident.to_string()));

    let non_cancellable: Vec<TokenStream> = NON_CANCELLABLE_METHODS
        .iter()
        .map(|(method, event)| {
            let event_ident: Ident = syn::parse_str(event)
                .expect("event struct ident parses (validated against the closed set)");
            emit_observe_arm_non_cancellable(struct_ident, &module_name, method, &event_ident)
        })
        .collect();

    let cancellable: Vec<TokenStream> = CANCELLABLE_METHODS
        .iter()
        .map(|(method, event)| {
            let event_ident: Ident = syn::parse_str(event)
                .expect("event struct ident parses (validated against the closed set)");
            emit_observe_arm_cancellable(struct_ident, &module_name, method, &event_ident)
        })
        .collect();

    quote! {
        impl #struct_ident {
            /// Phase 10C T2c — manual observer registration (per-model
            /// shim, complements the `#[suprnova::observer(M)]` inventory
            /// pathway).
            ///
            /// Registers all 16 lifecycle listener adapters for
            /// `observer` against the global event dispatcher. Each
            /// listener clones the observer and delegates into the
            /// corresponding trait method.
            ///
            /// # Idempotency
            ///
            /// Each call registers a fresh set of adapters. Two calls
            /// to `Self::observe(MyObs)` register two sets — matches
            /// Laravel's `Model::observe(MyObs::class)` semantics. If
            /// the observer is also registered via
            /// `#[suprnova::observer]`, the inventory adapter fires in
            /// addition to the manually-installed ones.
            pub async fn observe<O>(observer: O)
            where
                O: ::suprnova::eloquent::observers::Observer<Self>
                    + ::core::clone::Clone
                    + 'static,
            {
                #(#non_cancellable)*
                #(#cancellable)*
            }
        }
    }
}

/// Emit one non-cancellable listener registration block for the
/// `observe()` shim. Mirrors T2b's `emit_non_cancellable_adapter`, but
/// stores the observer by value (not zero-sized) so each invocation of
/// the shim installs a fresh listener carrying its own observer clone.
fn emit_observe_arm_non_cancellable(
    struct_ident: &Ident,
    module_name: &Ident,
    method: &str,
    event_ident: &Ident,
) -> TokenStream {
    let adapter_struct = format_ident!("__ObserveAdapter_{}", method);
    let call_expr = emit_observe_call_non_cancellable(method);

    quote! {
        {
            #[allow(non_camel_case_types)]
            struct #adapter_struct<O>
            where
                O: ::suprnova::eloquent::observers::Observer<#struct_ident>
                    + ::core::clone::Clone
                    + 'static,
            {
                observer: O,
            }

            #[::suprnova::__async_trait::async_trait]
            impl<O> ::suprnova::events::Listener<#module_name::events::#event_ident>
                for #adapter_struct<O>
            where
                O: ::suprnova::eloquent::observers::Observer<#struct_ident>
                    + ::core::clone::Clone
                    + 'static,
            {
                async fn handle(
                    &self,
                    event: &#module_name::events::#event_ident,
                ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                    let obs = ::core::clone::Clone::clone(&self.observer);
                    #call_expr
                }
            }

            ::suprnova::events::EventFacade::listen::<
                #module_name::events::#event_ident,
                #adapter_struct<O>,
            >(
                ::std::sync::Arc::new(#adapter_struct {
                    observer: ::core::clone::Clone::clone(&observer),
                }),
            )
            .await;
        }
    }
}

/// Emit one cancellable listener registration block for the `observe()`
/// shim. Mirrors T2b's `emit_cancellable_adapter`, with the same
/// by-value observer storage as the non-cancellable arm.
fn emit_observe_arm_cancellable(
    struct_ident: &Ident,
    module_name: &Ident,
    method: &str,
    event_ident: &Ident,
) -> TokenStream {
    let adapter_struct = format_ident!("__ObserveCancellableAdapter_{}", method);
    let call_expr = emit_observe_call_cancellable(method);

    quote! {
        {
            #[allow(non_camel_case_types)]
            struct #adapter_struct<O>
            where
                O: ::suprnova::eloquent::observers::Observer<#struct_ident>
                    + ::core::clone::Clone
                    + 'static,
            {
                observer: O,
            }

            #[::suprnova::__async_trait::async_trait]
            impl<O> ::suprnova::eloquent::events::CancellableListener<
                #module_name::events::#event_ident,
            > for #adapter_struct<O>
            where
                O: ::suprnova::eloquent::observers::Observer<#struct_ident>
                    + ::core::clone::Clone
                    + 'static,
            {
                async fn handle(
                    &self,
                    event: &#module_name::events::#event_ident,
                ) -> ::suprnova::eloquent::events::EventResult {
                    let obs = ::core::clone::Clone::clone(&self.observer);
                    #call_expr
                }
            }

            ::suprnova::eloquent::events::listen_cancellable::<
                #module_name::events::#event_ident,
                #adapter_struct<O>,
            >(
                ::std::sync::Arc::new(#adapter_struct {
                    observer: ::core::clone::Clone::clone(&observer),
                }),
            )
            .await;
        }
    }
}

/// Emit the call expression for a non-cancellable adapter's `handle`
/// body. Matches T2b's per-method shape — same event field layout
/// emitted by `model/events.rs`.
fn emit_observe_call_non_cancellable(method: &str) -> TokenStream {
    match method {
        "retrieving" => quote! { obs.retrieving().await },
        "retrieved" => quote! { obs.retrieved(&event.model).await },
        "created" => quote! { obs.created(&event.model).await },
        "saved" => quote! { obs.saved(&event.model).await },
        "trashed" => quote! { obs.trashed(&event.model).await },
        "restored" => quote! { obs.restored(&event.model).await },
        "force_deleting" => quote! { obs.force_deleting(&event.model).await },
        "force_deleted" => quote! { obs.force_deleted(&event.model).await },
        "updated" => quote! { obs.updated(&event.previous, &event.current).await },
        "deleted" => quote! { obs.deleted(&event.model, event.is_force).await },
        "replicating" => quote! {
            let mut replica = event.replica.lock().await;
            obs.replicating(&event.source, &mut *replica).await
        },
        other => {
            // Unreachable on the closed 11-method set — `NON_CANCELLABLE_METHODS`
            // is the lone source. A compile_error! here means the
            // method table grew without an arm update.
            let msg =
                format!("internal error: unhandled non-cancellable observer method `{other}`",);
            quote! { compile_error!(#msg) }
        }
    }
}

/// Emit the call expression for a cancellable adapter's `handle` body.
/// Mirrors T2b's `emit_cancellable_adapter_call`.
fn emit_observe_call_cancellable(method: &str) -> TokenStream {
    match method {
        "creating" => quote! {
            let mut attrs = event.attrs.lock().await;
            obs.creating(&mut *attrs).await
        },
        "saving" => quote! {
            let mut attrs = event.attrs.lock().await;
            obs.saving(&mut *attrs, event.is_creating).await
        },
        "updating" => quote! {
            let mut attrs = event.attrs.lock().await;
            obs.updating(&event.previous, &mut *attrs).await
        },
        "deleting" => quote! { obs.deleting(&event.model, event.is_force).await },
        "restoring" => quote! { obs.restoring(&event.model).await },
        other => {
            let msg = format!("internal error: unhandled cancellable observer method `{other}`",);
            quote! { compile_error!(#msg) }
        }
    }
}
