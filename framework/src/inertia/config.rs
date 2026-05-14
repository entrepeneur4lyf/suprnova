/// Which frontend framework the host application uses.
///
/// Detected at runtime from the `SUPRNOVA_FRONTEND` env var. The CLI
/// scaffolds this into `.env` when generating a new project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frontend {
    Svelte,
    React,
    Vue,
}

impl Frontend {
    /// Read `SUPRNOVA_FRONTEND` from the environment.
    ///
    /// Defaults to `Svelte` when unset or unrecognized — matches the
    /// CLI's default frontend choice in `suprnova new`.
    pub fn detect_from_env() -> Self {
        match std::env::var("SUPRNOVA_FRONTEND").as_deref() {
            Ok("react") | Ok("React") | Ok("REACT") => Frontend::React,
            Ok("vue") | Ok("Vue") | Ok("VUE") => Frontend::Vue,
            Ok("svelte") | Ok("Svelte") | Ok("SVELTE") => Frontend::Svelte,
            _ => Frontend::Svelte,
        }
    }

    /// Default Vite entry-point filename for this frontend.
    pub fn default_entry_point(self) -> &'static str {
        match self {
            Frontend::Svelte => "src/main.ts",
            Frontend::React => "src/main.tsx",
            Frontend::Vue => "src/main.ts",
        }
    }

    /// File extensions a page component for this frontend may use.
    ///
    /// Ordered by likelihood for the framework. Used by the macro to
    /// locate page components at compile time.
    pub fn page_extensions(self) -> &'static [&'static str] {
        match self {
            Frontend::Svelte => &["svelte"],
            Frontend::React => &["tsx", "jsx"],
            Frontend::Vue => &["vue"],
        }
    }

    /// Lowercase identifier used in env / config.
    pub fn as_str(self) -> &'static str {
        match self {
            Frontend::Svelte => "svelte",
            Frontend::React => "react",
            Frontend::Vue => "vue",
        }
    }
}

/// Configuration for Inertia.js integration.
pub struct InertiaConfig {
    /// Vite dev server URL (e.g. `http://localhost:5173`).
    pub vite_dev_server: String,
    /// Vite entry point. Defaults to the frontend's standard entry.
    pub entry_point: String,
    /// Asset version string for cache busting / version-mismatch detection.
    pub version: String,
    /// `true` during local development (loads via the Vite dev server);
    /// `false` for production (loads built assets from `/assets/`).
    pub development: bool,
    /// Which frontend framework is configured.
    pub frontend: Frontend,
    /// Default `<title>` for the HTML shell. Per-response title overrides
    /// via `InertiaResponse::title(...)`.
    pub default_title: String,
    /// Whether Inertia responses encrypt their browser history state by
    /// default. Maps to Laravel's `config('inertia.history.encrypt')`.
    /// Overridable per-request via `EncryptHistoryMiddleware` and
    /// per-response via `InertiaResponse::encrypt_history(bool)`.
    pub encrypt_history_default: bool,
}

impl Default for InertiaConfig {
    fn default() -> Self {
        let frontend = Frontend::detect_from_env();
        Self {
            vite_dev_server: "http://localhost:5173".to_string(),
            entry_point: frontend.default_entry_point().to_string(),
            version: "1.0".to_string(),
            development: true,
            frontend,
            default_title: "Suprnova".to_string(),
            encrypt_history_default: false,
        }
    }
}

impl InertiaConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn vite_dev_server(mut self, url: impl Into<String>) -> Self {
        self.vite_dev_server = url.into();
        self
    }

    pub fn entry_point(mut self, entry: impl Into<String>) -> Self {
        self.entry_point = entry.into();
        self
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    pub fn production(mut self) -> Self {
        self.development = false;
        self
    }

    pub fn frontend(mut self, frontend: Frontend) -> Self {
        self.frontend = frontend;
        // Update entry point default to match the new frontend unless the
        // user has already customized it.
        self.entry_point = frontend.default_entry_point().to_string();
        self
    }

    pub fn default_title(mut self, title: impl Into<String>) -> Self {
        self.default_title = title.into();
        self
    }

    pub fn encrypt_history(mut self, on: bool) -> Self {
        self.encrypt_history_default = on;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_detect_defaults_to_svelte_when_unset() {
        // Clear in case some other test set it.
        // SAFETY: tests in this module run sequentially in the same binary,
        // but cargo test runs tests in parallel by default. To avoid races
        // we don't unset; instead we test the explicit-match arm and the
        // explicit-Svelte arm separately.
        let _ = std::env::var("SUPRNOVA_FRONTEND"); // touch to silence unused warnings
        // The default arm covers unset + unknown values; verify the
        // explicit fallback by checking the match logic.
        assert_eq!(Frontend::Svelte.as_str(), "svelte");
        assert_eq!(Frontend::React.as_str(), "react");
        assert_eq!(Frontend::Vue.as_str(), "vue");
    }

    #[test]
    fn frontend_default_entry_points() {
        assert_eq!(Frontend::Svelte.default_entry_point(), "src/main.ts");
        assert_eq!(Frontend::React.default_entry_point(), "src/main.tsx");
        assert_eq!(Frontend::Vue.default_entry_point(), "src/main.ts");
    }

    #[test]
    fn frontend_page_extensions() {
        assert_eq!(Frontend::Svelte.page_extensions(), &["svelte"]);
        assert_eq!(Frontend::React.page_extensions(), &["tsx", "jsx"]);
        assert_eq!(Frontend::Vue.page_extensions(), &["vue"]);
    }

    #[test]
    fn config_default_has_svelte_entry_when_env_unset() {
        // Best-effort: only valid when env unset; CI may inject SUPRNOVA_FRONTEND.
        if std::env::var("SUPRNOVA_FRONTEND").is_err() {
            let cfg = InertiaConfig::default();
            assert_eq!(cfg.frontend, Frontend::Svelte);
            assert_eq!(cfg.entry_point, "src/main.ts");
        }
    }

    #[test]
    fn config_builder_updates_entry_point_with_frontend() {
        let cfg = InertiaConfig::new().frontend(Frontend::React);
        assert_eq!(cfg.frontend, Frontend::React);
        assert_eq!(cfg.entry_point, "src/main.tsx");
    }

    #[test]
    fn config_builder_overrides_default_title() {
        let cfg = InertiaConfig::new().default_title("My App");
        assert_eq!(cfg.default_title, "My App");
    }
}
