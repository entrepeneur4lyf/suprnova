//! `describe!` macro for grouping related tests
//!
//! Generates a module with properly structured tests, similar to Jest's describe blocks.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{braced, LitStr, Token};

/// Convert a string to snake_case for module/function names
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
        } else if c.is_whitespace() || c == '-' || c == '_' {
            if !result.ends_with('_') && !result.is_empty() {
                result.push('_');
            }
        }
    }

    // Remove trailing underscore
    while result.ends_with('_') {
        result.pop();
    }

    result
}

/// Parse describe macro arguments: describe!("Name", { ... })
struct DescribeArgs {
    name: LitStr,
    body: TokenStream2,
}

impl Parse for DescribeArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;

        let content;
        braced!(content in input);
        let body: TokenStream2 = content.parse()?;

        Ok(Self { name, body })
    }
}

pub fn describe_impl(input: TokenStream) -> TokenStream {
    let args = match syn::parse::<DescribeArgs>(input) {
        Ok(args) => args,
        Err(e) => return e.to_compile_error().into(),
    };

    let name_str = args.name.value();
    let mod_name = format_ident!("{}", to_snake_case(&name_str));
    let body = args.body;

    let output = quote! {
        mod #mod_name {
            use super::*;

            #body
        }
    };

    output.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("ListTodoAction"), "list_todo_action");
        assert_eq!(to_snake_case("with pagination"), "with_pagination");
        assert_eq!(to_snake_case("returns empty list"), "returns_empty_list");
        assert_eq!(to_snake_case("UserService"), "user_service");
        assert_eq!(to_snake_case("API endpoints"), "api_endpoints");
    }
}
