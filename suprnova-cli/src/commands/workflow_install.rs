//! workflow:install command - Install workflow migrations

use std::fs;
use std::path::Path;

use crate::templates;
use crate::ui;

const WORKFLOWS_MIGRATION: &str = "m20240101_000003_create_workflows_table";
const WORKFLOW_STEPS_MIGRATION: &str = "m20240101_000004_create_workflow_steps_table";

pub fn run() {
    let migrations_dir = Path::new("src/migrations");
    let mod_file = migrations_dir.join("mod.rs");

    if !Path::new("src").exists() {
        ui::error("Not in a Suprnova project root directory");
        std::process::exit(1);
    }

    if !migrations_dir.exists() {
        if let Err(e) = fs::create_dir_all(migrations_dir) {
            ui::error(&format!("Failed to create migrations directory: {}", e));
            std::process::exit(1);
        }
        ui::success("Created src/migrations/");
    }

    let workflows_file = migrations_dir.join(format!("{}.rs", WORKFLOWS_MIGRATION));
    let steps_file = migrations_dir.join(format!("{}.rs", WORKFLOW_STEPS_MIGRATION));

    if !workflows_file.exists() {
        if let Err(e) = fs::write(&workflows_file, templates::create_workflows_migration()) {
            ui::error(&format!("Failed to write workflows migration: {}", e));
            std::process::exit(1);
        }
        ui::success(&format!("Created {}", workflows_file.display()));
    } else {
        ui::warning(&format!("{} already exists", workflows_file.display()));
    }

    if !steps_file.exists() {
        if let Err(e) = fs::write(&steps_file, templates::create_workflow_steps_migration()) {
            ui::error(&format!("Failed to write workflow steps migration: {}", e));
            std::process::exit(1);
        }
        ui::success(&format!("Created {}", steps_file.display()));
    } else {
        ui::warning(&format!("{} already exists", steps_file.display()));
    }

    if mod_file.exists() {
        if let Err(e) = update_mod_file(&mod_file, WORKFLOWS_MIGRATION) {
            ui::error(&format!("Failed to update mod.rs: {}", e));
            std::process::exit(1);
        }
        if let Err(e) = update_mod_file(&mod_file, WORKFLOW_STEPS_MIGRATION) {
            ui::error(&format!("Failed to update mod.rs: {}", e));
            std::process::exit(1);
        }
        ui::success("Updated src/migrations/mod.rs");
    } else {
        let mod_content = format!(
            "pub use sea_orm_migration::prelude::*;\n\nmod {};\nmod {};\n\n\
            pub struct Migrator;\n\n\
            #[async_trait::async_trait]\n\
            impl MigratorTrait for Migrator {{\n\
                fn migrations() -> Vec<Box<dyn MigrationTrait>> {{\n\
                    vec![\n\
                        Box::new({}::Migration),\n\
                        Box::new({}::Migration),\n\
                    ]\n\
                }}\n\
            }}\n",
            WORKFLOWS_MIGRATION,
            WORKFLOW_STEPS_MIGRATION,
            WORKFLOWS_MIGRATION,
            WORKFLOW_STEPS_MIGRATION
        );

        if let Err(e) = fs::write(&mod_file, mod_content) {
            ui::error(&format!("Failed to create mod.rs: {}", e));
            std::process::exit(1);
        }
        ui::success("Created src/migrations/mod.rs");
    }

    ui::br();
    ui::info("Workflow migrations installed.");
    ui::hint("Run to apply:");
    ui::command("suprnova migrate");
    ui::br();
}

fn update_mod_file(mod_file: &Path, module_name: &str) -> Result<(), String> {
    let content =
        fs::read_to_string(mod_file).map_err(|e| format!("Failed to read mod.rs: {}", e))?;

    let mod_decl = format!("mod {};", module_name);
    let entry_line = format!("            Box::new({}::Migration),", module_name);

    let lines: Vec<&str> = content.lines().collect();

    let has_mod = content.contains(&mod_decl);
    let has_entry = content.contains(&entry_line);

    let mut last_mod_idx = None;
    let mut last_entry_idx = None;
    let mut vec_idx = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("mod ") || trimmed.starts_with("pub mod ") {
            last_mod_idx = Some(i);
        }
        if trimmed.contains("Box::new(") {
            last_entry_idx = Some(i);
        }
        if trimmed.contains("vec![") {
            vec_idx = Some(i);
        }
    }

    let mut new_lines = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        new_lines.push(line.to_string());

        if !has_mod {
            if let Some(idx) = last_mod_idx {
                if i == idx {
                    new_lines.push(mod_decl.clone());
                }
            }
        }

        if !has_entry {
            if let Some(idx) = last_entry_idx {
                if i == idx {
                    new_lines.push(entry_line.clone());
                }
            } else if let Some(idx) = vec_idx {
                if i == idx {
                    new_lines.push(entry_line.clone());
                }
            }
        }
    }

    if !has_mod && last_mod_idx.is_none() {
        new_lines.insert(1, mod_decl.clone());
    }

    let new_content = new_lines.join("\n") + "\n";
    fs::write(mod_file, new_content).map_err(|e| format!("Failed to write mod.rs: {}", e))?;

    Ok(())
}
