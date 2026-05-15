//! `test!` macro for individual test cases
//!
//! Creates tests with optional TestDatabase parameter and async support,
//! similar to Jest's test/it blocks.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{braced, parenthesized, Ident, LitStr, Token, Type};

/// Convert a string to snake_case for function names
fn to_snake_case(name: &str) -> String {
    let mut result = String::new();
    let mut prev_is_uppercase = false;

    for (i, c) in name.chars().enumerate() {
        if c.is_alphanumeric() {
            if c.is_uppercase() {
                if i > 0 && !prev_is_uppercase && !result.ends_with('_') {
                    result.push('_');
                }
                result.push(c.to_ascii_lowercase());
                prev_is_uppercase = true;
            } else {
                result.push(c);
                prev_is_uppercase = false;
            }
        } else if (c.is_whitespace() || c == '-' || c == '_')
            && !result.ends_with('_') && !result.is_empty() {
                result.push('_');
            }
    }

    // Remove trailing underscore
    while result.ends_with('_') {
        result.pop();
    }

    result
}

/// Parameter in the function signature
struct FnParam {
    name: Ident,
    ty: Type,
}

/// Arguments for the test! macro
/// Supports: test!("name", async fn(db: TestDatabase) { ... })
///           test!("name", async fn() { ... })
///           test!("name", fn() { ... })
struct TestArgs {
    name: LitStr,
    is_async: bool,
    params: Vec<FnParam>,
    body: TokenStream2,
}

impl Parse for TestArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Parse the test name string
        let name: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;

        // Check for async keyword
        let is_async = if input.peek(Token![async]) {
            input.parse::<Token![async]>()?;
            true
        } else {
            false
        };

        // Parse 'fn'
        input.parse::<Token![fn]>()?;

        // Parse parameters (param: Type, ...)
        let content;
        parenthesized!(content in input);

        let mut params = Vec::new();
        while !content.is_empty() {
            let param_name: Ident = content.parse()?;
            content.parse::<Token![:]>()?;
            let param_type: Type = content.parse()?;
            params.push(FnParam {
                name: param_name,
                ty: param_type,
            });

            if content.peek(Token![,]) {
                content.parse::<Token![,]>()?;
            }
        }

        // Parse the body
        let body_content;
        braced!(body_content in input);
        let body: TokenStream2 = body_content.parse()?;

        Ok(Self {
            name,
            is_async,
            params,
            body,
        })
    }
}

/// Check if a type path ends with "TestDatabase"
fn is_test_database(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last() {
            return segment.ident == "TestDatabase";
        }
    false
}

pub fn test_impl(input: TokenStream) -> TokenStream {
    let args = match syn::parse::<TestArgs>(input) {
        Ok(args) => args,
        Err(e) => return e.to_compile_error().into(),
    };

    let name_str = args.name.value();
    let fn_name = format_ident!("{}", to_snake_case(&name_str));
    let body = args.body;

    // Check if any parameter is TestDatabase
    let has_db_param = args.params.iter().any(|p| is_test_database(&p.ty));

    // Find the TestDatabase parameter name if it exists
    let db_param = args.params.iter().find(|p| is_test_database(&p.ty));

    if args.is_async {
        if has_db_param {
            // Async with TestDatabase - use suprnova_test
            let db_param_name = &db_param.unwrap().name;
            let output = quote! {
                #[::suprnova::suprnova_test]
                async fn #fn_name(#db_param_name: ::suprnova::testing::TestDatabase) {
                    // Set the test name for expect! macro output
                    ::suprnova::testing::set_current_test_name(Some(#name_str.to_string()));

                    // Run the test body
                    let __test_result = async {
                        #body
                    }.await;

                    // Clear the test name
                    ::suprnova::testing::set_current_test_name(None);

                    __test_result
                }
            };
            output.into()
        } else {
            // Async without TestDatabase - still use suprnova_test for consistency
            let output = quote! {
                #[::suprnova::suprnova_test]
                async fn #fn_name() {
                    // Set the test name for expect! macro output
                    ::suprnova::testing::set_current_test_name(Some(#name_str.to_string()));

                    // Run the test body
                    let __test_result = async {
                        #body
                    }.await;

                    // Clear the test name
                    ::suprnova::testing::set_current_test_name(None);

                    __test_result
                }
            };
            output.into()
        }
    } else {
        // Sync test - use regular #[test]
        let output = quote! {
            #[test]
            fn #fn_name() {
                // Set the test name for expect! macro output
                ::suprnova::testing::set_current_test_name(Some(#name_str.to_string()));

                // Run the test body
                let __test_result = {
                    #body
                };

                // Clear the test name
                ::suprnova::testing::set_current_test_name(None);

                __test_result
            }
        };
        output.into()
    }
}
