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
            if ident == "migrator" {
                input.parse::<syn::Token![=]>()?;
                migrator = Some(input.parse()?);
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
