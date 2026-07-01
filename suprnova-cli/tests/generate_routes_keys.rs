//! Route TS keys must always be valid, unquoted TypeScript identifiers.
//!
//! When several routes in a module share one handler (e.g. a `static_files::serve`
//! whitelist mapping many favicon/asset URLs), the first keeps the handler name
//! and the rest get a key derived from the route name/path. Those derived keys
//! must be sanitized: a file extension ('.') or a leading digit would otherwise
//! produce output like `favicon_16x16.png: (...) => ...` that fails tsc/svelte-check.

use suprnova_cli::commands::generate_routes::{
    GeneratedRoute, HttpMethod, RouteDefinition, generate_typescript,
};

fn route(path: &str, handler_fn: &str, name: Option<&str>) -> GeneratedRoute {
    GeneratedRoute {
        definition: RouteDefinition {
            method: HttpMethod::Get,
            path: path.to_string(),
            handler_module: "controllers::static_files".to_string(),
            handler_fn: handler_fn.to_string(),
            name: name.map(|n| n.to_string()),
            path_params: Vec::new(),
        },
        handler_info: None,
        request_struct: None,
    }
}

/// Slice out the `static_files: { ... }` block from the generated controllers object.
/// A module block closes with a line at 2-space indent (`\n  }`); route lines are
/// indented 4 spaces, so that marker unambiguously ends the block (inline arrow-body
/// `}`s sit mid-line and are never preceded by a newline + 2 spaces).
fn static_files_block(ts: &str) -> String {
    let start = ts
        .find("static_files: {")
        .expect("static_files block not found");
    let after = &ts[start..];
    let end = after.find("\n  }").expect("block close not found");
    after[..end].to_string()
}

#[test]
fn duplicate_handler_keys_are_valid_identifiers() {
    // All five routes hit the same `serve` handler, so four get path/name-derived keys.
    let routes = vec![
        route("/favicon.ico", "serve", None),
        route("/favicon-16x16.png", "serve", None),
        route("/site.webmanifest", "serve", None),
        route("/2fa.json", "serve", None), // leading digit after sanitizing
        route("/whatever", "serve", Some("assets.hero-image")), // dashed name segment
    ];

    let ts = generate_typescript(&routes);
    let block = static_files_block(&ts);

    // First occurrence keeps the handler name.
    assert!(block.contains("serve:"), "block: {block}");

    // No key may contain a '.' — that is the exact bug (member access, not a key).
    assert!(
        !block.contains(".png:") && !block.contains(".webmanifest:") && !block.contains(".json:"),
        "leaked a dotted key: {block}"
    );

    // Extension dots become underscores.
    assert!(block.contains("favicon_16x16_png:"), "block: {block}");
    assert!(block.contains("site_webmanifest:"), "block: {block}");

    // A key that would start with a digit is prefixed so it stays a legal identifier.
    assert!(block.contains("_2fa_json:"), "block: {block}");

    // A dashed route-name segment is sanitized too (no '-' in an identifier).
    assert!(block.contains("hero_image:"), "block: {block}");
    assert!(!block.contains("hero-image:"), "leaked dashed key: {block}");
}

#[test]
fn unique_handler_names_are_untouched() {
    // Distinct handlers keep their clean names — no spurious sanitizing.
    let routes = vec![
        route("/favicon.ico", "serve", None),
        route("/health", "healthcheck", None),
    ];
    let ts = generate_typescript(&routes);
    let block = static_files_block(&ts);
    assert!(block.contains("serve:"), "block: {block}");
    assert!(block.contains("healthcheck:"), "block: {block}");
}
