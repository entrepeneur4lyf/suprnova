//! `suprnova dev:tls` — register a portless HTTPS dev URL and trust
//! portless's local CA in every browser certificate store on the machine.
//!
//! On Linux, browsers read NSS databases (`~/.pki/nssdb`, Flatpak
//! `~/.var/app/<id>/.pki/nssdb`, Firefox profile `cert9.db`), not the
//! system trust store. We install the CA there with `certutil`, which
//! needs no sudo. macOS/Windows delegate to `portless trust`.
//!
//! See `manual/dev-tls.md` for the end-to-end workflow and troubleshooting.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::ui;

/// The CA's nickname in NSS, matching the cert's subject CN.
const CA_NICKNAME: &str = "portless Local CA";

/// Default backend port. Mirrors `serve::DEFAULT_BACKEND_PORT` and the
/// framework's `suprnova::config::providers::server::DEFAULT_SERVER_PORT`;
/// kept in sync deliberately (the CLI can't depend on the framework crate).
const DEFAULT_BACKEND_PORT: u16 = 8765;

/// A browser NSS certificate database to install the CA into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NssDb {
    /// Filesystem path to the NSS database directory.
    pub path: PathBuf,
    /// Whether the caller should `mkdir -p` this directory before
    /// running `certutil`. True for Chromium-family stores (safe to
    /// create ahead of the browser); false for Firefox profiles (we
    /// never fabricate one).
    pub create_if_missing: bool,
}

/// Resolve the app name. `--name` wins; else `[package].name` from the
/// project's `Cargo.toml`; else an error telling the user what to do.
pub fn resolve_name(cli: Option<String>, cargo_name: Option<String>) -> Result<String, String> {
    cli.or(cargo_name).ok_or_else(|| {
        "Could not determine the app name. Pass --name <name>, or run from a \
         Suprnova project root that has a Cargo.toml."
            .to_string()
    })
}

/// Resolve the backend port. `--port` wins; else `SERVER_PORT` (passed in
/// as `env_server_port`); else the 8765 default. No free-port scan —
/// `dev:tls` registers a route, it doesn't bind.
pub fn resolve_port(cli: Option<u16>, env_server_port: Option<u16>) -> u16 {
    cli.or(env_server_port).unwrap_or(DEFAULT_BACKEND_PORT)
}

/// Locate portless's CA. `$PORTLESS_STATE_DIR/ca.pem` when the state dir
/// is set, else `<home>/.portless/ca.pem`.
pub fn ca_path_for(state_dir: Option<&Path>, home: &Path) -> PathBuf {
    match state_dir {
        Some(dir) => dir.join("ca.pem"),
        None => home.join(".portless").join("ca.pem"),
    }
}

/// Discover candidate browser NSS databases under `home`.
///
/// Pure: computes paths and flags only — it creates nothing, so it stays
/// unit-testable against a temporary `$HOME`. The caller performs any
/// `mkdir -p` (guided by `create_if_missing`).
///
/// - `~/.pki/nssdb` (Chrome/Chromium deb/rpm) is **always** included with
///   `create_if_missing = true`, even when absent — a fresh Chrome may not
///   have created it yet, and trusting there pre-creation works.
/// - `~/.var/app/<id>/.pki/nssdb` (Flatpak Chromium-family) is included
///   only when that nssdb directory already exists (we don't fabricate NSS
///   stores for every Flatpak app), with `create_if_missing = true`.
/// - Firefox profiles under `~/.mozilla/firefox/<p>/` and the Flatpak
///   Firefox profile dir are included only when they already contain a
///   `cert9.db`, with `create_if_missing = false`.
pub fn nss_databases(home: &Path) -> Vec<NssDb> {
    let mut dbs = Vec::new();

    // Chrome / Chromium (deb/rpm) — always, create if needed.
    dbs.push(NssDb {
        path: home.join(".pki").join("nssdb"),
        create_if_missing: true,
    });

    // Flatpak Chromium-family: ~/.var/app/<id>/.pki/nssdb (existing only).
    let var_app = home.join(".var").join("app");
    if let Ok(entries) = std::fs::read_dir(&var_app) {
        for entry in entries.flatten() {
            let nssdb = entry.path().join(".pki").join("nssdb");
            if nssdb.is_dir() {
                dbs.push(NssDb {
                    path: nssdb,
                    create_if_missing: true,
                });
            }
        }
    }

    // Firefox profiles (native + Flatpak), existing cert9.db only.
    let firefox_roots = [
        home.join(".mozilla").join("firefox"),
        home.join(".var")
            .join("app")
            .join("org.mozilla.firefox")
            .join(".mozilla")
            .join("firefox"),
    ];
    for root in firefox_roots {
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let profile = entry.path();
                if profile.join("cert9.db").is_file() {
                    dbs.push(NssDb {
                        path: profile,
                        create_if_missing: false,
                    });
                }
            }
        }
    }

    dbs
}

