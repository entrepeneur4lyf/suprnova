use console::style;
use std::fs;
use std::path::Path;

use crate::templates::{self, Frontend};
use crate::ui;

const DATA_TEMPLATE: &str = r#"//! {name} — unified inbound + outbound DTO.

use suprnova::Data;
use validator::Validate;

#[derive(Data, Validate)]
pub struct {name} {{
    pub id: i64,
    // Add fields here.
    //
    // Available field attributes:
    //   #[data(input_only)]     — accepted on Deserialize, omitted from Serialize
    //   #[data(output_only)]    — rejected on Deserialize, included in Serialize
    //   #[data(allow_include)]  — registers as ?include=-eligible (default-deny)
    //
    // For PATCH endpoints, use suprnova::data::Field<T> to distinguish
    // absent from null. For lazy outbound fields, use suprnova::inertia::Prop<T>.
}}
"#;

pub fn run(name: String, data: bool) {
    if data {
        run_data_struct(name);
    } else {
        run_inertia_page(name);
    }
}

fn run_data_struct(name: String) {
    let struct_name = to_pascal_case(&name);

    if !is_valid_rust_identifier(&struct_name) {
        ui::error(&format!("'{}' is not a valid struct name", name));
        std::process::exit(1);
    }

    let file_name = to_snake_case(&struct_name);
    let props_dir = Path::new("app/src/props");
    let props_file = props_dir.join(format!("{}.rs", file_name));

    // Create the props directory if it doesn't exist.
    if !props_dir.exists()
        && let Err(e) = fs::create_dir_all(props_dir)
    {
        ui::error(&format!("Failed to create directory app/src/props: {}", e));
        std::process::exit(1);
    }

    if props_file.exists() {
        ui::warning(&format!(
            "Props struct '{}' already exists at {}",
            struct_name,
            props_file.display()
        ));
        std::process::exit(0);
    }

    let content = DATA_TEMPLATE.replace("{name}", &struct_name);

    if let Err(e) = fs::write(&props_file, &content) {
        ui::error(&format!("Failed to write props file: {}", e));
        std::process::exit(1);
    }
    ui::success(&format!("Created {}", props_file.display()));

    ui::br();
    ui::info(&format!(
        "Data struct {} created at {}",
        style(&struct_name).cyan().bold(),
        style(props_file.display().to_string().as_str()).dim(),
    ));
    ui::br();
    ui::hint("Use in a controller with automatic serde + validation:");
    ui::command(&format!(
        "let dto: {} = req.validate_json().await?;",
        struct_name
    ));
    ui::br();
}

fn run_inertia_page(name: String) {
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
    ui::command(&format!(
        "inertia_response!(&req, \"{}\", props)",
        page_name
    ));
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

fn is_valid_rust_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
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
