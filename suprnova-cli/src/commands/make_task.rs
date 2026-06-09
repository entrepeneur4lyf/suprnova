//! make:task command - Generate a new scheduled task

use console::style;
use std::fs;
use std::path::{Path, PathBuf};

use crate::templates;
use crate::ui;

pub fn run(name: String) {
    // Convert to PascalCase for struct name
    let struct_name = to_pascal_case(&name);

    // Append "Task" suffix if not already present
    let struct_name = if struct_name.ends_with("Task") {
        struct_name
    } else {
        format!("{}Task", struct_name)
    };

    // Convert to snake_case for file name
    let file_name = to_snake_case(&struct_name);

    // Validate the resulting name is a valid Rust identifier
    if !is_valid_identifier(&file_name) {
        ui::error(&format!("'{}' is not a valid task name", name));
        std::process::exit(1);
    }

    let tasks_dir = Path::new("src/tasks");
    let task_file = tasks_dir.join(format!("{}.rs", file_name));
    let mod_file = tasks_dir.join("mod.rs");
    let schedule_file = Path::new("src/schedule.rs");

    // Ensure we're in a Suprnova project (check for src directory)
    if !Path::new("src").exists() {
        ui::error("Not in a Suprnova project root directory");
        ui::hint("Make sure you're in a Suprnova project directory with a src/ folder.");
        std::process::exit(1);
    }

    // Create tasks directory if it doesn't exist
    if !tasks_dir.exists() {
        if let Err(e) = fs::create_dir_all(tasks_dir) {
            ui::error(&format!("Failed to create tasks directory: {}", e));
            std::process::exit(1);
        }
        ui::success("Created src/tasks/");

        let mod_content = templates::tasks_mod();
        if let Err(e) = fs::write(&mod_file, mod_content) {
            ui::error(&format!("Failed to create mod.rs: {}", e));
            std::process::exit(1);
        }
        ui::success("Created src/tasks/mod.rs");
    }

    // Create schedule.rs if it doesn't exist
    if !schedule_file.exists() {
        let schedule_content = templates::schedule_rs();
        if let Err(e) = fs::write(schedule_file, schedule_content) {
            ui::error(&format!("Failed to create schedule.rs: {}", e));
            std::process::exit(1);
        }
        ui::success("Created src/schedule.rs");
    }

    // Wire the lazily-created `schedule` + `tasks` modules into the project so
    // the application binary actually compiles and runs them: declare both in
    // src/lib.rs and add `.schedule(...)` to the builder chain. Idempotent, so
    // re-running make:task repairs wiring that was removed by hand.
    let lib_path = Path::new("src/lib.rs");
    if lib_path.exists() {
        match ensure_lib_modules(lib_path, &["schedule", "tasks"]) {
            Ok(added) => {
                for module in &added {
                    ui::success(&format!("Declared `pub mod {};` in src/lib.rs", module));
                }
            }
            Err(e) => ui::warning(&format!("Could not update src/lib.rs ({})", e)),
        }
    } else {
        ui::warning(
            "src/lib.rs not found — declare `pub mod schedule;` and `pub mod tasks;` yourself",
        );
    }

    match find_main_rs() {
        Some(main_path) => match ensure_application_schedule(&main_path) {
            Ok(true) => ui::success(&format!(
                ".schedule(...) wired into {}",
                main_path.display()
            )),
            Ok(false) => {}
            Err(e) => {
                ui::warning(&format!(
                    "Could not wire the scheduler automatically ({})",
                    e
                ));
                ui::hint(
                    "Add `.schedule(<crate>::schedule::register)` before `.run()` in your main.rs",
                );
            }
        },
        None => {
            ui::warning("Could not find cmd/main.rs or src/main.rs to wire the scheduler");
            ui::hint(
                "Add `.schedule(<crate>::schedule::register)` before `.run()` in your main.rs",
            );
        }
    }

    // Check if task file already exists
    if task_file.exists() {
        ui::warning(&format!(
            "Task '{}' already exists at {}",
            struct_name,
            task_file.display()
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
                "Module '{}' is already declared in src/tasks/mod.rs",
                file_name
            ));
            std::process::exit(0);
        }
    }

    // Generate task file content
    let task_content = templates::task_template(&file_name, &struct_name);

    // Write task file
    if let Err(e) = fs::write(&task_file, task_content) {
        ui::error(&format!("Failed to write task file: {}", e));
        std::process::exit(1);
    }
    ui::success(&format!("Created {}", task_file.display()));

    // Update mod.rs
    if let Err(e) = update_mod_file(&mod_file, &file_name, &struct_name) {
        ui::error(&format!("Failed to update mod.rs: {}", e));
        std::process::exit(1);
    }
    ui::success("Updated src/tasks/mod.rs");

    ui::br();
    ui::info(&format!(
        "Task {} created",
        style(&struct_name).cyan().bold()
    ));
    ui::br();
    ui::hint(&format!(
        "Implement your task logic in {}",
        task_file.display()
    ));
    ui::hint("Register the task in src/schedule.rs:");
    ui::command(&format!("use crate::tasks::{};", struct_name));
    ui::command(&format!(
        "schedule.add(schedule.task({}::new()).daily().name(\"{}\"));",
        struct_name, file_name
    ));
    ui::br();
    ui::hint("Run the scheduler:");
    ui::command("suprnova schedule:work");
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