/// Read a `u16` env var, treating empty/unparseable as unset.
fn env_port(key: &str) -> Option<u16> {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok())
}

/// Home directory from `$HOME` (Linux/macOS). On Windows this command's
/// Linux-specific NSS path isn't taken, so `$HOME` being unset there is
/// harmless.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Resolve portless's CA path from the environment.
fn ca_path() -> PathBuf {
    let state_dir = std::env::var_os("PORTLESS_STATE_DIR").map(PathBuf::from);
    let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
    ca_path_for(state_dir.as_deref(), &home)
}

/// Is `bin` on PATH? Probe by spawning it with a harmless arg; only a
/// `NotFound` spawn error counts as "absent" (a non-zero exit still means
/// the binary exists — e.g. `certutil -H` prints help and exits non-zero).
fn on_path(bin: &str, probe_arg: &str) -> bool {
    match Command::new(bin)
        .arg(probe_arg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Register the portless alias: `portless alias <name> <port> --force`.
/// Writes portless's `routes.json` whether or not the proxy is running,
/// so it's safe to run before the daemon starts.
fn register_alias(name: &str, port: u16) -> Result<(), String> {
    let status = Command::new("portless")
        .args(["alias", name, &port.to_string(), "--force"])
        .status()
        .map_err(|e| format!("Failed to run `portless alias`: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "`portless alias {name} {port}` failed (exit {})",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into())
        ))
    }
}

/// Trust the CA. Linux drives browser NSS stores directly; other
/// platforms delegate to `portless trust`.
fn trust_ca() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        trust_ca_linux()
    }
    #[cfg(not(target_os = "linux"))]
    {
        trust_ca_delegate()
    }
}

#[cfg(target_os = "linux")]
fn trust_ca_linux() -> Result<(), String> {
    let ca = ca_path();
    if !ca.is_file() {
        return Err(format!(
            "portless CA not found at {}. Start the proxy once so portless \
             generates its CA (e.g. `systemctl start portless` or `portless \
             proxy start`), then re-run `suprnova dev:tls`.",
            ca.display()
        ));
    }

    if !on_path("certutil", "-H") {
        ui::error("certutil (from libnss3-tools) is required to trust the CA in browsers.");
        ui::hint("  Debian/Ubuntu:  sudo apt install libnss3-tools");
        ui::hint("  Fedora/RHEL:    sudo dnf install nss-tools");
        ui::hint("  Arch:           sudo pacman -S nss");
        return Err("certutil not found".to_string());
    }

    let home =
        home_dir().ok_or_else(|| "Could not determine your home directory ($HOME unset)".to_string())?;
    let dbs = nss_databases(&home);

    let mut trusted = Vec::new();
    for db in &dbs {
        if db.create_if_missing {
            let _ = std::fs::create_dir_all(&db.path);
        }
        if !db.path.is_dir() {
            continue;
        }
        match trust_in_db(&db.path, &ca) {
            Ok(()) => trusted.push(db.path.clone()),
            Err(e) => ui::warning(&format!("Could not trust CA in {}: {e}", db.path.display())),
        }
    }

    if trusted.is_empty() {
        return Err("No browser certificate stores could be updated.".to_string());
    }

    ui::success(&format!("CA trusted in {} store(s):", trusted.len()));
    for p in &trusted {
        ui::hint(&format!("    {}", p.display()));
    }
    Ok(())
}

