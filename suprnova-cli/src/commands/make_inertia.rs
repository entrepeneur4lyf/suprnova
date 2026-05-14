use console::style;
use std::fs;
use std::path::Path;

use crate::templates::{self, Frontend};
use crate::ui;

pub fn run(name: String) {
    let _ = dotenvy::from_path(".env");

    let frontend = Frontend::detect_from_env();
    let ext = frontend.page_ext();
    let page_name = to_page_name(&name);

    if !is_valid_component_name(&page_name) {
        ui::error(&format!("'{}' is not a valid page name", name));
        std::process::exit(1);
    }

    let pages_dir = Path::new("frontend/src/pages");
    let page_file = pages_dir.join(format!("{}.{}", page_name, ext));

    if !pages_dir.exists() {
        ui::error("Pages directory not found at frontend/src/pages");
        ui::hint("Make sure you're in a Suprnova project root directory.");
        std::process::exit(1);
    }

    if page_file.exists() {
        ui::warning(&format!(
            "Page '{}' already exists at {}",
            page_name,
            page_file.display()
        ));
        std::process::exit(0);
    }

    let page_content = templates::inertia_page_template(&page_name, frontend);

    if let Err(e) = fs::write(&page_file, page_content) {
        ui::error(&format!("Failed to write page file: {}", e));
        std::process::exit(1);
    }
    ui::success(&format!("Created {}", page_file.display()));

    ui::br();
    ui::info(&format!(
        "Page {} ({}) created",
        style(&page_name).cyan().bold(),
        style(frontend.as_str()).dim(),
    ));
    ui::br();
    ui::hint("Use the page in a controller:");
    ui::command(&format!("inertia_response!(&req, \"{}\", props)", page_name));
    ui::br();
}

fn is_valid_component_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric())
}

fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;
    for c in s.chars() {
        if c == '_' || c == '-' || c == ' ' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_uppercase().next().unwrap());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }
    result
}

fn to_page_name(input: &str) -> String {
    let pascal = to_pascal_case(input);
    if pascal.ends_with("Page") {
        pascal
    } else {
        format!("{}Page", pascal)
    }
}