fn update_mod_file(mod_file: &Path, file_name: &str, struct_name: &str) -> Result<(), String> {
    let content =
        fs::read_to_string(mod_file).map_err(|e| format!("Failed to read mod.rs: {}", e))?;

    let pub_mod_decl = format!("pub mod {};", file_name);
    let pub_use_decl = format!("pub use {}::{};", file_name, struct_name);

    // Find position to insert declarations
    let lines: Vec<&str> = content.lines().collect();

    // Find the last pub mod declaration line
    let mut last_pub_mod_idx = None;
    let mut last_pub_use_idx = None;

    for (i, line) in lines.iter().enumerate() {
        if line.trim().starts_with("pub mod ") {
            last_pub_mod_idx = Some(i);
        }
        if line.trim().starts_with("pub use ") {
            last_pub_use_idx = Some(i);
        }
    }

    // Build new content
    let mut new_lines: Vec<String> = Vec::new();

    // If we found existing pub mod declarations, insert after them
    if let Some(idx) = last_pub_mod_idx {
        for (i, line) in lines.iter().enumerate() {
            new_lines.push(line.to_string());
            if i == idx {
                new_lines.push(pub_mod_decl.clone());
            }
        }
    } else {
        // No existing pub mod declarations, add at the end (before empty lines)
        let mut content_end = lines.len();
        while content_end > 0 && lines[content_end - 1].trim().is_empty() {
            content_end -= 1;
        }

        for (i, line) in lines.iter().enumerate() {
            new_lines.push(line.to_string());
            if i == content_end.saturating_sub(1) || (content_end == 0 && i == 0) {
                new_lines.push(pub_mod_decl.clone());
            }
        }

        // If file was empty
        if lines.is_empty() {
            new_lines.push(pub_mod_decl.clone());
        }
    }

    // Now add pub use declaration if there are existing pub use declarations
    if last_pub_use_idx.is_some() {
        // Find the new position of the last pub use after our modification
        let mut insert_idx = None;
        for (i, line) in new_lines.iter().enumerate() {
            if line.trim().starts_with("pub use ") {
                insert_idx = Some(i);
            }
        }
        if let Some(idx) = insert_idx {
            new_lines.insert(idx + 1, pub_use_decl);
        }
    }

    let new_content = new_lines.join("\n");

    // Ensure file ends with newline
    let new_content = if new_content.ends_with('\n') {
        new_content
    } else {
        format!("{}\n", new_content)
    };

    fs::write(mod_file, new_content).map_err(|e| format!("Failed to write mod.rs: {}", e))?;

    Ok(())
}

