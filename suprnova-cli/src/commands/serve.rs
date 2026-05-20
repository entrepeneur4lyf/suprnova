use console::style;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

use crate::ui;

struct ProcessManager {
    children: Vec<Child>,
    shutdown: Arc<AtomicBool>,
}

impl ProcessManager {
    fn new() -> Self {
        Self {
            children: Vec::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    fn spawn_with_prefix(
        &mut self,
        command: &str,
        args: &[&str],
        cwd: Option<&Path>,
        prefix: &str,
        color: console::Color,
    ) -> Result<(), String> {
        let mut cmd = Command::new(command);
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn {}: {}", command, e))?;

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let shutdown_stdout = self.shutdown.clone();
        let shutdown_stderr = self.shutdown.clone();

        let prefix_out = prefix.to_string();
        let prefix_err = prefix.to_string();

        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                if shutdown_stdout.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(line) = line {
                    println!("{} {}", style(&prefix_out).fg(color).bold(), line);
                }
            }
        });

        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                if shutdown_stderr.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(line) = line {
                    eprintln!("{} {}", style(&prefix_err).fg(color).bold(), line);
                }
            }
        });

        self.children.push(child);
        Ok(())
    }

    fn shutdown_all(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn any_exited(&mut self) -> bool {
        for child in &mut self.children {
            if let Ok(Some(_)) = child.try_wait() {
                return true;
            }
        }
        false
    }
}

fn get_package_name() -> Result<String, String> {
    let cargo_toml = Path::new("Cargo.toml");
    let content = std::fs::read_to_string(cargo_toml)
        .map_err(|e| format!("Failed to read Cargo.toml: {}", e))?;

    let parsed: toml::Value = content
        .parse()
        .map_err(|e| format!("Failed to parse Cargo.toml: {}", e))?;

    parsed
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Could not find package name in Cargo.toml".to_string())
}

fn validate_suprnova_project(backend_only: bool, frontend_only: bool) -> Result<(), String> {
    let cargo_toml = Path::new("Cargo.toml");
    let frontend_dir = Path::new("frontend");

    if !frontend_only && !cargo_toml.exists() {
        return Err("No Cargo.toml found. Are you in a Suprnova project directory?".into());
    }

    if !backend_only && !frontend_dir.exists() {
        return Err("No frontend directory found. Are you in a Suprnova project directory?".into());
    }

    Ok(())
}

