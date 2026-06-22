//! Phase 10C T2b — `#[suprnova::observer(M)]` attribute macro.
//!
//! Walks an `impl Observer<M> for SomeObserver { ... }` block at parse
//! time, identifies which trait methods the user overrode (by name
//! match against the 16-method closed set the trait ships), and emits
//! one adapter listener per overridden method. The adapters are
//! zero-sized structs that delegate into the user's trait impl;
//! registration goes through
//! [`EventFacade::listen`](::suprnova::EventFacade::listen) for the 11
//! non-cancellable methods and
//! [`listen_cancellable`](::suprnova::eloquent::events::listen_cancellable)
//! for the 5 cancellable ones.
//!
//! All registration is wrapped in a generated `install` fn whose
//! pointer is submitted to the `ObserverEntry` inventory T2a ships,
//! and [`bootstrap_observers`](::suprnova::bootstrap_observers) drains
//! that inventory once at boot.
//!
//! # Macro contract
//!
//! - **`#[suprnova::observer(M)]` MUST precede `#[async_trait]`.**
//!   Attribute macros expand outside-in. `async_trait` rewrites every
//!   `async fn` in the impl block into the desugared `Pin<Box<dyn
//!   Future>>` poll-fn shape, and this macro's name-match against the
//!   16 trait method names would fail to find any of them. Always
//!   apply this attribute first.
//!
//! - **The observer struct must be a unit struct in v1.** The macro
//!   constructs the observer via `let obs = #observer_ident;`, which
//!   only works for zero-sized types. Stateful observers (e.g.
//!   `Arc<Inner>::clone`) are a v2 concern; the current target is
//!   Laravel's `Observable::observe(SomeObserver::class)` shape, which
//!   is identical (the observer is a registered type, not an
//!   instance).
//!
//! - **Idempotency is enforced inside the install closure.** The
//!   generated `__install_<observer>_observer` fn is gated by a
//!   per-observer `AtomicBool` so calling
//!   [`bootstrap_observers`](::suprnova::bootstrap_observers) twice
//!   does not register the listeners twice. T2a's
//!   [`observers`](::suprnova::eloquent::observers) docs delegate this
//!   contract to T2b explicitly.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{ImplItem, ItemImpl, Path, Type, parse_macro_input, parse_str};

/// Lifecycle methods that return `Result<(), FrameworkError>` and
/// register through `EventFacade::listen`. Tuple is `(method_name,
/// event_struct_ident)`.
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
/// `listen_cancellable`. Tuple is `(method_name, event_struct_ident)`.
const CANCELLABLE_METHODS: &[(&str, &str)] = &[
    ("saving", "Saving"),
    ("creating", "Creating"),
    ("updating", "Updating"),
    ("deleting", "Deleting"),
    ("restoring", "Restoring"),
];

