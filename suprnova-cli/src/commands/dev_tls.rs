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
}
