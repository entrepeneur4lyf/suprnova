//! `suprnova make:command <name>` — scaffold a new console command.
//!
//! Creates `src/commands/<snake>.rs` with a `#[command]`-annotated
//! async fn, then ensures `src/commands/mod.rs` declares the new
//! module (`pub mod <snake>;`).
//!
//! Name normalization:
//!   - `greet`           → file `greet.rs`, command `greet`
//!   - `CleanCache`      → file `clean_cache.rs`, command `clean-cache`
//!   - `clean_cache`     → file `clean_cache.rs`, command `clean-cache`
//!   - `clean-cache`     → file `clean_cache.rs`, command `clean-cache`
//!   - `mail:send`       → file `mail_send.rs`, command `mail:send`
//!
//! Inputs containing `:` are used verbatim as the registered command
//! name (Laravel-style namespacing: `db:seed`, `cache:clear`). The
//! Rust fn name is always snake_case, and the file matches.

use console::style;
use std::fs;
use std::path::Path;

use crate::templates;
use crate::ui;

pub fn run(name: String) {
    let snake = to_snake_case_relaxed(&name);

    if !is_valid_identifier(&snake) {
        ui::error(&format!("'{}' is not a valid command name", name));
        ui::hint("Use letters, digits, '_', '-', or ':' (':' = namespace separator).");
        std::process::exit(1);
    }

    let command_name = pick_command_name(&name, &snake);

    let commands_dir = Path::new("src/commands");
    let command_file = commands_dir.join(format!("{}.rs", snake));
    let mod_file = commands_dir.join("mod.rs");

    if !commands_dir.exists() {
        if let Err(e) = fs::create_dir_all(commands_dir) {
            ui::error(&format!(
                "Failed to create commands directory at {}: {}",
                commands_dir.display(),
                e
            ));
            std::process::exit(1);
        }
        ui::success(&format!("Created {}", commands_dir.display()));
    }

    if command_file.exists() {
        ui::warning(&format!(
            "Command '{}' already exists at {}",
            snake,
            command_file.display()
        ));
        std::process::exit(0);
    }

    if mod_file.exists() {
        let mod_content = fs::read_to_string(&mod_file).unwrap_or_default();
        let mod_decl = format!("mod {};", snake);
        let pub_mod_decl = format!("pub mod {};", snake);
        if mod_content.contains(&mod_decl) || mod_content.contains(&pub_mod_decl) {
            ui::warning(&format!(
                "Module '{}' is already declared in src/commands/mod.rs",
                snake
            ));
            std::process::exit(0);
        }
    }

    let content = templates::command_template(&snake, &command_name);
    if let Err(e) = fs::write(&command_file, content) {
        ui::error(&format!("Failed to write command file: {}", e));
        std::process::exit(1);
    }
    ui::success(&format!("Created {}", command_file.display()));

    if mod_file.exists() {
        if let Err(e) = update_mod_file(&mod_file, &snake) {
            ui::error(&format!("Failed to update commands/mod.rs: {}", e));
            std::process::exit(1);
        }
        ui::success("Updated src/commands/mod.rs");
    } else {
        let mod_content = format!(
            "//! Application-defined console commands. Each module \
             registers an `#[command]`-annotated async fn via inventory.\n\npub mod {};\n",
            snake
        );
        if let Err(e) = fs::write(&mod_file, mod_content) {
            ui::error(&format!("Failed to create commands/mod.rs: {}", e));
            std::process::exit(1);
        }
        ui::success("Created src/commands/mod.rs");
    }

    ui::br();
    ui::info(&format!(
        "Command {} created (registered as {})",
        style(&snake).cyan().bold(),
        style(&command_name).cyan().bold()
    ));
    ui::br();
    ui::hint("Run it through the project console:");
    ui::command(&format!("cargo run --bin console -- {}", command_name));
    ui::br();
    ui::hint("Make sure `pub mod commands;` is declared in src/lib.rs so the");
    ui::hint("inventory submission is link-reachable from the console binary.");
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

/// Convert "any input" → snake_case suitable for a Rust fn/file name.
/// PascalCase, camelCase, kebab-case, and `ns:command` all collapse to
/// `lower_snake_case`. Colons and dashes become underscores.
fn to_snake_case_relaxed(s: &str) -> String {
    let mut result = String::new();
    let mut prev_was_lower = false;
    for c in s.chars() {
        if c == '-' || c == ':' || c == ' ' || c == '_' {
            if !result.ends_with('_') && !result.is_empty() {
                result.push('_');
            }
            prev_was_lower = false;
        } else if c.is_uppercase() {
            if prev_was_lower {
                result.push('_');
            }
            for lc in c.to_lowercase() {
                result.push(lc);
            }
            prev_was_lower = false;
        } else {
            result.push(c);
            prev_was_lower = c.is_lowercase();
        }
    }
    // Trim trailing underscores produced by collapsing consecutive separators
    while result.ends_with('_') {
        result.pop();
    }
    result
}

/// Pick the registered command name. Inputs containing `:` are used
/// verbatim (Laravel namespace style); otherwise the snake-cased fn
/// name with `_` → `-` produces a kebab-case command.
fn pick_command_name(raw: &str, snake_fn: &str) -> String {
    if raw.contains(':') {
        raw.to_string()
    } else {
        snake_fn.replace('_', "-")
    }
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
            let mut idx = 0;
            for (i, line) in lines.iter().enumerate() {
                if line.starts_with("//!") || line.is_empty() {
                    idx = i + 1;
                } else {
                    break;
                }
            }
            idx
        }
    };
    lines.insert(insert_idx, &pub_mod_decl);

    let new_content = lines.join("\n");
    fs::write(mod_file, new_content).map_err(|e| format!("Failed to write mod.rs: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_snake_case_handles_pascal_kebab_colon() {
        assert_eq!(to_snake_case_relaxed("greet"), "greet");
        assert_eq!(to_snake_case_relaxed("CleanCache"), "clean_cache");
        assert_eq!(to_snake_case_relaxed("clean-cache"), "clean_cache");
        assert_eq!(to_snake_case_relaxed("clean_cache"), "clean_cache");
        assert_eq!(to_snake_case_relaxed("mail:send"), "mail_send");
        assert_eq!(to_snake_case_relaxed("DbSeed"), "db_seed");
        // Consecutive uppercase letters stay grouped; the underscore lands
        // only on the upper→lower transition. `HTTPServerStart` → the
        // `HTTP` initialism collapses into the next segment cleanly.
        assert_eq!(to_snake_case_relaxed("HTTPServerStart"), "httpserver_start");
    }

    #[test]
    fn pick_command_name_preserves_namespacing_else_kebabs() {
        assert_eq!(pick_command_name("greet", "greet"), "greet");
        assert_eq!(pick_command_name("CleanCache", "clean_cache"), "clean-cache");
        assert_eq!(pick_command_name("clean-cache", "clean_cache"), "clean-cache");
        assert_eq!(pick_command_name("mail:send", "mail_send"), "mail:send");
        assert_eq!(pick_command_name("db:seed", "db_seed"), "db:seed");
    }
}
