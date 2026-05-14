use console::style;
use std::fs;
use std::path::Path;

use crate::templates;
use crate::ui;

pub fn run(name: String) {
    let file_name = to_snake_case(&name);
    let struct_name = to_pascal_case(&name);

    if !is_valid_identifier(&file_name) {
        ui::error(&format!("'{}' is not a valid error name", name));
        std::process::exit(1);
    }

    let errors_dir = Path::new("src/errors");
    let error_file = errors_dir.join(format!("{}.rs", file_name));
    let mod_file = errors_dir.join("mod.rs");

    if !Path::new("src").exists() {
        ui::error("src directory not found");
        ui::hint("Make sure you're in a Suprnova project root directory.");
        std::process::exit(1);
    }

    let created_dir = if !errors_dir.exists() {
        if let Err(e) = fs::create_dir_all(errors_dir) {
            ui::error(&format!("Failed to create errors directory: {}", e));
            std::process::exit(1);
        }
        ui::success("Created src/errors/");
        true
    } else {
        false
    };

    if error_file.exists() {
        ui::warning(&format!(
            "Error '{}' already exists at {}",
            struct_name,
            error_file.display()
        ));
        std::process::exit(0);
    }

    if mod_file.exists() {
        let mod_content = fs::read_to_string(&mod_file).unwrap_or_default();
        let mod_decl = format!("mod {};", file_name);
        let pub_mod_decl = format!("pub mod {};", file_name);
        if mod_content.contains(&mod_decl) || mod_content.contains(&pub_mod_decl) {
            ui::warning(&format!(
                "Module '{}' is already declared in src/errors/mod.rs",
                file_name
            ));
            std::process::exit(0);
        }
    }

    let error_content = templates::error_template(&struct_name);

    if let Err(e) = fs::write(&error_file, error_content) {
        ui::error(&format!("Failed to write error file: {}", e));
        std::process::exit(1);
    }
    ui::success(&format!("Created {}", error_file.display()));

    if mod_file.exists() {
        if let Err(e) = update_mod_file(&mod_file, &file_name) {
            ui::error(&format!("Failed to update mod.rs: {}", e));
            std::process::exit(1);
        }
        ui::success("Updated src/errors/mod.rs");
    } else {
        let mod_content = format!("pub mod {};\n", file_name);
        if let Err(e) = fs::write(&mod_file, mod_content) {
            ui::error(&format!("Failed to create mod.rs: {}", e));
            std::process::exit(1);
        }
        ui::success("Created src/errors/mod.rs");
    }

    ui::br();
    ui::info(&format!(
        "Error {} created",
        style(&struct_name).cyan().bold()
    ));
    ui::br();
    ui::hint("Import in your controller:");
    ui::command(&format!(
        "use crate::errors::{}::{};",
        file_name, struct_name
    ));
    ui::command(&format!("Err({})?", struct_name));
    ui::br();

    if created_dir {
        ui::warning("Make sure to add `mod errors;` to your src/lib.rs");
        ui::br();
    }
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

fn update_mod_file(mod_file: &Path, file_name: &str) -> Result<(), String> {
    let content =
        fs::read_to_string(mod_file).map_err(|e| format!("Failed to read mod.rs: {}", e))?;

    let pub_mod_decl = format!("pub mod {};", file_name);

    let mut lines: Vec<&str> = content.lines().collect();

    let mut last_pub_mod_idx = None;
    for (i, line) in lines.iter().enumerate() {
        if line.trim().starts_with("pub mod ") {
            last_pub_mod_idx = Some(i);
        }
    }

    let insert_idx = match last_pub_mod_idx {
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
    lines.insert(insert_idx, &pub_mod_decl);

    let new_content = lines.join("\n");
    fs::write(mod_file, new_content).map_err(|e| format!("Failed to write mod.rs: {}", e))?;

    Ok(())
}
