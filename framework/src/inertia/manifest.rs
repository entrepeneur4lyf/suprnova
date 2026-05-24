//! Vite manifest reader for production asset resolution.
//!
//! Vite emits a `manifest.json` (under `<outDir>/.vite/` since Vite 5.0)
//! that maps entry-point source paths to their hashed output files,
//! their imported CSS, and their dependency chunks (for `modulepreload`).
//!
//! `framework/src/inertia/response.rs::render_prod_head` reads this
//! manifest at first use and emits the correct asset tags in the
//! production HTML shell. Without it the framework would serve hardcoded
//! `/assets/main.js` paths that don't match Vite's hashed output —
//! production deployments would 404 on every page.
//!
//! # Schema
//!
//! The manifest's top-level object maps source path → entry:
//!
//! ```json
//! {
//!   "src/main.ts": {
//!     "file": "main-abc123.js",
//!     "name": "main",
//!     "src": "src/main.ts",
//!     "isEntry": true,
//!     "css": ["main-def456.css"],
//!     "imports": ["_chunk-xyz.js"]
//!   },
//!   "_chunk-xyz.js": {
//!     "file": "chunk-xyz789.js",
//!     "imports": []
//!   }
//! }
//! ```
//!
//! `imports` keys reference other entries in the same map (typically
//! starting with `_` for shared chunks). They are emitted as
//! `<link rel="modulepreload">` so the browser fetches them in parallel
//! with the entry script. CSS imported by a dependency chunk also
//! propagates up so every `<link rel="stylesheet">` is included on the
//! initial page.

use crate::error::FrameworkError;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// A single entry in `manifest.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestEntry {
    /// The hashed output filename (e.g. `main-abc123.js`).
    pub file: String,

    /// CSS files emitted alongside this entry (already hashed).
    #[serde(default)]
    pub css: Vec<String>,

    /// Other manifest keys this entry depends on. Walked recursively
    /// to build the modulepreload list and to pull transitive CSS.
    #[serde(default)]
    pub imports: Vec<String>,

    /// Marks this entry as a Vite "isEntry" target. Informational —
    /// resolution doesn't require it; we look up by configured path.
    #[serde(default, rename = "isEntry")]
    pub is_entry: bool,
}

/// Parsed Vite manifest.
#[derive(Debug, Clone)]
pub struct ViteManifest {
    entries: HashMap<String, ManifestEntry>,
}

/// Resolution of a manifest entry into the tags `render_prod_head`
/// needs to emit.
#[derive(Debug, Default)]
pub struct ResolvedAssets {
    /// JavaScript files to emit as `<script type="module" src="...">`.
    pub js: Vec<String>,
    /// CSS files to emit as `<link rel="stylesheet" href="...">`.
    pub css: Vec<String>,
    /// Dependency chunks to emit as `<link rel="modulepreload" href="...">`.
    pub preload: Vec<String>,
}

impl ViteManifest {
    /// Read and parse a Vite manifest from disk.
    pub fn load(path: &Path) -> Result<Self, FrameworkError> {
        let bytes = std::fs::read(path).map_err(|e| {
            FrameworkError::internal(format!(
                "Vite manifest not found at {}: {}",
                path.display(),
                e
            ))
        })?;
        let entries: HashMap<String, ManifestEntry> = serde_json::from_slice(&bytes)
            .map_err(|e| {
                FrameworkError::internal(format!(
                    "Vite manifest at {} is not valid JSON: {}",
                    path.display(),
                    e
                ))
            })?;
        Ok(Self { entries })
    }

    /// Build from an already-parsed entry map. Test hook.
    #[doc(hidden)]
    pub fn from_entries(entries: HashMap<String, ManifestEntry>) -> Self {
        Self { entries }
    }

    /// Resolve a source-path entry-point to the assets the browser
    /// needs. Returns `None` when the entry is not present in the
    /// manifest (typically a stale config pointing at a removed entry,
    /// or a manifest produced from a different `rollupOptions.input`).
    pub fn resolve_entry(&self, entry: &str) -> Option<ResolvedAssets> {
        let root = self.entries.get(entry)?;
        let mut resolved = ResolvedAssets {
            js: vec![root.file.clone()],
            css: root.css.clone(),
            preload: Vec::new(),
        };
        let mut visited = HashSet::new();
        visited.insert(entry.to_string());
        for dep in &root.imports {
            self.collect_imports(
                dep,
                &mut resolved.preload,
                &mut resolved.css,
                &mut visited,
            );
        }
        Some(resolved)
    }

