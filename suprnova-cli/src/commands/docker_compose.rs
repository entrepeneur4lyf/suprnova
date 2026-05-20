//! docker:compose command - Generate docker-compose.yml for local development

use dialoguer::{Confirm, theme::ColorfulTheme};
use std::fs;
use std::path::Path;
use toml::Value;

use crate::templates;
use crate::ui;

pub fn run(with_mailpit: bool, with_minio: bool) {
    if !Path::new("Cargo.toml").exists() {
        ui::error("Cargo.toml not found");
        ui::hint("Make sure you're in a Suprnova project root directory.");
        std::process::exit(1);
    }

    let project_name = get_project_name();
    let compose_path = Path::new("docker-compose.yml");

    if compose_path.exists() {
        ui::warning("docker-compose.yml already exists");
        ui::hint("Remove or rename the existing docker-compose.yml to generate a new one.");
        std::process::exit(0);
    }

    let (include_mailpit, include_minio) = prompt_for_services(with_mailpit, with_minio);

    let compose_content =
        templates::docker_compose_template(&project_name, include_mailpit, include_minio);
    if let Err(e) = fs::write(compose_path, compose_content) {
        ui::error(&format!("Failed to write docker-compose.yml: {}", e));
        std::process::exit(1);
    }
    ui::success("Created docker-compose.yml");

    update_gitignore();

    print_instructions(&project_name, include_mailpit, include_minio);
}

fn get_project_name() -> String {
    let cargo_toml = match fs::read_to_string("Cargo.toml") {
        Ok(content) => content,
        Err(_) => {
            return std::env::current_dir()
                .ok()
                .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
                .unwrap_or_else(|| "suprnova_app".to_string());
        }
    };

    let parsed: Value = match cargo_toml.parse() {
        Ok(v) => v,
        Err(_) => {
            return std::env::current_dir()
                .ok()
                .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
                .unwrap_or_else(|| "suprnova_app".to_string());
        }
    };

    parsed["package"]["name"]
        .as_str()
        .unwrap_or("suprnova_app")
        .to_string()
}

fn prompt_for_services(with_mailpit: bool, with_minio: bool) -> (bool, bool) {
    if with_mailpit || with_minio {
        return (with_mailpit, with_minio);
    }

    ui::br();
    ui::header("Optional Services");
    ui::hint("MySQL and Redis are included by default.");
    ui::br();

    let include_mailpit = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Include Mailpit (email testing)?")
        .default(false)
        .interact()
        .unwrap_or(false);

    let include_minio = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Include MinIO (S3-compatible storage)?")
        .default(false)
        .interact()
        .unwrap_or(false);

    ui::br();

    (include_mailpit, include_minio)
}

fn update_gitignore() {
    let gitignore_path = Path::new(".gitignore");
    if !gitignore_path.exists() {
        return;
    }

    let content = match fs::read_to_string(gitignore_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    if content.contains("docker-compose.override.yml") {
        return;
    }

    let new_content = format!(
        "{}\n# Local Docker overrides\ndocker-compose.override.yml\n",
        content.trim_end()
    );

    if fs::write(gitignore_path, new_content).is_ok() {
        ui::success("Updated .gitignore");
    }
}

fn print_instructions(_project_name: &str, has_mailpit: bool, has_minio: bool) {
    ui::br();

    let mut services = vec![
        "PostgreSQL ···· localhost:5432",
        "Redis ········· localhost:6379",
    ];
    if has_mailpit {
        services.push("Mailpit SMTP ·· localhost:1025");
        services.push("Mailpit UI ···· http://localhost:8025");
    }
    if has_minio {
        services.push("MinIO API ····· localhost:9000");
        services.push("MinIO Console · http://localhost:9001");
    }
    ui::panel("Services", &services);

    ui::br();
    ui::hint("Start:");
    ui::command("docker compose up -d");
    ui::br();
    ui::hint("Update your .env:");
    ui::command("DATABASE_URL=postgres://suprnova:suprnova_secret@localhost:5432/suprnova_db");
    ui::br();
}
