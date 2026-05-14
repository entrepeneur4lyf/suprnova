use console::style;
use dialoguer::{theme::ColorfulTheme, Input, Select};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::str::FromStr;

use crate::templates::{self, Frontend};
use crate::ui;

pub fn run(
    name: Option<String>,
    no_interaction: bool,
    no_git: bool,
    frontend: Option<String>,
) {
    ui::banner();

    let project_name = get_project_name(name, no_interaction);
    let description = get_description(no_interaction);
    let author = get_author(no_interaction);
    let frontend = get_frontend(frontend, no_interaction);

    let package_name = to_snake_case(&project_name);

    ui::br();
    ui::info(&format!(
        "Creating {} with {} frontend...",
        style(&project_name).bold(),
        style(frontend.as_str()).cyan(),
    ));
    ui::br();

    if let Err(e) = create_project(
        &project_name,
        &package_name,
        &description,
        &author,
        no_git,
        frontend,
    ) {
        ui::error(&format!("{}", e));
        std::process::exit(1);
    }

    ui::success("Generated project structure");

    if !no_git {
        ui::success("Initialized git repository");
    }

    ui::success("Ready to go!");

    ui::br();
    ui::panel("Next Steps", &[
        &format!("cd {}", project_name),
        "suprnova serve",
    ]);
    ui::br();
    ui::label_value("Backend", &format!("http://localhost:8000"));
    ui::label_value("Frontend", &format!("http://localhost:5173"));
    ui::br();
}

fn get_project_name(name: Option<String>, no_interaction: bool) -> String {
    if let Some(n) = name {
        return n;
    }

    if no_interaction {
        return "my-suprnova-app".to_string();
    }

    Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Project name")
        .default("my-suprnova-app".to_string())
        .interact_text()
        .unwrap()
}

fn get_description(no_interaction: bool) -> String {
    if no_interaction {
        return "A web application built with Suprnova".to_string();
    }

    Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Description")
        .default("A web application built with Suprnova".to_string())
        .interact_text()
        .unwrap()
}

fn get_frontend(choice: Option<String>, no_interaction: bool) -> Frontend {
    if let Some(s) = choice {
        match Frontend::from_str(&s) {
            Ok(fe) => return fe,
            Err(e) => {
                eprintln!("{} {}", style("Warning:").yellow().bold(), e);
                eprintln!("Falling back to default (svelte).");
            }
        }
    }

    if no_interaction {
        return Frontend::Svelte;
    }

    let options = ["Svelte (recommended)", "React", "Vue"];
    let idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Frontend framework")
        .items(&options)
        .default(0)
        .interact()
        .unwrap();
    match idx {
        0 => Frontend::Svelte,
        1 => Frontend::React,
        _ => Frontend::Vue,
    }
}

fn get_author(no_interaction: bool) -> String {
    if no_interaction {
        return String::new();
    }

    let default_author = get_git_author().unwrap_or_default();

    Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Author")
        .default(default_author)
        .allow_empty(true)
        .interact_text()
        .unwrap()
}

