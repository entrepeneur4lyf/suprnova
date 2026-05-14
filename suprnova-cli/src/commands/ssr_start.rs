//! `suprnova ssr:start` — launch the Inertia SSR worker.
//!
//! Foreground process. Picks up `SUPRNOVA_SSR_BUNDLE` and
//! `SUPRNOVA_SSR_RUNTIME` from the environment (or `--bundle` /
//! `--runtime` flags). Forwards stdout/stderr verbatim so the user
//! sees the SSR worker's logs. User stops with Ctrl-C; runs under
//! systemd/pm2/supervisord in production.
//!
//! Daemonization, PID files, log rotation, restart-on-crash, and
//! `:stop`/`:check` subcommands are deliberately *not* in the
//! framework — those are operator-stack concerns.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Runtime to launch the SSR worker under. Defaults to `node` when
/// neither `--runtime` nor `SUPRNOVA_SSR_RUNTIME` are set.
fn resolve_runtime(flag: Option<String>) -> String {
    flag.or_else(|| std::env::var("SUPRNOVA_SSR_RUNTIME").ok())
        .unwrap_or_else(|| "node".to_string())
}

/// Path to the built SSR bundle. Looks at (in order):
/// 1. `--bundle` flag
/// 2. `SUPRNOVA_SSR_BUNDLE` env var
/// 3. `frontend/bootstrap/ssr/ssr.js` (Vite default for the
///    `@inertiajs/{...}/server` bundle)
fn resolve_bundle(flag: Option<String>) -> PathBuf {
    if let Some(p) = flag {
        return PathBuf::from(p);
    }
    if let Ok(p) = std::env::var("SUPRNOVA_SSR_BUNDLE") {
        return PathBuf::from(p);
    }
    PathBuf::from("frontend/bootstrap/ssr/ssr.js")
}

pub fn run(runtime: Option<String>, bundle: Option<String>) {
    let runtime = resolve_runtime(runtime);
    let bundle_path = resolve_bundle(bundle);

    if !bundle_path.exists() {
        eprintln!(
            "Error: SSR bundle not found at '{}'.",
            bundle_path.display()
        );
        eprintln!();
        eprintln!("Build the SSR bundle first. With Vite + @inertiajs/{{vue3,react,svelte}}:");
        eprintln!("  vite build --ssr");
        eprintln!();
        eprintln!(
            "Then run `suprnova ssr:start` again, or pass --bundle <path> / set SUPRNOVA_SSR_BUNDLE."
        );
        std::process::exit(1);
    }

    println!(
        "Starting Inertia SSR worker: {} {}",
        runtime,
        bundle_path.display()
    );
    println!("(stop with Ctrl-C)");
    println!();

    // Foreground process. stdout/stderr inherited so the worker's logs
    // show up in the operator's terminal. Exit code propagates so
    // supervisors see the right signal.
    let mut child = match Command::new(&runtime)
        .arg(&bundle_path)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to start SSR worker: {e}");
            std::process::exit(1);
        }
    };

    match child.wait() {
        Ok(status) => {
            std::process::exit(status.code().unwrap_or(1));
        }
        Err(e) => {
            eprintln!("SSR worker exited abnormally: {e}");
            std::process::exit(1);
        }
    }
}
