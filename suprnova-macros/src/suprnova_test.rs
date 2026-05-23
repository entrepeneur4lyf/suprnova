//! `#[suprnova_test]` attribute macro for database-enabled tests
//!
//! This macro simplifies writing tests that need database access by automatically
//! setting up an in-memory SQLite database with migrations applied.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, FnArg, ItemFn, Pat, Type};

/// Parse the macro attributes
struct SuprnovaTestArgs {
    migrator: Option<syn::Path>,
}

impl syn::parse::Parse for SuprnovaTestArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut migrator = None;

        while !input.is_empty() {
            let ident: syn::Ident = input.parse()?;
            // Domain 5 audit M-D5-5: unknown keys (e.g. typo
            // `migrtor = MyMigrator`) used to be silently ignored
            // because the `if ident == "migrator"` branch had no else
            // — the next iteration just parsed the next ident as if
            // nothing was wrong. Reject unknown keys with a span-
            // pointed compile error so typos surface immediately.
            if ident == "migrator" {
                input.parse::<syn::Token![=]>()?;
                migrator = Some(input.parse()?);
            } else {
                return Err(syn::Error::new(
                    ident.span(),
                    format!(
                        "unknown #[suprnova_test(...)] key `{ident}` — supported keys: migrator"
                    ),
                ));
            }

            if input.peek(syn::Token![,]) {
                input.parse::<syn::Token![,]>()?;
            }
        }

        Ok(Self { migrator })
    }
}

/// Check if a type is `TestDatabase`
fn is_test_database_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last() {
            return segment.ident == "TestDatabase";
        }
    false
}

/// Find the parameter name for TestDatabase if it exists
fn find_db_param_name(func: &ItemFn) -> Option<syn::Ident> {
    for arg in &func.sig.inputs {
        if let FnArg::Typed(pat_type) = arg
            && is_test_database_type(&pat_type.ty)
                && let Pat::Ident(pat_ident) = &*pat_type.pat {
                    return Some(pat_ident.ident.clone());
                }
    }
    None
}

pub fn suprnova_test_impl(attr: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as SuprnovaTestArgs);
    let input_fn = parse_macro_input!(input as ItemFn);

    let fn_name = &input_fn.sig.ident;
    let fn_block = &input_fn.block;
    let fn_attrs: Vec<_> = input_fn
        .attrs
        .iter()
        .filter(|attr| !attr.path().is_ident("suprnova_test"))
        .collect();
    let fn_vis = &input_fn.vis;

    // Default to crate::migrations::Migrator if not specified
    let migrator_type = args
        .migrator
        .unwrap_or_else(|| syn::parse_quote!(crate::migrations::Migrator));

    // Check if function takes TestDatabase parameter
    let db_param_name = find_db_param_name(&input_fn);

    let setup_and_body = if let Some(param_name) = db_param_name {
        // Function has TestDatabase parameter - bind it
        quote! {
            // Bootstrap services so #[injectable] types are available
            ::suprnova::App::init();
            ::suprnova::App::boot_services();
            let #param_name = ::suprnova::testing::TestDatabase::fresh::<#migrator_type>()
                .await
                .expect("Failed to set up test database");
            #fn_block
        }
    } else {
        // No TestDatabase parameter - still set up but don't bind
        quote! {
            // Bootstrap services so #[injectable] types are available
            ::suprnova::App::init();
            ::suprnova::App::boot_services();
            let _db = ::suprnova::testing::TestDatabase::fresh::<#migrator_type>()
                .await
                .expect("Failed to set up test database");
            #fn_block
        }
    };

    let output = quote! {
        #(#fn_attrs)*
        #[::tokio::test]
        #fn_vis async fn #fn_name() {
            #setup_and_body
        }
    };

    output.into()
}

#[cfg(test)]
mod tests {
    //! Domain 5 audit M-D5-5 regression: unknown
    //! `#[suprnova_test(...)]` keys must produce a compile error
    //! rather than being silently ignored. Previously the parser
    //! had `if ident == "migrator"` with no else branch, so a typo
    //! like `migrtor = MyMigrator` advanced past the `=` token to
    //! the next iteration with no signal anything went wrong.

    use super::*;
    use syn::parse2;

    #[test]
    fn known_key_parses_cleanly() {
        let tokens: proc_macro2::TokenStream = "migrator = crate::Migrator".parse().unwrap();
        let parsed: SuprnovaTestArgs = parse2(tokens).expect("known key must parse");
        assert!(parsed.migrator.is_some());
    }

    #[test]
    fn empty_attribute_parses_cleanly() {
        // `#[suprnova_test]` with no args is the common case — must
        // remain valid.
        let tokens: proc_macro2::TokenStream = "".parse().unwrap();
        let parsed: SuprnovaTestArgs = parse2(tokens).expect("empty attribute must parse");
        assert!(parsed.migrator.is_none());
    }

    #[test]
    fn unknown_key_is_rejected() {
        // Typo: `migrtor` is what the user wrote when they meant
        // `migrator`. Old behaviour silently kept the default
        // migrator; new behaviour rejects with a span-pointed error.
        let tokens: proc_macro2::TokenStream = "migrtor = crate::Migrator".parse().unwrap();
        let err = parse2::<SuprnovaTestArgs>(tokens)
            .err()
            .expect("unknown key must reject");
        let msg = err.to_string();
        assert!(
            msg.contains("unknown") && msg.contains("migrtor"),
            "error must name the bad key; got: {msg}"
        );
        assert!(
            msg.contains("migrator"),
            "error must hint at the supported key; got: {msg}"
        );
    }
}