fn get_git_author() -> Option<String> {
    let name = Command::new("git")
        .args(["config", "user.name"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    let email = Command::new("git")
        .args(["config", "user.email"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    Some(format!("{} <{}>", name, email))
}

fn to_snake_case(s: &str) -> String {
    s.replace('-', "_").to_lowercase()
}

fn to_title_case(s: &str) -> String {
    s.replace('-', " ")
        .replace('_', " ")
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn create_project(
    project_name: &str,
    package_name: &str,
    description: &str,
    author: &str,
    no_git: bool,
    frontend: Frontend,
) -> Result<(), String> {
    let project_path = Path::new(project_name);

    if project_path.exists() {
        return Err(format!("Directory '{}' already exists", project_name));
    }

    // Create directory structure
    // Backend directories
    fs::create_dir_all(project_path.join("cmd"))
        .map_err(|e| format!("Failed to create directories: {}", e))?;
    fs::create_dir_all(project_path.join("src/controllers"))
        .map_err(|e| format!("Failed to create directories: {}", e))?;
    fs::create_dir_all(project_path.join("src/config"))
        .map_err(|e| format!("Failed to create directories: {}", e))?;
    fs::create_dir_all(project_path.join("src/middleware"))
        .map_err(|e| format!("Failed to create directories: {}", e))?;
    fs::create_dir_all(project_path.join("src/actions"))
        .map_err(|e| format!("Failed to create directories: {}", e))?;
    fs::create_dir_all(project_path.join("src/models"))
        .map_err(|e| format!("Failed to create directories: {}", e))?;
    fs::create_dir_all(project_path.join("src/migrations"))
        .map_err(|e| format!("Failed to create directories: {}", e))?;

    // Frontend directories + files are created by templates::scaffold_frontend below.

    // Public assets directory (for production builds)
    fs::create_dir_all(project_path.join("public/assets"))
        .map_err(|e| format!("Failed to create directories: {}", e))?;

    // === Backend files ===

    // Write Cargo.toml
    let cargo_toml = templates::cargo_toml(package_name, description, author);
    fs::write(project_path.join("Cargo.toml"), cargo_toml)
        .map_err(|e| format!("Failed to write Cargo.toml: {}", e))?;

    // Write .gitignore
    fs::write(project_path.join(".gitignore"), templates::gitignore())
        .map_err(|e| format!("Failed to write .gitignore: {}", e))?;

    // Write .env
    fs::write(project_path.join(".env"), templates::env(project_name))
        .map_err(|e| format!("Failed to write .env: {}", e))?;

    // Write .env.example
    fs::write(project_path.join(".env.example"), templates::env_example())
        .map_err(|e| format!("Failed to write .env.example: {}", e))?;

    // Write cmd/main.rs
    fs::write(
        project_path.join("cmd/main.rs"),
        templates::cmd_main_rs(package_name),
    )
    .map_err(|e| format!("Failed to write cmd/main.rs: {}", e))?;

    // Write src/lib.rs
    fs::write(project_path.join("src/lib.rs"), templates::lib_rs())
        .map_err(|e| format!("Failed to write src/lib.rs: {}", e))?;

    // Write src/routes.rs
    fs::write(project_path.join("src/routes.rs"), templates::routes_rs())
        .map_err(|e| format!("Failed to write src/routes.rs: {}", e))?;

    // Write src/controllers/mod.rs
    fs::write(
        project_path.join("src/controllers/mod.rs"),
        templates::controllers_mod(),
    )
    .map_err(|e| format!("Failed to write src/controllers/mod.rs: {}", e))?;

    // Write src/controllers/home.rs
    fs::write(
        project_path.join("src/controllers/home.rs"),
        templates::home_controller(),
    )
    .map_err(|e| format!("Failed to write src/controllers/home.rs: {}", e))?;

    // Write src/controllers/auth.rs
    fs::write(
        project_path.join("src/controllers/auth.rs"),
        templates::auth_controller(),
    )
    .map_err(|e| format!("Failed to write src/controllers/auth.rs: {}", e))?;

    // Write src/controllers/dashboard.rs
    fs::write(
        project_path.join("src/controllers/dashboard.rs"),
        templates::dashboard_controller(),
    )
    .map_err(|e| format!("Failed to write src/controllers/dashboard.rs: {}", e))?;

    // Write src/config/mod.rs
    fs::write(
        project_path.join("src/config/mod.rs"),
        templates::config_mod(),
    )
    .map_err(|e| format!("Failed to write src/config/mod.rs: {}", e))?;

    // Write src/config/database.rs
    fs::write(
        project_path.join("src/config/database.rs"),
        templates::config_database(),
    )
    .map_err(|e| format!("Failed to write src/config/database.rs: {}", e))?;

    // Write src/config/mail.rs
    fs::write(
        project_path.join("src/config/mail.rs"),
        templates::config_mail(),
    )
    .map_err(|e| format!("Failed to write src/config/mail.rs: {}", e))?;

    // Write src/middleware/mod.rs
    fs::write(
        project_path.join("src/middleware/mod.rs"),
        templates::middleware_mod(),
    )
    .map_err(|e| format!("Failed to write src/middleware/mod.rs: {}", e))?;

    // Write src/middleware/logging.rs
    fs::write(
        project_path.join("src/middleware/logging.rs"),
        templates::middleware_logging(),
    )
    .map_err(|e| format!("Failed to write src/middleware/logging.rs: {}", e))?;

    // Write src/middleware/authenticate.rs
    fs::write(
        project_path.join("src/middleware/authenticate.rs"),
        templates::authenticate_middleware(),
    )
    .map_err(|e| format!("Failed to write src/middleware/authenticate.rs: {}", e))?;

    // Write src/bootstrap.rs
    fs::write(
        project_path.join("src/bootstrap.rs"),
        templates::bootstrap(),
    )
    .map_err(|e| format!("Failed to write src/bootstrap.rs: {}", e))?;

    // Write src/actions/mod.rs
    fs::write(
        project_path.join("src/actions/mod.rs"),
        templates::actions_mod(),
    )
    .map_err(|e| format!("Failed to write src/actions/mod.rs: {}", e))?;

    // Write src/actions/example_action.rs
    fs::write(
        project_path.join("src/actions/example_action.rs"),
        templates::example_action(),
    )
    .map_err(|e| format!("Failed to write src/actions/example_action.rs: {}", e))?;

    // Write src/models/mod.rs
    fs::write(
        project_path.join("src/models/mod.rs"),
        templates::models_mod(),
    )
    .map_err(|e| format!("Failed to write src/models/mod.rs: {}", e))?;

    // Write src/models/user.rs
    fs::write(
        project_path.join("src/models/user.rs"),
        templates::user_model(),
    )
    .map_err(|e| format!("Failed to write src/models/user.rs: {}", e))?;

    // Write src/migrations/mod.rs
    fs::write(
        project_path.join("src/migrations/mod.rs"),
        templates::migrations_mod(),
    )
    .map_err(|e| format!("Failed to write src/migrations/mod.rs: {}", e))?;

    // Write auth migration files
    fs::write(
        project_path.join("src/migrations/m20240101_000001_create_users_table.rs"),
        templates::create_users_migration(),
    )
    .map_err(|e| format!("Failed to write create_users_table migration: {}", e))?;

    fs::write(
        project_path.join("src/migrations/m20240101_000002_create_sessions_table.rs"),
        templates::create_sessions_migration(),
    )
    .map_err(|e| format!("Failed to write create_sessions_table migration: {}", e))?;

    // Note: migrations are now integrated into the main binary
    // Run with: ./app migrate

    // === Frontend files ===
    let title = to_title_case(project_name);
    templates::scaffold_frontend(project_path, project_name, &title, frontend)?;

    // Initialize git repository
    if !no_git {
        Command::new("git")
            .args(["init"])
            .current_dir(project_path)
            .output()
            .map_err(|e| format!("Failed to initialize git repository: {}", e))?;
    }

    Ok(())
}
