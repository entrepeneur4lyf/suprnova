//! `#[command]` — attribute macro for registering an async fn as a
//! console command.
//!
//! Applied to an `async fn(Vec<String>) -> Result<(), FrameworkError>`,
//! the macro:
//!
//!   1. preserves the original function so it can still be called
//!      directly from tests or other Rust code
//!   2. generates a `__suprnova_command_runner_<fn>` adapter matching
//!      the `CommandHandler` fn-pointer signature
//!   3. submits a `CommandEntry { name, description, handler }` into
//!      the global `inventory::collect!` registry
//!
//! Attributes:
//!
//!   - `name = "db:seed"` (required) — the invocation name used on the
//!     command line (allowed to contain `:` etc.)
//!   - `description = "..."` (optional, default `""`) — one-line help
//!     text shown by `console --help`
//!
//! Modelled after `#[workflow]` (see `suprnova-macros/src/workflow.rs`),
//! which is the existing precedent in this crate for "attribute on an
//! async fn + inventory::submit a fn-pointer registry entry."

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse::Parser, punctuated::Punctuated, Expr, ItemFn, Lit, Meta, Token};

#[derive(Default)]
struct CommandAttrs {
    name: Option<String>,
    description: Option<String>,
}

fn parse_attrs(attr: TokenStream) -> syn::Result<CommandAttrs> {
    let mut out = CommandAttrs::default();
    if attr.is_empty() {
        return Ok(out);
    }

    let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
    let metas = parser.parse(attr)?;

    for meta in metas {
        let Meta::NameValue(nv) = meta else {
            return Err(syn::Error::new_spanned(
                meta,
                "#[command(...)] expects `name = \"...\"` and optional `description = \"...\"`",
            ));
        };

        let key = nv
            .path
            .get_ident()
            .ok_or_else(|| {
                syn::Error::new_spanned(&nv.path, "expected identifier key in #[command(...)]")
            })?
            .to_string();

        let Expr::Lit(expr_lit) = &nv.value else {
            return Err(syn::Error::new_spanned(
                &nv.value,
                format!("`{key}` in #[command(...)] expects a string literal"),
            ));
        };
        let Lit::Str(s) = &expr_lit.lit else {
            return Err(syn::Error::new_spanned(
                &expr_lit.lit,
                format!("`{key}` in #[command(...)] expects a string literal"),
            ));
        };
        let v = s.value();

        match key.as_str() {
            "name" => out.name = Some(v),
            "description" => out.description = Some(v),
            other => {
                return Err(syn::Error::new_spanned(
                    &nv.path,
                    format!(
                        "unknown key `{other}` in #[command(...)] — supported keys: name, description"
                    ),
                ));
            }
        }
    }

    Ok(out)
}

pub fn command_impl(attr: TokenStream, input: TokenStream) -> TokenStream {
    let attrs = match parse_attrs(attr) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };

    let input_fn = match syn::parse::<ItemFn>(input) {
        Ok(f) => f,
        Err(e) => return e.to_compile_error().into(),
    };

    if input_fn.sig.asyncness.is_none() {
        return syn::Error::new_spanned(
            input_fn.sig.fn_token,
            "#[command] requires an async fn",
        )
        .to_compile_error()
        .into();
    }

    let Some(name) = attrs.name else {
        return syn::Error::new_spanned(
            &input_fn.sig.ident,
            "#[command] requires a `name = \"...\"` attribute",
        )
        .to_compile_error()
        .into();
    };
    let description = attrs.description.unwrap_or_default();

    let fn_name = &input_fn.sig.ident;
    let runner_ident = format_ident!("__suprnova_command_runner_{}", fn_name);

    let expanded = quote! {
        #input_fn

        #[doc(hidden)]
        fn #runner_ident(
            __args: ::std::vec::Vec<::std::string::String>,
        ) -> ::std::pin::Pin<
            ::std::boxed::Box<
                dyn ::std::future::Future<
                        Output = ::std::result::Result<(), ::suprnova::FrameworkError>,
                    > + ::std::marker::Send,
            >,
        > {
            ::std::boxed::Box::pin(async move { #fn_name(__args).await })
        }

        ::suprnova::inventory::submit! {
            ::suprnova::CommandEntry {
                name: #name,
                description: #description,
                handler: #runner_ident,
            }
        }
    };

    expanded.into()
}