    fn collect_imports(
        &self,
        key: &str,
        preload: &mut Vec<String>,
        css: &mut Vec<String>,
        visited: &mut HashSet<String>,
    ) {
        if !visited.insert(key.to_string()) {
            return;
        }
        let Some(entry) = self.entries.get(key) else {
            return;
        };
        if !preload.contains(&entry.file) {
            preload.push(entry.file.clone());
        }
        for c in &entry.css {
            if !css.contains(c) {
                css.push(c.clone());
            }
        }
        for imp in &entry.imports {
            self.collect_imports(imp, preload, css, visited);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ViteManifest {
        let mut entries = HashMap::new();
        entries.insert(
            "src/main.ts".to_string(),
            ManifestEntry {
                file: "assets/main-AAA.js".to_string(),
                css: vec!["assets/main-BBB.css".to_string()],
                imports: vec!["_chunk-shared.js".to_string()],
                is_entry: true,
            },
        );
        entries.insert(
            "_chunk-shared.js".to_string(),
            ManifestEntry {
                file: "assets/chunk-shared-CCC.js".to_string(),
                css: vec!["assets/chunk-shared-DDD.css".to_string()],
                imports: vec!["_chunk-deep.js".to_string()],
                is_entry: false,
            },
        );
        entries.insert(
            "_chunk-deep.js".to_string(),
            ManifestEntry {
                file: "assets/chunk-deep-EEE.js".to_string(),
                css: Vec::new(),
                imports: Vec::new(),
                is_entry: false,
            },
        );
        ViteManifest::from_entries(entries)
    }

    #[test]
    fn resolves_entry_with_one_js_and_one_css() {
        let m = fixture();
        let r = m.resolve_entry("src/main.ts").unwrap();
        assert_eq!(r.js, vec!["assets/main-AAA.js".to_string()]);
        // CSS pulls in the entry CSS and the transitively-imported chunk CSS.
        assert!(r.css.contains(&"assets/main-BBB.css".to_string()));
        assert!(r.css.contains(&"assets/chunk-shared-DDD.css".to_string()));
    }

    #[test]
    fn collects_recursive_modulepreloads() {
        let m = fixture();
        let r = m.resolve_entry("src/main.ts").unwrap();
        assert!(r.preload.contains(&"assets/chunk-shared-CCC.js".to_string()));
        assert!(r.preload.contains(&"assets/chunk-deep-EEE.js".to_string()));
    }

    #[test]
    fn missing_entry_returns_none() {
        let m = fixture();
        assert!(m.resolve_entry("src/missing.ts").is_none());
    }

    #[test]
    fn handles_circular_imports_without_stack_overflow() {
        let mut entries = HashMap::new();
        entries.insert(
            "src/main.ts".to_string(),
            ManifestEntry {
                file: "main.js".into(),
                css: vec![],
                imports: vec!["_a.js".into()],
                is_entry: true,
            },
        );
        entries.insert(
            "_a.js".to_string(),
            ManifestEntry {
                file: "a.js".into(),
                css: vec![],
                imports: vec!["_b.js".into()],
                is_entry: false,
            },
        );
        entries.insert(
            "_b.js".to_string(),
            ManifestEntry {
                file: "b.js".into(),
                css: vec![],
                imports: vec!["_a.js".into()],
                is_entry: false,
            },
        );
        let m = ViteManifest::from_entries(entries);
        let r = m.resolve_entry("src/main.ts").unwrap();
        // Both deps collected exactly once.
        assert_eq!(r.preload.iter().filter(|p| p == &"a.js").count(), 1);
        assert_eq!(r.preload.iter().filter(|p| p == &"b.js").count(), 1);
    }

    #[test]
    fn load_returns_error_for_missing_file() {
        let res = ViteManifest::load(Path::new("/nonexistent/manifest.json"));
        assert!(res.is_err());
    }

    #[test]
    fn load_returns_error_for_invalid_json() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("test-manifest-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"not valid json").unwrap();
        let res = ViteManifest::load(&path);
        std::fs::remove_file(&path).ok();
        assert!(res.is_err());
    }

    #[test]
    fn load_parses_real_vite_manifest_shape() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("test-manifest-{}.json", uuid::Uuid::new_v4()));
        let content = r#"{
            "src/main.ts": {
                "file": "main-Q9zSqcUL.js",
                "name": "main",
                "src": "src/main.ts",
                "isEntry": true,
                "css": ["main-3R4lN-AT.css"],
                "imports": ["_lib-DTQbz0Cz.js"]
            },
            "_lib-DTQbz0Cz.js": {
                "file": "lib-DTQbz0Cz.js"
            }
        }"#;
        std::fs::write(&path, content).unwrap();
        let m = ViteManifest::load(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let r = m.resolve_entry("src/main.ts").unwrap();
        assert_eq!(r.js, vec!["main-Q9zSqcUL.js".to_string()]);
        assert_eq!(r.css, vec!["main-3R4lN-AT.css".to_string()]);
        assert_eq!(r.preload, vec!["lib-DTQbz0Cz.js".to_string()]);
    }
}