/// Implementation of `#[suprnova::observer(M)]`.
///
/// Parses the impl block, generates one adapter listener per
/// overridden trait method, and emits an `ObserverEntry` inventory
/// submission whose `install` closure registers every adapter.
pub fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Domain 5 audit M-D5-7 — parse the model arg as `syn::Path`
    // (not `syn::Expr`). The downstream module-path emission requires
    // a sequence of named segments (`crate::models::User` → walk to
    // `user`); `Expr` accepted unparseable shapes like `123` or
    // `2 + 2`, which then panicked inside `Ident::new` at
    // `expr_to_snake_module_path` instead of producing a clean
    // compile error. Path's segment-typed AST guarantees each segment
    // is already a valid `syn::Ident`, so the panic class is gone.
    let attr_ts2 = proc_macro2::TokenStream::from(attr);
    let model_path: Path = match syn::parse2(attr_ts2.clone()) {
        Ok(p) => p,
        Err(e) => {
            return syn::Error::new_spanned(
                attr_ts2,
                format!(
                    "#[suprnova::observer(Model)] takes one model type argument as a \
                     qualified path (e.g. `User`, `crate::models::User`): {e}"
                ),
            )
            .to_compile_error()
            .into();
        }
    };
    let input = parse_macro_input!(item as ItemImpl);

    let observer_ident = match &*input.self_ty {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.clone()),
        _ => None,
    };
    let observer_ident = match observer_ident {
        Some(id) => id,
        None => {
            return syn::Error::new_spanned(
                &input.self_ty,
                "#[suprnova::observer(M)] requires the observer type to be a named struct",
            )
            .to_compile_error()
            .into();
        }
    };

    let module_path = path_to_snake_module_path(&model_path);

    // Identify which trait methods the user actually wrote by name
    // match. The trait defaults everything to no-ops, so any method
    // absent from the impl block is intentionally inheriting the
    // default and must NOT get a listener registered for it.
    let written_method_names: Vec<String> = input
        .items
        .iter()
        .filter_map(|i| match i {
            ImplItem::Fn(f) => Some(f.sig.ident.to_string()),
            _ => None,
        })
        .collect();

    let mut listener_emissions: Vec<TokenStream2> = Vec::new();

    for (method, event_struct) in NON_CANCELLABLE_METHODS {
        if !written_method_names.iter().any(|n| n == method) {
            continue;
        }
        let event_ident: syn::Ident = match parse_str(event_struct) {
            Ok(i) => i,
            Err(e) => {
                return syn::Error::new_spanned(
                    &input.self_ty,
                    format!("internal: event struct ident `{event_struct}` did not parse: {e}"),
                )
                .to_compile_error()
                .into();
            }
        };
        let adapter =
            emit_non_cancellable_adapter(&observer_ident, &module_path, method, &event_ident);
        listener_emissions.push(adapter);
    }

    for (method, event_struct) in CANCELLABLE_METHODS {
        if !written_method_names.iter().any(|n| n == method) {
            continue;
        }
        let event_ident: syn::Ident = match parse_str(event_struct) {
            Ok(i) => i,
            Err(e) => {
                return syn::Error::new_spanned(
                    &input.self_ty,
                    format!("internal: event struct ident `{event_struct}` did not parse: {e}"),
                )
                .to_compile_error()
                .into();
            }
        };
        let adapter = emit_cancellable_adapter(&observer_ident, &module_path, method, &event_ident);
        listener_emissions.push(adapter);
    }

    let installer_name = format_ident!(
        "__install_{}_observer",
        to_snake(&observer_ident.to_string())
    );
    let observer_str = observer_ident.to_string();

    // The `AtomicBool` gate inside the async block enforces single-
    // install idempotency: every observer's listeners install exactly
    // once across the process lifetime, even if `bootstrap_observers`
    // is called twice (which the T2a test does). Per-observer state is
    // declared as a `static` inside the async block so it stays local
    // to the generated fn and cannot collide with other observers'
    // gates.
    let output = quote! {
        #input

        fn #installer_name() -> ::suprnova::eloquent::observers::ObserverInstallFuture {
            ::std::boxed::Box::pin(async {
                static __INSTALLED: ::std::sync::atomic::AtomicBool =
                    ::std::sync::atomic::AtomicBool::new(false);
                if __INSTALLED.swap(true, ::std::sync::atomic::Ordering::SeqCst) {
                    return ::core::result::Result::Ok(());
                }
                #(#listener_emissions)*
                ::core::result::Result::Ok(())
            })
        }

        ::suprnova::inventory::submit! {
            ::suprnova::eloquent::observers::ObserverEntry {
                name: #observer_str,
                install: #installer_name,
            }
        }
    };

    output.into()
}

