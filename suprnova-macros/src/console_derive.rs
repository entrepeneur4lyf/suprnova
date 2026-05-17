//! `#[derive(Command)]` — register a `clap::Parser`-deriving struct as
//! a typed console command.
//!
//! Goes on top of `#[derive(clap::Parser)]`. Reads `#[console(name =
//! "...", description = "...")]` from the same struct for the
//! Suprnova-side registration metadata; clap's `#[command(...)]`
//! attribute stays as the source of truth for arg-parsing config.
//!
//! Generated code:
//!
//! ```rust,ignore
//! fn __suprnova_clap_builder_Greet() -> ::clap::Command {
//!     <Greet as ::clap::CommandFactory>::command()
//!         .name("greet")          // overrides whatever clap picked
//!         .about("Greet someone") // ditto
//! }
//!
//! fn __suprnova_runner_Greet(matches: &::clap::ArgMatches) -> Pin<Box<...>> {
//!     Box::pin(async move {
//!         let parsed = <Greet as ::clap::FromArgMatches>::from_arg_matches(matches)?;
//!         <Greet as ::suprnova::TypedCommand>::run(parsed).await
//!     })
//! }
//!
//! inventory::submit! {
//!     ::suprnova::CommandEntry {
//!         name: "greet",
//!         description: "Greet someone",
//!         clap_builder: __suprnova_clap_builder_Greet,
//!         handler: __suprnova_runner_Greet,
//!     }
//! }
//! ```
//!
//! The user provides the `impl TypedCommand` separately.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, DeriveInput, Expr, Lit};

#[derive(Default)]
struct ConsoleAttrs {
    name: Option<String>,
    description: Option<String>,
}

fn parse_attrs(input: &DeriveInput) -> syn::Result<ConsoleAttrs> {
    let mut out = ConsoleAttrs::default();

    for attribute in &input.attrs {
        if !attribute.path().is_ident("console") {
            continue;
        }

        attribute.parse_nested_meta(|nested| {
            let key = nested
                .path
                .get_ident()
                .ok_or_else(|| nested.error("expected identifier key in #[console(...)]"))?
                .to_string();

            let value: Expr = nested.value()?.parse()?;
            let Expr::Lit(expr_lit) = &value else {
                return Err(nested.error(format!(
                    "`{key}` in #[console(...)] expects a string literal"
                )));
            };
            let Lit::Str(s) = &expr_lit.lit else {
                return Err(nested.error(format!(
                    "`{key}` in #[console(...)] expects a string literal"
                )));
            };
            let v = s.value();

            match key.as_str() {
                "name" => out.name = Some(v),
                "description" => out.description = Some(v),
                other => {
                    return Err(nested.error(format!(
                        "unknown key `{other}` in #[console(...)] — supported keys: name, description"
                    )));
                }
            }
            Ok(())
        })?;
    }

    Ok(out)
}

pub fn derive_command_impl(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let attrs = match parse_attrs(&input) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };

    let Some(name) = attrs.name else {
        return syn::Error::new_spanned(
            &input.ident,
            "#[derive(Command)] requires a `#[console(name = \"...\")]` attribute on the struct",
        )
        .to_compile_error()
        .into();
    };
    let description = attrs.description.unwrap_or_default();

    let ty = &input.ident;
    let builder_ident = format_ident!("__suprnova_console_builder_{}", ty);
    let runner_ident = format_ident!("__suprnova_console_runner_{}", ty);

    let expanded = quote! {
        #[doc(hidden)]
        #[allow(non_snake_case)]
        fn #builder_ident() -> ::suprnova::__clap::Command {
            <#ty as ::suprnova::__clap::CommandFactory>::command()
                .name(#name)
                .about(#description)
        }

        #[doc(hidden)]
        #[allow(non_snake_case)]
        fn #runner_ident(
            __matches: &::suprnova::__clap::ArgMatches,
        ) -> ::std::pin::Pin<
            ::std::boxed::Box<
                dyn ::std::future::Future<
                        Output = ::std::result::Result<(), ::suprnova::FrameworkError>,
                    > + ::std::marker::Send,
            >,
        > {
            let __parsed = match <#ty as ::suprnova::__clap::FromArgMatches>::from_arg_matches(__matches) {
                ::std::result::Result::Ok(v) => v,
                ::std::result::Result::Err(e) => {
                    return ::std::boxed::Box::pin(async move {
                        ::std::result::Result::Err(::suprnova::FrameworkError::internal(
                            format!("console arg parse: {}", e),
                        ))
                    });
                }
            };
            ::std::boxed::Box::pin(async move {
                <#ty as ::suprnova::TypedCommand>::run(__parsed).await
            })
        }

        ::suprnova::inventory::submit! {
            ::suprnova::CommandEntry {
                name: #name,
                description: #description,
                clap_builder: #builder_ident,
                handler: #runner_ident,
            }
        }
    };

    expanded.into()
}
