use console::style;
use dialoguer::{Input, Select, theme::ColorfulTheme};
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
    api: bool,
    with_portless: bool,
) {
    ui::banner();

    let project_name = get_project_name(name, no_interaction);
    if let Err(e) = validate_project_name(&project_name) {
        ui::error(&e);
        std::process::exit(1);
    }
    let package_name = to_snake_case(&project_name);

    if api {
        ui::br();
        ui::info(&format!(
            "Creating {} as a JSON:API-only project...",
            style(&project_name).bold(),
        ));
        ui::br();

        if let Err(e) = create_api_project(&project_name, &package_name, no_git, with_portless) {
            ui::error(&e);
            std::process::exit(1);
        }

        ui::success("Generated API project structure");

        if !no_git {
            ui::success("Initialized git repository");
        }

        ui::success("Ready to go!");

        ui::br();
        ui::panel(
            "Next Steps",
            &[
                &format!("cd {}", project_name),
                "suprnova migrate",
                "suprnova serve",
            ],
        );
        ui::br();
        ui::label_value("API", "http://localhost:8765/api");
        ui::br();
        if with_portless {
            ui::header("portless — HTTPS dev URL");
            ui::hint("Wrote portless.json. For a named https://<name>.localhost URL:");
            ui::command("suprnova dev:tls   # one-time: trust the CA + register the route");
            ui::command("suprnova serve");
            ui::br();
        }
        return;
    }

    let description = get_description(no_interaction);
    let author = get_author(no_interaction);
    let frontend = get_frontend(frontend, no_interaction);

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
        with_portless,
    ) {
        ui::error(&e.to_string());
        std::process::exit(1);
    }

    ui::success("Generated project structure");

    if !no_git {
        ui::success("Initialized git repository");
    }

    ui::success("Ready to go!");

    ui::br();
    ui::panel(
        "Next Steps",
        &[&format!("cd {}", project_name), "suprnova serve"],
    );
    ui::br();
    ui::label_value("Backend", "http://localhost:8765");
    ui::label_value("Frontend", "http://localhost:5765");
    ui::br();
    if with_portless {
        ui::header("portless — HTTPS dev URL");
        ui::hint("Wrote portless.json. For a named https://<name>.localhost URL:");
        ui::command("suprnova dev:tls   # one-time: trust the CA + register the route");
        ui::command("suprnova serve");
        ui::br();
    }
}

fn get_project_name(name: Option<String>, no_interaction: bool) -> String {
    if let Some(n) = name {
        return n;
    }

    if no_interaction {
        return "my-suprnova-app".to_string();
    }

    match Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Project name")
        .default("my-suprnova-app".to_string())
        .interact_text()
    {
        Ok(name) => name,
        Err(e) => {
            ui::error(&format!(
                "Failed to read the project name: {e}. \
                 Pass a name argument or use --no-interaction in non-interactive shells."
            ));
            std::process::exit(1);
        }
    }
}

fn get_description(no_interaction: bool) -> String {
    if no_interaction {
        return "A web application built with Suprnova".to_string();
    }

    match Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Description")
        .default("A web application built with Suprnova".to_string())
        .interact_text()
    {
        Ok(description) => description,
        Err(e) => {
            ui::error(&format!(
                "Failed to read the description: {e}. \
                 Use --no-interaction in non-interactive shells."
            ));
            std::process::exit(1);
        }
    }
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
    let idx = match Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Frontend framework")
        .items(options)
        .default(0)
        .interact()
    {
        Ok(idx) => idx,
        Err(e) => {
            ui::error(&format!(
                "Failed to read the frontend selection: {e}. \
                 Pass --frontend <svelte|react|vue> or --no-interaction in \
                 non-interactive shells."
            ));
            std::process::exit(1);
        }
    };
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

    match Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Author")
        .default(default_author)
        .allow_empty(true)
        .interact_text()
    {
        Ok(author) => author,
        Err(e) => {
            ui::error(&format!(
                "Failed to read the author: {e}. \
                 Use --no-interaction in non-interactive shells."
            ));
            std::process::exit(1);
        }
    }
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

/// The default backend port written into scaffolded `.env` files
/// (`SERVER_PORT=8765`). `portless.json`'s `appPort` must match it so
/// `portless run` routes the named URL to the app's fixed port.
const SCAFFOLD_SERVER_PORT: u16 = 8765;

/// Write a `portless.json` at the project root. `name` is the portless
/// route label — we use the snake_case package name so it matches
/// `suprnova dev:tls`'s default (which reads `[package].name`), giving
/// one stable URL `https://<package_name>.localhost`.
fn write_portless_json(project_path: &Path, name: &str, app_port: u16) -> Result<(), String> {
    let body = format!("{{\n  \"name\": \"{name}\",\n  \"appPort\": {app_port}\n}}\n");
    fs::write(project_path.join("portless.json"), body)
        .map_err(|e| format!("Failed to write portless.json: {e}"))
}

/// Validate a project name supplied to `suprnova new`.
///
/// Domain 22 audit D22-A: `get_project_name` previously returned the
/// user's raw input unmodified. The value was then used as a path
/// (`Path::new(project_name)`) under `fs::create_dir_all`. A name
/// containing `..` or absolute-path components would create
/// directories outside the working directory. The snake-cased form
/// is also written into `Cargo.toml` as the crate name, so it has to
/// satisfy crate-name rules too.
///
/// Rejected:
/// - empty
/// - longer than 64 characters
/// - contains a path separator (`/`, `\`) or `..`
/// - leading dot (hidden directory)
/// - first character is not an ASCII letter (Cargo crate names must
///   start with a letter; otherwise downstream `cargo new` would
///   reject the manifest anyway)
/// - contains a character that is not ASCII alphanumeric, `-`, or `_`
fn validate_project_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Project name cannot be empty".to_string());
    }
    if name.len() > 64 {
        return Err(format!(
            "Project name '{name}' is too long (max 64 characters)"
        ));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(format!(
            "Project name '{name}' must not contain path separators (/ or \\)"
        ));
    }
    if name.contains("..") {
        return Err(format!(
            "Project name '{name}' must not contain '..' (path traversal)"
        ));
    }
    if name.starts_with('.') {
        return Err(format!(
            "Project name '{name}' must not start with '.' (hidden directory)"
        ));
    }
    let first = name.chars().next().unwrap_or(' ');
    if !first.is_ascii_alphabetic() {
        return Err(format!(
            "Project name '{name}' must start with an ASCII letter"
        ));
    }
    for c in name.chars() {
        if !c.is_ascii_alphanumeric() && c != '-' && c != '_' {
            return Err(format!(
                "Project name '{name}' contains invalid character '{c}'; \
                 use ASCII letters, digits, '-', or '_'"
            ));
        }
    }
    Ok(())
}