/// Convert `CamelCase` → `snake_case`. Matches `model::parse::to_snake`
/// behaviour so emitted event-module paths line up with the model
/// macro's emission. Inlined here so the observer macro doesn't have
/// to cross-depend on the model macro internals.
fn to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.char_indices() {
        if ch.is_uppercase() && i != 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// Convert a model `Path` into the module-path prefix the
/// `#[suprnova::model]` macro uses for the per-model `events::`
/// submodule.
///
/// - `User` → `user`
/// - `crate::models::User` → `crate::models::user`
/// - `super::User` → `super::user`
///
/// Only the LAST segment is snake-cased; earlier segments are passed
/// through verbatim because they're already-valid module identifiers
/// authored by the user.
///
/// Domain 5 audit M-D5-7 — takes `&syn::Path` instead of `&syn::Expr`
/// so the segment-walk operates on typed `PathSegment`s. The previous
/// implementation round-tripped through a string and called
/// `Ident::new(&snake, ...)` on the result, which panicked when the
/// user supplied a non-path expression (e.g. `#[observer(123)]`).
/// `Path::parse` rejects those at the entry point now.
fn path_to_snake_module_path(model: &Path) -> TokenStream2 {
    // Path::parse always yields at least one segment; an empty path
    // is not constructible via `syn::parse_str::<Path>`.
    let segments = &model.segments;
    let last_seg = segments
        .last()
        .expect("Path::parse guarantees at least one segment");
    let snake = to_snake(&last_seg.ident.to_string());
    let snake_ident: syn::Ident = syn::Ident::new(&snake, last_seg.ident.span());

    if segments.len() == 1 {
        // Preserve the leading `::` (absolute path) on the
        // sole-segment case for consistency with the multi-segment
        // branch — only matters when the user writes `::User`,
        // which is syntactically legal but unusual.
        if model.leading_colon.is_some() {
            quote! { ::#snake_ident }
        } else {
            quote! { #snake_ident }
        }
    } else {
        // Walk all-but-last segments verbatim, then append the
        // snake-cased final segment. Each segment is already a valid
        // ident — no string round-trip / re-parse needed.
        let prefix_segments = segments.iter().take(segments.len() - 1);
        let leading = if model.leading_colon.is_some() {
            quote! { :: }
        } else {
            quote! {}
        };
        quote! { #leading #(#prefix_segments)::*::#snake_ident }
    }
}

/// Emit an adapter `Listener<events::E>` for a non-cancellable method.
///
/// The adapter is a zero-sized struct unique per
/// `(observer × method)` so the `Listener<E>` blanket impl per event
/// type doesn't collide. Registration goes through `EventFacade::listen`.
fn emit_non_cancellable_adapter(
    observer_ident: &syn::Ident,
    module_path: &TokenStream2,
    method: &str,
    event_ident: &syn::Ident,
) -> TokenStream2 {
    let adapter_struct = format_ident!("__{}_adapter_{}", observer_ident, method);
    let call_expr = emit_adapter_call(method);

    quote! {
        #[allow(non_camel_case_types)]
        struct #adapter_struct;

        #[::suprnova::__async_trait::async_trait]
        impl ::suprnova::events::Listener<#module_path::events::#event_ident>
            for #adapter_struct
        {
            async fn handle(
                &self,
                event: &#module_path::events::#event_ident,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                let obs = #observer_ident;
                #call_expr
            }
        }

        ::suprnova::events::EventFacade::listen::<
            #module_path::events::#event_ident,
            #adapter_struct,
        >(
            ::std::sync::Arc::new(#adapter_struct),
        ).await;
    }
}

/// Emit the call expression that the non-cancellable adapter's
/// `handle` body executes. Each match arm uses the event struct's
/// field layout emitted by `model/events.rs`.
fn emit_adapter_call(method: &str) -> TokenStream2 {
    match method {
        // `Retrieving` carries no payload — fires once per query.
        "retrieving" => quote! { obs.retrieving().await },
        // Single-model events.
        "retrieved" => quote! { obs.retrieved(&event.model).await },
        "created" => quote! { obs.created(&event.model).await },
        "saved" => quote! { obs.saved(&event.model).await },
        "trashed" => quote! { obs.trashed(&event.model).await },
        "restored" => quote! { obs.restored(&event.model).await },
        "force_deleting" => quote! { obs.force_deleting(&event.model).await },
        "force_deleted" => quote! { obs.force_deleted(&event.model).await },
        // `Updated` carries `(previous, current)`.
        "updated" => quote! { obs.updated(&event.previous, &event.current).await },
        // `Deleted` carries `(model, is_force)`.
        "deleted" => quote! { obs.deleted(&event.model, event.is_force).await },
        // `Replicating` carries `source` plus `Arc<Mutex<replica>>`.
        // Lock the replica for the duration of the trait-method call;
        // listeners may mutate it in place (clear timestamps, reset
        // flags, ...).
        "replicating" => quote! {
            let mut replica = event.replica.lock().await;
            obs.replicating(&event.source, &mut *replica).await
        },
        other => {
            let msg =
                format!("internal error: unhandled non-cancellable observer method `{other}`");
            quote! { compile_error!(#msg) }
        }
    }
}

/// Emit an adapter `CancellableListener<events::E>` for a cancellable
/// method. Registration goes through `listen_cancellable` and the
/// adapter returns `EventResult` instead of `Result<(), _>`.
fn emit_cancellable_adapter(
    observer_ident: &syn::Ident,
    module_path: &TokenStream2,
    method: &str,
    event_ident: &syn::Ident,
) -> TokenStream2 {
    let adapter_struct = format_ident!("__{}_cancellable_adapter_{}", observer_ident, method);
    let call_expr = emit_cancellable_adapter_call(method);

    quote! {
        #[allow(non_camel_case_types)]
        struct #adapter_struct;

        #[::suprnova::__async_trait::async_trait]
        impl ::suprnova::eloquent::events::CancellableListener<
            #module_path::events::#event_ident,
        > for #adapter_struct
        {
            async fn handle(
                &self,
                event: &#module_path::events::#event_ident,
            ) -> ::suprnova::eloquent::events::EventResult {
                let obs = #observer_ident;
                #call_expr
            }
        }

        ::suprnova::eloquent::events::listen_cancellable::<
            #module_path::events::#event_ident,
            #adapter_struct,
        >(
            ::std::sync::Arc::new(#adapter_struct),
        ).await;
    }
}

/// Emit the call expression for a cancellable adapter's `handle` body.
/// `Saving` / `Creating` / `Updating` carry `Arc<Mutex<Attrs>>` so the
/// adapter locks, deref-muts, and hands `&mut Attrs` to the trait
/// method — the one place where the observer's `&mut Attrs` claim is
/// actually honoured.
fn emit_cancellable_adapter_call(method: &str) -> TokenStream2 {
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
        // `Deleting` carries `(model, is_force)` — no mutex, since
        // there's nothing to mutate before delete.
        "deleting" => quote! { obs.deleting(&event.model, event.is_force).await },
        // `Restoring` carries the model only.
        "restoring" => quote! { obs.restoring(&event.model).await },
        other => {
            let msg = format!("internal error: unhandled cancellable observer method `{other}`");
            quote! { compile_error!(#msg) }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Domain 5 audit M-D5-7 regression: the `#[suprnova::observer(M)]`
    //! macro used to accept any `syn::Expr` as the model argument and
    //! then panic at `Ident::new` if the expression wasn't path-shaped
    //! (`observer(123)` → "rustc panicked"). The fix swapped `Expr` for
    //! `syn::Path` at the entry point — non-path inputs reject cleanly
    //! through syn's parser, and `path_to_snake_module_path` walks
    //! typed segments that are already valid idents.
    //!
    //! These tests lock in the segment-walk semantics on the happy
    //! paths so a future refactor of the snake-module-path helper
    //! can't silently break them.
    use super::*;
    use syn::parse_str;

    fn render(path: &Path) -> String {
        path_to_snake_module_path(path).to_string().replace(' ', "")
    }

    #[test]
    fn single_segment_camelcase_snake_cases() {
        let p: Path = parse_str("User").unwrap();
        assert_eq!(render(&p), "user");
    }

    #[test]
    fn multi_segment_only_last_is_snake_cased() {
        let p: Path = parse_str("crate::models::User").unwrap();
        // Earlier segments pass through verbatim; only `User` →
        // `user`.
        assert_eq!(render(&p), "crate::models::user");
    }

    #[test]
    fn super_relative_path_handled() {
        let p: Path = parse_str("super::User").unwrap();
        assert_eq!(render(&p), "super::user");
    }

    #[test]
    fn leading_colon_absolute_path_preserved() {
        let p: Path = parse_str("::crate::models::User").unwrap();
        // Leading `::` survives the walk; trailing segment snake-cased.
        let rendered = render(&p);
        assert!(
            rendered.starts_with("::") && rendered.ends_with("::user"),
            "expected leading `::` preserved + user suffix; got: {rendered}",
        );
    }

    #[test]
    fn camelcase_with_multiple_words_snake_cased() {
        let p: Path = parse_str("MorphPost").unwrap();
        assert_eq!(render(&p), "morph_post");
    }

    #[test]
    fn non_path_input_rejected_by_parse_path() {
        // The actual panic class M-D5-7 catches: `#[observer(123)]`
        // used to panic at Ident::new on the snake-cased "123".
        // Now `parse_str::<Path>` rejects at the entry, never reaching
        // the snake-case helper.
        let result: syn::Result<Path> = parse_str("123");
        assert!(
            result.is_err(),
            "syn::Path::parse must reject numeric-only input \
             (pre-fix, Expr accepted this and panicked downstream)",
        );

        let result: syn::Result<Path> = parse_str("2 + 2");
        assert!(
            result.is_err(),
            "syn::Path::parse must reject arithmetic expressions",
        );
    }
}
