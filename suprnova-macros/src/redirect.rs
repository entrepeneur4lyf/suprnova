use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use std::path::{Path, PathBuf};
use syn::{LitStr, parse::Parse, parse::ParseStream, parse_macro_input};

use crate::utils::levenshtein_distance;

/// Custom parser for redirect! macro
pub struct RedirectInput {
    pub route_name: LitStr,
}

impl Parse for RedirectInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Ok(RedirectInput {
            route_name: input.parse()?,
        })
    }
}

/// Implementation for the redirect! macro
pub fn redirect_impl(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as RedirectInput);
    let route_name = input.route_name.value();
    let route_lit = &input.route_name;

    // Validate the route exists at compile time
    if let Err(err) = validate_route_exists(&route_name, route_lit.span()) {
        return err.to_compile_error().into();
    }

    // Generate the redirect builder
    let expanded = quote! {
        ::suprnova::Redirect::route(#route_lit)
    };

    expanded.into()
}

fn validate_route_exists(route_name: &str, span: Span) -> Result<(), syn::Error> {
    // Get the manifest directory
    let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(dir) => dir,
        Err(_) => return Ok(()), // Skip validation if env not available
    };

    let project_root = PathBuf::from(&manifest_dir);

    // Scan routes.rs (or main entrypoint) for route definitions
    let available_routes = extract_route_names(&project_root);

    if available_routes.is_empty() {
        // No routes found, skip validation (might be running in different context)
        return Ok(());
    }

    if !available_routes.contains(&route_name.to_string()) {
        let mut error_msg = format!("Route '{}' not found.", route_name);

        error_msg.push_str("\n\nAvailable routes:");
        for route in &available_routes {
            error_msg.push_str(&format!("\n  - {}", route));
        }

        // Suggest similar route names
        if let Some(suggestion) = find_similar_route(route_name, &available_routes) {
            error_msg.push_str(&format!("\n\nDid you mean '{}'?", suggestion));
        }

        return Err(syn::Error::new(span, error_msg));
    }

    Ok(())
}

fn extract_route_names(project_root: &Path) -> Vec<String> {
    // Try routes.rs first, fall back to cmd/main.rs or legacy src/main.rs
    let routes_rs = project_root.join("src").join("routes.rs");
    let cmd_main_rs = project_root.join("cmd").join("main.rs");
    let main_rs = project_root.join("src").join("main.rs");

    let content = std::fs::read_to_string(&routes_rs)
        .or_else(|_| std::fs::read_to_string(&cmd_main_rs))
        .or_else(|_| std::fs::read_to_string(&main_rs))
        .unwrap_or_default();

    if content.is_empty() {
        return Vec::new();
    }

    // Use regex to find .name("...") patterns
    let re = regex::Regex::new(r#"\.name\s*\(\s*"([^"]+)"\s*\)"#).unwrap();

    re.captures_iter(&content)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

fn find_similar_route(target: &str, available: &[String]) -> Option<String> {
    let target_lower = target.to_lowercase();

    // Check for case-insensitive exact match first
    for route in available {
        if route.to_lowercase() == target_lower {
            return Some(route.clone());
        }
    }

    // Find closest match using Levenshtein distance
    let mut best_match: Option<(String, usize)> = None;
    for route in available {
        let distance = levenshtein_distance(&target_lower, &route.to_lowercase());
        let threshold = std::cmp::max(2, target.len() / 3);
        if distance <= threshold
            && (best_match.is_none() || distance < best_match.as_ref().unwrap().1)
        {
            best_match = Some((route.clone(), distance));
        }
    }

    best_match.map(|(name, _)| name)
}
