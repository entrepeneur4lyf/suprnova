//! `#[policy(UserTy, ResourceTy)]` — registers each method of the impl block
//! as a Gate action via `inventory::submit!`.
//!
//! The action name is derived from the method name + resource kind:
//! `view` + `Comment` → `"view-comment"`.
//!
//! Because `inventory::submit!` requires `'static` constants, we emit free-
//! function shims that delegate to the impl methods, then reference those by
//! name in the submission.

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{ImplItem, ItemImpl, Meta, Token, Type, parse_macro_input, punctuated::Punctuated};

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
            let fn_name = m.sig.ident.to_string();
            let action = format!("{fn_name}-{resource_lower}");
            let method_ident = &m.sig.ident;

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
                fn #shim_ident(user: &#user_ty, resource: &#resource_ty) -> bool {
                    #self_ty::#method_ident(user, resource)
                }
            });

            // inventory::submit! referencing only the shim name (which is 'static).
            submits.push(quote! {
                ::suprnova::inventory::submit! {
                    ::suprnova::authorization::__PolicyRegistration {
                        register: || {
                            ::suprnova::Gate::define::<#user_ty, #resource_ty>(
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
