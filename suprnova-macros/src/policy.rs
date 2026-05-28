//! `#[policy(UserTy, ResourceTy)]` — registers each method of the impl block
//! as a Gate action via `inventory::submit!`.
//!
//! The action name is derived from the method name + resource kind:
//! `view` + `Comment` → `"view-comment"`.
//!
//! Each method's return type selects the registration path: `-> bool` routes
//! to `Gate::define`, and `-> Response` (the authorization `Response`, also
//! reachable as the crate-root alias `GateResponse`) routes to
//! `Gate::define_with` so a denial can carry a message, code, and HTTP status.
//! Any other return type — including a missing one — is a compile error.
//!
//! Because `inventory::submit!` requires `'static` constants, we emit free-
//! function shims that delegate to the impl methods, then reference those by
//! name in the submission.

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{
    ImplItem, ItemImpl, Meta, ReturnType, Token, Type, parse_macro_input, punctuated::Punctuated,
};

/// Convert a PascalCase identifier to kebab-case.
///
/// `"Post"` → `"post"`, `"UserProfile"` → `"user-profile"`.
fn pascal_to_kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('-');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Which `Gate` registration a policy method routes to, decided by its return
/// type.
enum PolicyReturn {
    /// `-> bool` → `Gate::define` (a bare allow/deny).
    Bool,
    /// `-> Response` / `-> GateResponse` → `Gate::define_with` (a rich denial
    /// carrying message / code / status).
    Response,
}

/// Classify a policy method's return type by its final path segment, so
/// `bool`, `Response`, `GateResponse`, and the fully-qualified
/// `suprnova::authorization::Response` all resolve. Returns `None` for an
/// unsupported or missing return type — the caller turns that into a spanned
/// compile error.
fn classify_return(output: &ReturnType) -> Option<PolicyReturn> {
    let ReturnType::Type(_, ty) = output else {
        // `-> ()` (or an elided return) carries no allow/deny decision.
        return None;
    };
    let Type::Path(tp) = ty.as_ref() else {
        return None;
    };
    match tp.path.segments.last()?.ident.to_string().as_str() {
        "bool" => Some(PolicyReturn::Bool),
        "Response" | "GateResponse" => Some(PolicyReturn::Response),
        _ => None,
    }
}

pub fn policy(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr with Punctuated::<Meta, Token![,]>::parse_terminated);
    let item = parse_macro_input!(item as ItemImpl);

    let mut iter = args.iter();

    let user_ty = match iter.next() {
        Some(Meta::Path(p)) => p.clone(),
        _ => {
            return syn::Error::new(
                Span::call_site(),
                "#[policy] requires (UserType, ResourceType)",
            )
            .to_compile_error()
            .into();
        }
    };

    let resource_ty = match iter.next() {
        Some(Meta::Path(p)) => p.clone(),
        _ => {
            return syn::Error::new(
                Span::call_site(),
                "#[policy] requires (UserType, ResourceType)",
            )
            .to_compile_error()
            .into();
        }
    };

    let resource_ident = resource_ty
        .segments
        .last()
        .expect("#[policy] resource type must be a simple path")
        .ident
        .to_string();
    let resource_lower = pascal_to_kebab(&resource_ident);

    // Extract the self-type identifier for mangling shim names.
    let self_ty_ident = match item.self_ty.as_ref() {
        Type::Path(tp) => tp
            .path
            .segments
            .last()
            .expect("#[policy] self type must be a simple path")
            .ident
            .to_string(),
        _ => {
            return syn::Error::new(
                Span::call_site(),
                "#[policy] can only be applied to a named impl block",
            )
            .to_compile_error()
            .into();
        }
    };

    let self_ty = &item.self_ty;
    let items = &item.items;

    let mut shims = Vec::new();
    let mut submits = Vec::new();

    for impl_item in items {
        if let ImplItem::Fn(m) = impl_item {
            // Policy methods are synchronous predicates; async authorization
            // belongs on `Gate::define_async`. Reject `async fn` with a clear
            // message rather than a downstream "expected bool, found Future".
            if m.sig.asyncness.is_some() {
                return syn::Error::new_spanned(
                    m.sig.fn_token,
                    "#[policy] methods must be synchronous; register async \
                     authorization with `Gate::define_async`",
                )
                .to_compile_error()
                .into();
            }

            let fn_name = m.sig.ident.to_string();
            let action = format!("{fn_name}-{resource_lower}");
            let method_ident = &m.sig.ident;

            // The method's return type picks the shim return type and the
            // `Gate` registration: `bool` → `define`, `Response` →
            // `define_with`. Anything else is a spanned compile error.
            let (shim_ret, gate_method) = match classify_return(&m.sig.output) {
                Some(PolicyReturn::Bool) => (quote!(bool), quote!(define)),
                Some(PolicyReturn::Response) => (
                    quote!(::suprnova::authorization::Response),
                    quote!(define_with),
                ),
                None => {
                    return syn::Error::new_spanned(
                        method_ident,
                        format!(
                            "#[policy] method `{fn_name}` must return `bool` or \
                             `suprnova::authorization::Response`"
                        ),
                    )
                    .to_compile_error()
                    .into();
                }
            };

            // Build a unique shim name: __policy_<SelfType>_<method>
            let shim_name = format!(
                "__policy_{self_ty_ident}_{fn_name}",
                self_ty_ident = self_ty_ident.to_lowercase(),
                fn_name = fn_name
            );
            let shim_ident = syn::Ident::new(&shim_name, m.sig.ident.span());

            // The shim delegates to the concrete policy method.
            shims.push(quote! {
                #[allow(non_snake_case)]
                fn #shim_ident(user: &#user_ty, resource: &#resource_ty) -> #shim_ret {
                    #self_ty::#method_ident(user, resource)
                }
            });

            // inventory::submit! referencing only the shim name (which is 'static).
            submits.push(quote! {
                ::suprnova::inventory::submit! {
                    ::suprnova::authorization::__PolicyRegistration {
                        register: || {
                            ::suprnova::Gate::#gate_method::<#user_ty, #resource_ty>(
                                #action,
                                #shim_ident,
                            );
                        },
                    }
                }
            });
        }
    }

    let expanded = quote! {
        impl #self_ty {
            #(#items)*
        }
        #(#shims)*
        #(#submits)*
    };

    expanded.into()
}