/// Locate the application entrypoint: backend scaffolds put it at `cmd/main.rs`,
/// API scaffolds at `src/main.rs`. Returns the first that exists.
fn find_main_rs() -> Option<PathBuf> {
    ["cmd/main.rs", "src/main.rs"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

/// Idempotently declare each `pub mod <name>;` in `lib.rs`, writing the file
/// only when something changed. Returns the modules that were newly added.
fn ensure_lib_modules(lib_path: &Path, modules: &[&str]) -> Result<Vec<String>, String> {
    let content =
        fs::read_to_string(lib_path).map_err(|e| format!("Failed to read lib.rs: {}", e))?;
    let (new_content, added) = declare_lib_modules(&content, modules);
    if !added.is_empty() {
        fs::write(lib_path, &new_content).map_err(|e| format!("Failed to write lib.rs: {}", e))?;
    }
    Ok(added)
}

/// Pure core of [`ensure_lib_modules`]: add a `pub mod <name>;` line for each
/// module not already declared (either `pub mod` or bare `mod`), inserting after
/// the last existing module declaration. Returns the rewritten source and the
/// list of modules that were added.
fn declare_lib_modules(lib_src: &str, modules: &[&str]) -> (String, Vec<String>) {
    let trailing_newline = lib_src.ends_with('\n');
    let mut lines: Vec<String> = lib_src.lines().map(str::to_string).collect();
    let mut added = Vec::new();

    for &module in modules {
        let pub_decl = format!("pub mod {};", module);
        let bare_decl = format!("mod {};", module);
        let present = lines.iter().any(|l| {
            let t = l.trim();
            t == pub_decl.as_str() || t == bare_decl.as_str()
        });
        if present {
            continue;
        }
        let insert_at = lines
            .iter()
            .rposition(|l| {
                let t = l.trim();
                t.starts_with("pub mod ") || t.starts_with("mod ")
            })
            .map(|i| i + 1)
            .unwrap_or(lines.len());
        lines.insert(insert_at, pub_decl);
        added.push(module.to_string());
    }

    let mut new_src = lines.join("\n");
    if trailing_newline {
        new_src.push('\n');
    }
    (new_src, added)
}

/// Idempotently insert `.schedule(<crate>::schedule::register)` into the
/// application builder chain in `main.rs`. Returns `Ok(true)` when wired,
/// `Ok(false)` when a `.schedule(` call is already present.
fn ensure_application_schedule(main_path: &Path) -> Result<bool, String> {
    let content =
        fs::read_to_string(main_path).map_err(|e| format!("Failed to read main.rs: {}", e))?;
    let crate_name = detect_lib_crate(&content).ok_or_else(|| {
        "could not determine the application crate name (no [package].name in Cargo.toml \
         and no recognizable `use <crate>::{...}` line in main.rs)"
            .to_string()
    })?;
    match insert_schedule_call(&content, &crate_name)? {
        Some(new_content) => {
            fs::write(main_path, &new_content)
                .map_err(|e| format!("Failed to write main.rs: {}", e))?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Pure core of [`ensure_application_schedule`]: insert the `.schedule(...)` call
/// immediately before the `.run()` line, matching its indentation. `Ok(Some)`
/// with the rewritten source when inserted, `Ok(None)` when a `.schedule(` call
/// already exists, `Err` when there is no `.run()` line to anchor against.
fn insert_schedule_call(main_src: &str, crate_name: &str) -> Result<Option<String>, String> {
    if main_src.contains(".schedule(") {
        return Ok(None);
    }
    let trailing_newline = main_src.ends_with('\n');
    let lines: Vec<&str> = main_src.lines().collect();
    let run_idx = lines
        .iter()
        .position(|l| l.trim_start().starts_with(".run()"))
        .ok_or_else(|| "no `.run()` call found in main.rs".to_string())?;
    let indent: String = lines[run_idx]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();
    let insertion = format!("{}.schedule({}::schedule::register)", indent, crate_name);

    let mut new_lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    new_lines.insert(run_idx, insertion);
    let mut new_src = new_lines.join("\n");
    if trailing_newline {
        new_src.push('\n');
    }
    Ok(Some(new_src))
}

/// Determine the library crate name `main.rs` refers to, so the wired call can be
/// fully qualified (`<crate>::schedule::register`) from the separate binary crate.
/// Prefers the unambiguous `[package].name` in Cargo.toml (hyphens become
/// underscores); falls back to parsing `main.rs`'s own `use <crate>::{...}` line.
fn detect_lib_crate(main_src: &str) -> Option<String> {
    crate_name_from_cargo().or_else(|| crate_name_from_use_line(main_src))
}

/// Read `[package].name` from the current directory's Cargo.toml, normalizing
/// hyphens to underscores to match the Rust crate identifier.
fn crate_name_from_cargo() -> Option<String> {
    let name = crate::commands::cargo_meta::package_name_from_path(Path::new("Cargo.toml"))?;
    Some(name.replace('-', "_"))
}

/// Extract the crate name from `main.rs`'s import of the app's own modules,
/// e.g. `use my_app::{bootstrap, config, migrations, routes};` → `my_app`. Only
/// matches a `use <crate>::{...}` line that pulls in the scaffold's own modules,
/// not a third-party dependency import.
fn crate_name_from_use_line(main_src: &str) -> Option<String> {
    for line in main_src.lines() {
        let Some(rest) = line.trim().strip_prefix("use ") else {
            continue;
        };
        let Some((crate_name, items)) = rest.split_once("::{") else {
            continue;
        };
        if items.contains("routes") || items.contains("bootstrap") {
            let name = crate_name.trim();
            if is_valid_identifier(name) {
                return Some(name.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN_RS: &str = r#"//! entry
use suprnova::Application;

use my_app::{bootstrap, config, migrations, routes};

#[tokio::main]
async fn main() {
    Application::new()
        .config(config::register_all)
        .bootstrap(bootstrap::register)
        .routes(routes::register)
        .migrations::<migrations::Migrator>()
        .run()
        .await;
}
"#;

    #[test]
    fn declares_missing_modules_after_last_mod() {
        let lib = "pub mod controllers;\npub mod routes;\n";
        let (out, added) = declare_lib_modules(lib, &["schedule", "tasks"]);
        assert_eq!(added, vec!["schedule".to_string(), "tasks".to_string()]);
        assert!(out.contains("pub mod schedule;"));
        assert!(out.contains("pub mod tasks;"));
        // existing declarations are preserved and the trailing newline is kept
        assert!(out.starts_with("pub mod controllers;\npub mod routes;\n"));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn declare_lib_modules_is_idempotent() {
        let lib = "pub mod schedule;\npub mod tasks;\n";
        let (out, added) = declare_lib_modules(lib, &["schedule", "tasks"]);
        assert!(added.is_empty());
        assert_eq!(out, lib);
        // a bare `mod` form also counts as already declared
        let (_, added) = declare_lib_modules("mod schedule;\n", &["schedule"]);
        assert!(added.is_empty());
    }

    #[test]
    fn inserts_schedule_before_run_with_matching_indent() {
        let out = insert_schedule_call(MAIN_RS, "my_app").unwrap().unwrap();
        assert!(out.contains("        .schedule(my_app::schedule::register)\n"));
        let sched = out.find(".schedule(").unwrap();
        let run = out.find(".run()").unwrap();
        assert!(sched < run, "schedule must be chained before run");
    }

    #[test]
    fn insert_schedule_call_is_idempotent() {
        let already = MAIN_RS.replace(
            "        .run()",
            "        .schedule(my_app::schedule::register)\n        .run()",
        );
        assert!(insert_schedule_call(&already, "my_app").unwrap().is_none());
    }

    #[test]
    fn insert_schedule_call_errors_without_run() {
        let no_run = "fn main() {\n    Application::new();\n}\n";
        assert!(insert_schedule_call(no_run, "my_app").is_err());
    }

    #[test]
    fn detects_crate_from_use_line_and_ignores_deps() {
        assert_eq!(crate_name_from_use_line(MAIN_RS).as_deref(), Some("my_app"));
        let only_dep = "use serde::{Serialize, Deserialize};\nfn main() {}\n";
        assert_eq!(crate_name_from_use_line(only_dep), None);
    }
}