/// Install the CA into one NSS database, delete-then-add for idempotent
/// re-runs (`-t "C,,"` = trusted CA for issuing SSL server certs).
#[cfg(target_os = "linux")]
fn trust_in_db(db: &Path, ca: &Path) -> Result<(), String> {
    let db_arg = format!("sql:{}", db.display());

    // Delete any prior entry under the same nickname (ignore failure: it
    // may simply not exist yet), then add fresh.
    let _ = Command::new("certutil")
        .args(["-d", &db_arg, "-D", "-n", CA_NICKNAME])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let status = Command::new("certutil")
        .args(["-d", &db_arg, "-A", "-t", "C,,", "-n", CA_NICKNAME, "-i"])
        .arg(ca)
        .status()
        .map_err(|e| format!("certutil failed to spawn: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "certutil -A exited {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into())
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn trust_ca_delegate() -> Result<(), String> {
    ui::info("Delegating CA trust to `portless trust` (native OS cert store)...");
    let status = Command::new("portless")
        .arg("trust")
        .status()
        .map_err(|e| format!("Failed to run `portless trust`: {e}"))?;
    if status.success() {
        ui::success("CA trusted via `portless trust`");
        Ok(())
    } else {
        Err(format!(
            "`portless trust` failed (exit {})",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into())
        ))
    }
}

/// Entry point for `suprnova dev:tls`.
pub fn run(name: Option<String>, port: Option<u16>, no_alias: bool) {
    // Load .env so SERVER_PORT can resolve the route's target port.
    let _ = dotenvy::dotenv();

    ui::banner();
    ui::header("dev:tls — named HTTPS dev URL via portless");

    // 1. Locate portless.
    if !on_path("portless", "--version") {
        ui::error("portless was not found on your PATH.");
        ui::hint("Install it with:  npm install -g portless");
        ui::hint("Docs: https://portless.sh");
        std::process::exit(1);
    }
    ui::success("portless found");

    // 2. Resolve name + port.
    let cargo_name = crate::commands::cargo_meta::package_name_from_path(Path::new("Cargo.toml"));
    let app_name = match resolve_name(name, cargo_name) {
        Ok(n) => n,
        Err(e) => {
            ui::error(&e);
            std::process::exit(1);
        }
    };
    let backend_port = resolve_port(port, env_port("SERVER_PORT"));
    let url = format!("https://{app_name}.localhost");

    // 3. Register the alias (unless --no-alias).
    if no_alias {
        ui::hint("Skipping route registration (--no-alias).");
    } else {
        match register_alias(&app_name, backend_port) {
            Ok(()) => ui::success(&format!(
                "Route registered   {app_name}.localhost → 127.0.0.1:{backend_port}"
            )),
            Err(e) => {
                ui::error(&e);
                std::process::exit(1);
            }
        }
    }

    // 4. Trust the CA (the load-bearing step).
    if let Err(e) = trust_ca() {
        ui::error(&e);
        std::process::exit(1);
    }

    // 5. Next steps — always, in order.
    ui::br();
    ui::info("Next:");
    ui::hint("  1. Fully restart your browser — type chrome://restart (a tab");
    ui::hint("     reload is not enough; the cert store is read once at launch)");
    ui::hint("  2. suprnova serve");
    ui::hint(&format!("  3. open {url}"));
    ui::br();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_name_prefers_cli_over_cargo() {
        assert_eq!(
            resolve_name(Some("flag".into()), Some("cargo".into())).unwrap(),
            "flag"
        );
    }

    #[test]
    fn resolve_name_falls_back_to_cargo() {
        assert_eq!(resolve_name(None, Some("cargo".into())).unwrap(), "cargo");
    }

    #[test]
    fn resolve_name_errors_when_both_absent() {
        let err = resolve_name(None, None).expect_err("no name source must error");
        assert!(err.contains("--name"), "error should mention --name: {err}");
    }

    #[test]
    fn resolve_port_precedence_cli_then_env_then_default() {
        assert_eq!(resolve_port(Some(9000), Some(7000)), 9000);
        assert_eq!(resolve_port(None, Some(7000)), 7000);
        assert_eq!(resolve_port(None, None), DEFAULT_BACKEND_PORT);
    }

    #[test]
    fn ca_path_respects_state_dir_then_home() {
        let state = PathBuf::from("/custom/state");
        let home = PathBuf::from("/home/alice");
        assert_eq!(
            ca_path_for(Some(state.as_path()), &home),
            PathBuf::from("/custom/state/ca.pem")
        );
        assert_eq!(
            ca_path_for(None, &home),
            PathBuf::from("/home/alice/.portless/ca.pem")
        );
    }

    #[test]
    fn nss_databases_always_includes_chrome_store_even_when_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let dbs = nss_databases(home);
        let chrome = home.join(".pki").join("nssdb");
        let found = dbs.iter().find(|d| d.path == chrome).expect("chrome store");
        assert!(found.create_if_missing, "chrome store must be create_if_missing");
    }

    #[test]
    fn nss_databases_includes_existing_flatpak_chromium_excludes_non_browser() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        // A flatpak browser with an nssdb...
        let browser_nssdb = home
            .join(".var/app/io.github.someone.Chromium/.pki/nssdb");
        std::fs::create_dir_all(&browser_nssdb).unwrap();
        // ...and a flatpak app with NO nssdb (must be excluded).
        std::fs::create_dir_all(home.join(".var/app/org.example.NotABrowser")).unwrap();

        let dbs = nss_databases(home);
        assert!(
            dbs.iter().any(|d| d.path == browser_nssdb && d.create_if_missing),
            "flatpak nssdb should be discovered: {dbs:?}"
        );
        assert!(
            !dbs.iter().any(|d| d.path.to_string_lossy().contains("NotABrowser")),
            "flatpak app without nssdb must be excluded: {dbs:?}"
        );
    }

    #[test]
    fn nss_databases_includes_firefox_profile_with_cert9_excludes_without() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let with_db = home.join(".mozilla/firefox/abc.default-release");
        std::fs::create_dir_all(&with_db).unwrap();
        std::fs::write(with_db.join("cert9.db"), b"fake").unwrap();
        let without_db = home.join(".mozilla/firefox/empty.profile");
        std::fs::create_dir_all(&without_db).unwrap();

        let dbs = nss_databases(home);
        let ff = dbs.iter().find(|d| d.path == with_db).expect("firefox profile");
        assert!(!ff.create_if_missing, "firefox profile must NOT be create_if_missing");
        assert!(
            !dbs.iter().any(|d| d.path == without_db),
            "firefox profile lacking cert9.db must be excluded: {dbs:?}"
        );
    }

    #[test]
    fn nss_databases_discovers_flatpak_firefox_profile() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let profile = home.join(".var/app/org.mozilla.firefox/.mozilla/firefox/xyz.default");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("cert9.db"), b"fake").unwrap();

        let dbs = nss_databases(home);
        let ff = dbs
            .iter()
            .find(|d| d.path == profile)
            .expect("flatpak firefox profile must be discovered");
        assert!(
            !ff.create_if_missing,
            "firefox profile must NOT be create_if_missing"
        );
    }
}
