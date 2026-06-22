use console::style;
use std::fs;
use std::path::Path;

use crate::templates;
use crate::ui;

pub fn run(name: String) {
    // Convert to PascalCase for struct name
    let struct_name = to_pascal_case(&name);

    // Append "Action" suffix if not already present
    let struct_name = if struct_name.ends_with("Action") {
        struct_name
    } else {
        format!("{}Action", struct_name)
    };

    // Convert to snake_case for file name
    let file_name = to_snake_case(&struct_name);

    // Validate the resulting name is a valid Rust identifier
    if !is_valid_identifier(&file_name) {
        ui::error(&format!("'{}' is not a valid action name", name));
        std::process::exit(1);
    }

    let actions_dir = Path::new("src/actions");
    let action_file = actions_dir.join(format!("{}.rs", file_name));
    let mod_file = actions_dir.join("mod.rs");

    // Check if actions directory exists
    if !actions_dir.exists() {
        ui::error("Actions directory not found at src/actions");
        ui::hint("Make sure you're in a Suprnova project root directory.");
        std::process::exit(1);
    }

    // Check if action file already exists
    if action_file.exists() {
        ui::warning(&format!(
            "Action '{}' already exists at {}",
            struct_name,
            action_file.display()
        ));
        std::process::exit(0);
    }

    // Check if module is already declared in mod.rs
    if mod_file.exists() {
        let mod_content = fs::read_to_string(&mod_file).unwrap_or_default();
        let mod_decl = format!("mod {};", file_name);
        let pub_mod_decl = format!("pub mod {};", file_name);
        if mod_content.contains(&mod_decl) || mod_content.contains(&pub_mod_decl) {
            ui::warning(&format!(
                "Module '{}' is already declared in src/actions/mod.rs",
                file_name
            ));
            std::process::exit(0);
        }
    }

    // Generate action file content
    let action_content = templates::action_template(&file_name, &struct_name);

    // Write action file
    if let Err(e) = fs::write(&action_file, action_content) {
        ui::error(&format!("Failed to write action file: {}", e));
        std::process::exit(1);
    }
    ui::success(&format!("Created {}", action_file.display()));

    // Update mod.rs
    if mod_file.exists() {
        if let Err(e) = update_mod_file(&mod_file, &file_name) {
            ui::error(&format!("Failed to update mod.rs: {}", e));
            std::process::exit(1);
        }
        ui::success("Updated src/actions/mod.rs");
    } else {
        let mod_content = format!("pub mod {};\n", file_name);
        if let Err(e) = fs::write(&mod_file, mod_content) {
            ui::error(&format!("Failed to create mod.rs: {}", e));
            std::process::exit(1);
        }
        ui::success("Created src/actions/mod.rs");
    }

    ui::br();
    ui::info(&format!(
        "Action {} created",
        style(&struct_name).cyan().bold()
    ));
    ui::br();
    ui::hint("Resolve from container in your controller:");
    ui::command(&format!(
        "let action: {} = App::get().unwrap();",
        struct_name
    ));
    ui::command("action.execute();");
    ui::br();
}

fn is_valid_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    let mut chars = name.chars();

    // First character must be letter or underscore
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }

    // Rest must be alphanumeric or underscore
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

    // Find position to insert pub mod declaration (after other pub mod declarations)
    let mut lines: Vec<&str> = content.lines().collect();

    // Find the last pub mod declaration line
    let mut last_pub_mod_idx = None;
    for (i, line) in lines.iter().enumerate() {
        if line.trim().starts_with("pub mod ") {
            last_pub_mod_idx = Some(i);
        }
    }

    // Insert pub mod declaration
    let insert_idx = match last_pub_mod_idx {
        Some(idx) => idx + 1,
        None => {
            // If no pub mod declarations, insert at the beginning (after any doc comments)
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

    let new_content = lines.join("\n") + "\n";
    fs::write(mod_file, new_content).map_err(|e| format!("Failed to write mod.rs: {}", e))?;

    Ok(())
}
