use chrono::Local;
use console::style;
use std::fs;
use std::path::Path;

use crate::ui;

pub fn run(name: String) {
    // Convert to snake_case for file name
    let file_name = to_snake_case(&name);

    // Validate the resulting name is a valid Rust identifier
    if !is_valid_identifier(&file_name) {
        ui::error(&format!("'{}' is not a valid migration name", name));
        std::process::exit(1);
    }

    // Extract table name from migration name (e.g., create_users_table -> users)
    let table_name = extract_table_name(&file_name);
    let table_enum_name = to_pascal_case(&table_name);

    let migrations_dir = Path::new("src/migrations");

    // Check if migrations directory exists, create if not
    if !migrations_dir.exists() {
        if let Err(e) = fs::create_dir_all(migrations_dir) {
            ui::error(&format!("Failed to create migrations directory: {}", e));
            std::process::exit(1);
        }
        ui::success("Created src/migrations directory");
    }

    // Generate timestamp-based filename: m{YYYYMMDD}_{HHMMSS}_{name}.rs
    let timestamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let migration_file_name = format!("m{}_{}", timestamp, file_name);
    let migration_file = migrations_dir.join(format!("{}.rs", migration_file_name));
    let mod_file = migrations_dir.join("mod.rs");

    // Check if migration file already exists (unlikely with timestamp)
    if migration_file.exists() {
        ui::warning(&format!(
            "Migration '{}' already exists at {}",
            migration_file_name,
            migration_file.display()
        ));
        std::process::exit(0);
    }

    // Generate migration file content
    let migration_content = migration_template(&table_name, &table_enum_name);

    // Write migration file
    if let Err(e) = fs::write(&migration_file, &migration_content) {
        ui::error(&format!("Failed to write migration file: {}", e));
        std::process::exit(1);
    }
    ui::success(&format!("Created {}", migration_file.display()));

    // Update or create mod.rs
    if mod_file.exists() {
        if let Err(e) = update_mod_file(&mod_file, &migration_file_name) {
            ui::error(&format!("Failed to update mod.rs: {}", e));
            std::process::exit(1);
        }
        ui::success("Updated src/migrations/mod.rs");
    } else {
        let mod_content = migrator_mod_template(&migration_file_name);
        if let Err(e) = fs::write(&mod_file, mod_content) {
            ui::error(&format!("Failed to create mod.rs: {}", e));
            std::process::exit(1);
        }
        ui::success("Created src/migrations/mod.rs");
    }

    ui::br();
    ui::info(&format!(
        "Migration {} created",
        style(&migration_file_name).cyan().bold()
    ));
    ui::br();
    ui::hint("Edit the migration file to define your schema, then:");
    ui::command("suprnova migrate");
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
        } else if c == '-' || c == ' ' {
            result.push('_');
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

/// Extract table name from migration name
/// e.g., "create_users_table" -> "users"
/// e.g., "add_email_to_users" -> "users"
/// e.g., "users" -> "users"
fn extract_table_name(name: &str) -> String {
    // Common patterns: create_X_table, add_Y_to_X, drop_X_table
    if name.starts_with("create_") && name.ends_with("_table") {
        let without_prefix = name.strip_prefix("create_").unwrap();
        let without_suffix = without_prefix.strip_suffix("_table").unwrap();
        return without_suffix.to_string();
    }

    if name.contains("_to_") {
        // add_X_to_Y -> Y
        if let Some(pos) = name.rfind("_to_") {
            return name[pos + 4..].to_string();
        }
    }

    if name.starts_with("drop_") && name.ends_with("_table") {
        let without_prefix = name.strip_prefix("drop_").unwrap();
        let without_suffix = without_prefix.strip_suffix("_table").unwrap();
        return without_suffix.to_string();
    }

    // Default: use the name as-is (assume it's a table name)
    name.to_string()
}

fn migration_template(table_name: &str, table_enum_name: &str) -> String {
    format!(
        r#"use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {{
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {{
        manager
            .create_table(
                Table::create()
                    .table({table_enum_name}::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new({table_enum_name}::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new({table_enum_name}::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new({table_enum_name}::UpdatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await
    }}

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {{
        manager
            .drop_table(Table::drop().table({table_enum_name}::Table).to_owned())
            .await
    }}
}}

/// Table and column identifiers for {table_name}
#[derive(DeriveIden)]
enum {table_enum_name} {{
    Table,
    Id,
    CreatedAt,
    UpdatedAt,
}}
"#,
        table_name = table_name,
        table_enum_name = table_enum_name
    )
}

fn migrator_mod_template(migration_name: &str) -> String {
    format!(
        r#"pub use sea_orm_migration::prelude::*;

mod {migration_name};

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {{
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {{
        vec![
            Box::new({migration_name}::Migration),
        ]
    }}
}}
"#,
        migration_name = migration_name
    )
}

fn update_mod_file(mod_file: &Path, migration_name: &str) -> Result<(), String> {
    let content =
        fs::read_to_string(mod_file).map_err(|e| format!("Failed to read mod.rs: {}", e))?;

    let mod_decl = format!("mod {};", migration_name);

    // Check if already declared
    if content.contains(&mod_decl) {
        return Ok(());
    }

    let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

    // Find position to insert mod declaration (after other mod declarations)
    let mut last_mod_idx = None;
    for (i, line) in lines.iter().enumerate() {
        if line.trim().starts_with("mod ") && !line.contains("mod tests") {
            last_mod_idx = Some(i);
        }
    }

    // Insert mod declaration
    let insert_idx = match last_mod_idx {
        Some(idx) => idx + 1,
        None => {
            // Find after "pub use sea_orm_migration::prelude::*;"
            let mut insert_idx = 0;
            for (i, line) in lines.iter().enumerate() {
                if line.contains("sea_orm_migration") || line.is_empty() {
                    insert_idx = i + 1;
                } else if line.starts_with("mod ") || line.starts_with("pub struct") {
                    break;
                }
            }
            insert_idx
        }
    };
    lines.insert(insert_idx, mod_decl);

    // Update migrations() vec to include the new migration
    let box_new_line = format!("            Box::new({}::Migration),", migration_name);
    let mut insert_vec_idx = None;

    for (i, line) in lines.iter().enumerate() {
        // Handle empty vec![] on single line
        if line.contains("vec![]") {
            // Replace vec![] with vec![\n    Box::new(...)\n]
            lines[i] = line.replace("vec![]", &format!("vec![\n{}\n        ]", box_new_line));
            let new_content = lines.join("\n") + "\n";
            fs::write(mod_file, new_content)
                .map_err(|e| format!("Failed to write mod.rs: {}", e))?;
            return Ok(());
        }
        // Handle multi-line vec![ ... ]
        if line.contains("vec![") && !line.contains("vec![]") {
            // Find closing ] to insert before it
            for (j, inner_line) in lines.iter().enumerate().skip(i + 1) {
                if inner_line.trim() == "]" || inner_line.trim().starts_with("]") {
                    insert_vec_idx = Some(j);
                    break;
                }
            }
            break;
        }
    }

    match insert_vec_idx {
        Some(idx) => {
            lines.insert(idx, box_new_line);
        }
        None => {
            return Err(format!(
                "Could not locate the migrations() vec in src/migrations/mod.rs \
                 (the closing `]` may be on the same line as the last entry). \
                 Please add `{}` to the vec manually.",
                box_new_line.trim()
            ));
        }
    }

    let new_content = lines.join("\n") + "\n";
    fs::write(mod_file, new_content).map_err(|e| format!("Failed to write mod.rs: {}", e))?;

    Ok(())
}
