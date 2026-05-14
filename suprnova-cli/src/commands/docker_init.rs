//! docker:init command - Generate production-ready Dockerfile

use std::fs;
use std::path::Path;
use toml::Value;

use crate::templates;
use crate::ui;

pub fn run() {
    if !Path::new("Cargo.toml").exists() {
        ui::error("Cargo.toml not found");
        ui::hint("Make sure you're in a Suprnova project root directory.");
        std::process::exit(1);
    }

    let package_name = get_package_name();

    let dockerfile_path = Path::new("Dockerfile");
    let dockerignore_path = Path::new(".dockerignore");

    if dockerfile_path.exists() {
        ui::warning("Dockerfile already exists");
        ui::hint("Remove or rename the existing Dockerfile to generate a new one.");
        std::process::exit(0);
    }

    let dockerfile_content = templates::dockerfile_template(&package_name);
    if let Err(e) = fs::write(dockerfile_path, dockerfile_content) {
        ui::error(&format!("Failed to write Dockerfile: {}", e));
        std::process::exit(1);
    }
    ui::success("Created Dockerfile");

    if !dockerignore_path.exists() {
        let dockerignore_content = templates::dockerignore_template();
        if let Err(e) = fs::write(dockerignore_path, dockerignore_content) {
            ui::error(&format!("Failed to write .dockerignore: {}", e));
            std::process::exit(1);
        }
        ui::success("Created .dockerignore");
    }

    ui::br();
    ui::panel("Docker", &[
        &format!("docker build -t {} .", package_name),
        &format!("docker run -p 8080:8080 --env-file .env.production {}", package_name),
    ]);
    ui::br();
    ui::hint("Create a .env.production file with your production environment variables.");
    ui::br();
}

fn get_package_name() -> String {
    let cargo_toml = match fs::read_to_string("Cargo.toml") {
        Ok(content) => content,
        Err(_) => return "app".to_string(),
    };

    let parsed: Value = match cargo_toml.parse() {
        Ok(v) => v,
        Err(_) => return "app".to_string(),
    };

    parsed["package"]["name"]
        .as_str()
        .unwrap_or("app")
        .to_string()
}