fn ensure_cargo_watch() -> Result<(), String> {
    let status = Command::new("cargo")
        .args(["watch", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => Ok(()),
        _ => {
            ui::warning("cargo-watch not found. Installing...");
            let install = Command::new("cargo")
                .args(["install", "cargo-watch"])
                .status()
                .map_err(|e| format!("Failed to install cargo-watch: {}", e))?;

            if !install.success() {
                return Err("Failed to install cargo-watch".into());
            }
            ui::success("cargo-watch installed");
            Ok(())
        }
    }
}

fn ensure_npm_dependencies() -> Result<(), String> {
    let frontend_path = Path::new("frontend");
    let node_modules = frontend_path.join("node_modules");

    if !node_modules.exists() {
        ui::info("Installing frontend dependencies...");
        let npm_install = Command::new("npm")
            .args(["install"])
            .current_dir(frontend_path)
            .status()
            .map_err(|e| format!("Failed to run npm install: {}", e))?;

        if !npm_install.success() {
            return Err("Failed to install npm dependencies".into());
        }
        ui::success("Frontend dependencies installed");
    }

    Ok(())
}

pub fn run(
    port: u16,
    frontend_port: u16,
    backend_only: bool,
    frontend_only: bool,
    skip_types: bool,
) {
    // Load .env file from current directory
    let _ = dotenvy::dotenv();

    // Resolve ports: CLI args take precedence, then env vars, then defaults
    let backend_port = if port != 8080 {
        // CLI argument was explicitly provided (different from default)
        port
    } else {
        // Use env var or default
        std::env::var("SERVER_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(port)
    };

    let vite_port = if frontend_port != 5173 {
        // CLI argument was explicitly provided
        frontend_port
    } else {
        // Use env var or default
        std::env::var("VITE_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(frontend_port)
    };

    ui::banner();
    ui::info("Starting development servers...");
    ui::br();

    // Validate project
    if let Err(e) = validate_suprnova_project(backend_only, frontend_only) {
        ui::error(&e);
        std::process::exit(1);
    }

    // Generate TypeScript types on startup (unless skipped or frontend-only)
    if !skip_types && !frontend_only {
        let project_path = Path::new(".");
        let output_path = project_path.join("frontend/src/types/inertia-props.ts");

        ui::info("Generating TypeScript types...");
        match super::generate_types::generate_types_to_file(project_path, &output_path) {
            Ok(0) => {
                ui::hint("No InertiaProps structs found (skipping type generation)");
            }
            Ok(count) => {
                ui::success(&format!(
                    "Generated {} type(s) → {}",
                    count,
                    output_path.display()
                ));
            }
            Err(e) => {
                ui::warning(&format!("Failed to generate types: {} (continuing)", e));
            }
        }
        ui::br();
    }

    // Ensure cargo-watch is installed (only if running backend)
    if !frontend_only && let Err(e) = ensure_cargo_watch() {
        ui::error(&e);
        std::process::exit(1);
    }

    // Ensure npm dependencies are installed (only if running frontend)
    if !backend_only && let Err(e) = ensure_npm_dependencies() {
        ui::error(&e);
        std::process::exit(1);
    }

    let mut manager = ProcessManager::new();
    let shutdown = manager.shutdown.clone();

    // Set up Ctrl+C handler
    ctrlc::set_handler(move || {
        println!();
        ui::info("Shutting down servers...");
        shutdown.store(true, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");

    // Start backend with cargo-watch
    if !frontend_only {
        let package_name = match get_package_name() {
            Ok(name) => name,
            Err(e) => {
                ui::error(&e);
                std::process::exit(1);
            }
        };

        ui::label_value("Backend", &format!("http://127.0.0.1:{}", backend_port));

        let run_cmd = format!("run --bin {}", package_name);
        if let Err(e) = manager.spawn_with_prefix(
            "cargo",
            &["watch", "-x", &run_cmd],
            None,
            "[backend] ",
            console::Color::Magenta,
        ) {
            ui::error(&e);
            std::process::exit(1);
        }
    }

    // Start frontend with npm/vite
    if !backend_only {
        ui::label_value("Frontend", &format!("http://127.0.0.1:{}", vite_port));

        let frontend_path = Path::new("frontend");

        if let Err(e) = manager.spawn_with_prefix(
            "npm",
            &["run", "dev"],
            Some(frontend_path),
            "[frontend]",
            console::Color::Cyan,
        ) {
            ui::error(&e);
            manager.shutdown_all();
            std::process::exit(1);
        }
    }

    // Start file watcher for TypeScript type regeneration
    if !skip_types && !frontend_only {
        let shutdown_watcher = manager.shutdown.clone();
        thread::spawn(move || {
            start_type_watcher(shutdown_watcher);
        });
    }

    ui::br();
    ui::hint("Press Ctrl+C to stop all servers");
    ui::br();

    // Wait for shutdown signal or process exit
    while !manager.shutdown.load(Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(100));

        // Check if any child process has exited
        if manager.any_exited() {
            manager.shutdown.store(true, Ordering::SeqCst);
            break;
        }
    }

    manager.shutdown_all();
    ui::success("Servers stopped.");
}

/// File watcher that regenerates TypeScript types when Rust files change
fn start_type_watcher(shutdown: Arc<AtomicBool>) {
    let (tx, rx) = channel();
    let src_path = Path::new("src");

    let watcher_result = RecommendedWatcher::new(
        move |res| {
            if let Ok(event) = res {
                let _ = tx.send(event);
            }
        },
        Config::default().with_poll_interval(Duration::from_secs(2)),
    );

    let mut watcher = match watcher_result {
        Ok(w) => w,
        Err(e) => {
            eprintln!(
                "{} Failed to start type watcher: {}",
                style("[types]").yellow(),
                e
            );
            return;
        }
    };

    if let Err(e) = watcher.watch(src_path, RecursiveMode::Recursive) {
        eprintln!(
            "{} Failed to watch src directory: {}",
            style("[types]").yellow(),
            e
        );
        return;
    }

    println!(
        "{} Watching for Rust file changes to regenerate types",
        style("[types]").blue()
    );

    let project_path = Path::new(".");
    let output_path = project_path.join("frontend/src/types/inertia-props.ts");

    // Debounce timer to avoid regenerating too frequently
    let mut last_regen = std::time::Instant::now();
    let debounce_duration = Duration::from_millis(500);

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        // Use recv_timeout to periodically check shutdown
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                // Check if it's a Rust file change
                let is_rust_change = event
                    .paths
                    .iter()
                    .any(|p| p.extension().map(|e| e == "rs").unwrap_or(false));

                if is_rust_change && last_regen.elapsed() > debounce_duration {
                    last_regen = std::time::Instant::now();

                    match super::generate_types::generate_types_to_file(project_path, &output_path)
                    {
                        Ok(count) if count > 0 => {
                            println!("{} Regenerated {} type(s)", style("[types]").blue(), count);
                        }
                        Ok(_) => {} // No types found, stay quiet
                        Err(e) => {
                            eprintln!("{} Failed to regenerate: {}", style("[types]").yellow(), e);
                        }
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}