fn to_title_case(s: &str) -> String {
    s.replace(['-', '_'], " ")
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

fn create_api_project(
    project_name: &str,
    package_name: &str,
    no_git: bool,
    with_portless: bool,
) -> Result<(), String> {
    let project_path = Path::new(project_name);

    if project_path.exists() {
        return Err(format!("Directory '{}' already exists", project_name));
    }

    fs::create_dir_all(project_path)
        .map_err(|e| format!("Failed to create project directory: {}", e))?;

    templates::scaffold_api(project_path, project_name, package_name)?;

    if with_portless {
        write_portless_json(project_path, package_name, SCAFFOLD_SERVER_PORT)?;
    }

    if !no_git {
        Command::new("git")
            .args(["init"])
            .current_dir(project_path)
            .output()
            .map_err(|e| format!("Failed to initialize git repository: {}", e))?;
    }

    Ok(())
}

fn create_project(
    project_name: &str,
    package_name: &str,
    description: &str,
    author: &str,
    no_git: bool,
    frontend: Frontend,
    with_portless: bool,
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
    fs::create_dir_all(project_path.join("src/bin"))
        .map_err(|e| format!("Failed to create directories: {}", e))?;
    fs::create_dir_all(project_path.join("src/commands"))
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

    // Write .env (with a freshly-generated APP_KEY so the scaffolded
    // project boots out-of-the-box without operator intervention).
    let app_key = crate::commands::key_generate::generate_app_key();
    fs::write(
        project_path.join(".env"),
        templates::env(project_name, &app_key),
    )
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

    // Write src/bin/console.rs — per-project console binary that
    // dispatches argv to `#[command]`-registered handlers.
    fs::write(
        project_path.join("src/bin/console.rs"),
        templates::console_main_rs(package_name),
    )
    .map_err(|e| format!("Failed to write src/bin/console.rs: {}", e))?;

    // Write src/commands/mod.rs — empty stub for user `#[command]`s.
    // `suprnova make:command <name>` appends `pub mod <snake>;` lines.
    fs::write(
        project_path.join("src/commands/mod.rs"),
        templates::commands_mod_rs(),
    )
    .map_err(|e| format!("Failed to write src/commands/mod.rs: {}", e))?;

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

    fs::write(
        project_path.join("src/migrations/m20240101_000003_create_remember_tokens_table.rs"),
        templates::create_remember_tokens_migration(),
    )
    .map_err(|e| {
        format!(
            "Failed to write create_remember_tokens_table migration: {}",
            e
        )
    })?;

    fs::write(
        project_path.join("src/migrations/m20240101_000004_create_auth_flow_tokens_table.rs"),
        templates::create_auth_flow_tokens_migration(),
    )
    .map_err(|e| {
        format!(
            "Failed to write create_auth_flow_tokens_table migration: {}",
            e
        )
    })?;

    // Note: migrations are now integrated into the main binary
    // Run with: ./app migrate

    // === Frontend files ===
    let title = to_title_case(project_name);
    templates::scaffold_frontend(project_path, project_name, &title, frontend)?;

    // portless.json (opt-in via --with-portless) — maps the app's fixed
    // backend port to https://<package_name>.localhost for `portless run`.
    if with_portless {
        write_portless_json(project_path, package_name, SCAFFOLD_SERVER_PORT)?;
    }

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

#[cfg(test)]
mod tests {
    use super::validate_project_name;

    #[test]
    fn accepts_well_formed_names() {
        for name in ["foo", "my-app", "my_app", "foo123", "Foo", "a"] {
            assert!(validate_project_name(name).is_ok(), "rejected: {name}");
        }
    }

    #[test]
    fn rejects_empty() {
        assert!(validate_project_name("").is_err());
    }

    #[test]
    fn rejects_path_separators_and_traversal() {
        for bad in [
            "../etc", "foo/bar", "foo\\bar", "..", "..foo", "foo/..", ".hidden",
        ] {
            assert!(validate_project_name(bad).is_err(), "should reject: {bad}");
        }
    }

    #[test]
    fn rejects_non_letter_first_char() {
        for bad in ["1foo", "_foo", "-foo", "9", "/", "."] {
            assert!(
                validate_project_name(bad).is_err(),
                "should reject leading-non-letter: {bad}"
            );
        }
    }

    #[test]
    fn rejects_disallowed_characters() {
        for bad in [
            "foo bar", "foo!bar", "foo@bar", "foo.bar", "foo:bar", "foo;bar", "foo`bar",
        ] {
            assert!(validate_project_name(bad).is_err(), "should reject: {bad}");
        }
    }

    #[test]
    fn rejects_overlong_names() {
        let long = "a".repeat(65);
        assert!(validate_project_name(&long).is_err());
    }
}
