use console::style;
use std::fs;
use std::path::Path;

use crate::templates;
use crate::ui;

pub fn run(name: String) {
    if !is_valid_identifier(&name) {
        ui::error(&format!("'{}' is not a valid Rust identifier", name));
        std::process::exit(1);
    }

    let struct_name = if name.ends_with("Middleware") {
        name.clone()
    } else {
        format!("{}Middleware", name)
    };
    let file_name = to_snake_case(&name.trim_end_matches("Middleware"));

    let middleware_dir = Path::new("src/middleware");
    let middleware_file = middleware_dir.join(format!("{}.rs", file_name));
    let mod_file = middleware_dir.join("mod.rs");

    if !middleware_dir.exists() {
        if let Err(e) = fs::create_dir_all(middleware_dir) {
            ui::error(&format!("Failed to create middleware directory: {}", e));
            std::process::exit(1);
        }
        ui::success("Created src/middleware directory");
    }

    if middleware_file.exists() {
        ui::warning(&format!(
            "Middleware '{}' already exists at {}",
            struct_name,
            middleware_file.display()
        ));
        std::process::exit(1);
    }

    let middleware_content = templates::middleware_template(&name, &struct_name);

    if let Err(e) = fs::write(&middleware_file, middleware_content) {
        ui::error(&format!("Failed to write middleware file: {}", e));
        std::process::exit(1);
    }
    ui::success(&format!("Created {}", middleware_file.display()));

    if mod_file.exists() {
        if let Err(e) = update_mod_file(&mod_file, &file_name, &struct_name) {
            ui::error(&format!("Failed to update mod.rs: {}", e));
            std::process::exit(1);
        }
        ui::success("Updated src/middleware/mod.rs");
    } else {
        let mod_content = format!(
            "//! Application middleware\n\nmod {};\n\npub use {}::{};\n",
            file_name, file_name, struct_name
        );
        if let Err(e) = fs::write(&mod_file, mod_content) {
            ui::error(&format!("Failed to create mod.rs: {}", e));
            std::process::exit(1);
        }
        ui::success("Created src/middleware/mod.rs");
    }

    ui::br();
    ui::info(&format!(
        "Middleware {} created",
        style(&struct_name).cyan().bold()
    ));
    ui::br();
    ui::hint("Import and use in routes:");
    ui::command(&format!("use crate::middleware::{};", struct_name));
    ui::command(&format!(
        ".get(\"/path\", handler).middleware({})",
        struct_name
    ));
    ui::br();
    ui::hint("Or apply globally in bootstrap.rs:");
    ui::command(&format!("global_middleware!(middleware::{})", struct_name));
    ui::br();
}

fn is_valid_identifier(name: &str) -> bool {
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

fn update_mod_file(mod_file: &Path, file_name: &str, struct_name: &str) -> Result<(), String> {
    let content =
        fs::read_to_string(mod_file).map_err(|e| format!("Failed to read mod.rs: {}", e))?;

    let mod_decl = format!("mod {};", file_name);
    if content.contains(&mod_decl) {
        return Err(format!("Module '{}' already declared in mod.rs", file_name));
    }

    let mut lines: Vec<&str> = content.lines().collect();

    let mut last_mod_idx = None;
    for (i, line) in lines.iter().enumerate() {
        if line.trim().starts_with("mod ") {
            last_mod_idx = Some(i);
        }
    }

    let mod_insert_idx = match last_mod_idx {
        Some(idx) => idx + 1,
        None => {
            let mut insert_idx = 0;
            for (i, line) in lines.iter().enumerate() {
                if line.starts_with("//!") || line.is_empty() {
                    insert_idx = i + 1;
                } else {
                    break;
                }
            }
            insert_idx
        }
    };
    lines.insert(mod_insert_idx, &mod_decl);

    let pub_use_decl = format!("pub use {}::{};", file_name, struct_name);
    let mut last_pub_use_idx = None;
    for (i, line) in lines.iter().enumerate() {
        if line.trim().starts_with("pub use ") {
            last_pub_use_idx = Some(i);
        }
    }

    match last_pub_use_idx {
        Some(idx) => {
            lines.insert(idx + 1, &pub_use_decl);
        }
        None => {
            let mut insert_idx = mod_insert_idx + 1;
            while insert_idx < lines.len() && lines[insert_idx].trim().starts_with("mod ") {
                insert_idx += 1;
            }
            if insert_idx < lines.len() && !lines[insert_idx].is_empty() {
                lines.insert(insert_idx, "");
                insert_idx += 1;
            }
            lines.insert(insert_idx, &pub_use_decl);
        }
    }

    let new_content = lines.join("\n");
    fs::write(mod_file, new_content).map_err(|e| format!("Failed to write mod.rs: {}", e))?;

    Ok(())
}
