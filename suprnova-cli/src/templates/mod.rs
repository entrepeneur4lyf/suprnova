// Types for entity generation templates

/// Column information from database schema
pub struct ColumnInfo {
    pub name: String,
    pub col_type: String,
    pub is_nullable: bool,
    pub is_primary_key: bool,
}

/// Table information from database schema
pub struct TableInfo {
    pub name: String,
    pub columns: Vec<ColumnInfo>,
}

// Backend templates

pub fn cargo_toml(package_name: &str, description: &str, author: &str) -> String {
    let authors_line = if author.is_empty() {
        String::new()
    } else {
        format!("authors = [\"{}\"]\n", author)
    };

    format!(
        include_str!("files/backend/Cargo.toml.tpl"),
        package_name = package_name,
        description = description,
        authors_line = authors_line
    )
}

pub fn cmd_main_rs(package_name: &str) -> String {
    include_str!("files/backend/cmd/main.rs.tpl")
        .replace("{package_name}", package_name)
}

pub fn lib_rs() -> &'static str {
    include_str!("files/backend/lib.rs.tpl")
}

pub fn routes_rs() -> &'static str {
    include_str!("files/backend/routes.rs.tpl")
}

pub fn controllers_mod() -> &'static str {
    include_str!("files/backend/controllers/mod.rs.tpl")
}

pub fn home_controller() -> &'static str {
    include_str!("files/backend/controllers/home.rs.tpl")
}

pub fn create_workflows_migration() -> &'static str {
    include_str!("files/backend/migrations/create_workflows_table.rs.tpl")
}

pub fn create_workflow_steps_migration() -> &'static str {
    include_str!("files/backend/migrations/create_workflow_steps_table.rs.tpl")
}

// Middleware templates

pub fn middleware_mod() -> &'static str {
    include_str!("files/backend/middleware/mod.rs.tpl")
}

pub fn middleware_logging() -> &'static str {
    include_str!("files/backend/middleware/logging.rs.tpl")
}

/// Template for generating new middleware with make:middleware command.
///
/// Emits a real, working middleware skeleton: it logs the inbound method,
/// path, and per-request id, runs the inner handler, and logs completion
/// time once the response is in hand. Production-safe out of the box —
/// users replace the body with whatever they actually need (auth checks,
/// CORS, tracing context, etc).
pub fn middleware_template(name: &str, struct_name: &str) -> String {
    format!(
        r#"//! {name} middleware

use std::time::Instant;

use suprnova::{{async_trait, current_request_id, Middleware, Next, Request, Response}};

/// {name} middleware.
///
/// Times the wrapped request and logs both the inbound and outbound
/// events with the per-request id installed by the framework's
/// `RequestIdMiddleware`. Replace the body below with your own logic
/// when you need different behavior.
pub struct {struct_name};

#[async_trait]
impl Middleware for {struct_name} {{
    async fn handle(&self, request: Request, next: Next) -> Response {{
        let method = request.method().to_string();
        let path = request.path().to_string();
        let request_id = current_request_id()
            .map(|id| id.as_str().to_string())
            .unwrap_or_default();
        let started_at = Instant::now();

        println!(
            "[{struct_name}] --> {{}} {{}} (request_id={{}})",
            method, path, request_id,
        );

        let response = next(request).await;

        println!(
            "[{struct_name}] <-- {{}} {{}} ({{}} ms, request_id={{}})",
            method,
            path,
            started_at.elapsed().as_millis(),
            request_id,
        );

        response
    }}
}}
"#,
        name = name,
        struct_name = struct_name
    )
}

/// Template for generating new controller with make:controller command
pub fn controller_template(name: &str) -> String {
    format!(
        r#"//! {name} controller

use suprnova::{{handler, json_response, Request, Response}};

#[handler]
pub async fn invoke(_req: Request) -> Response {{
    json_response!({{
        "controller": "{name}"
    }})
}}
"#,
        name = name
    )
}

/// Template for generating new action with make:action command.
///
/// Emits a real, working single-responsibility action: a container-resolvable
/// struct with an async `execute()` that returns `Result<String,
/// FrameworkError>`. Compiles out of the box and demonstrates the resolve →
/// invoke pattern that controllers use. Replace the body when you need
/// different behavior; the signature is the production-safe shape every
/// Suprnova action uses.
pub fn action_template(name: &str, struct_name: &str) -> String {
    format!(
        r#"//! {name} action

use suprnova::{{injectable, FrameworkError}};

/// {struct_name}
///
/// Single-responsibility command resolved from the container. Inject any
/// dependencies as fields and the `#[injectable]` macro wires them at
/// resolve time.
#[injectable]
pub struct {struct_name} {{
    // Add injected dependencies as fields here, e.g.
    // db: suprnova::DbConnection,
}}

impl {struct_name} {{
    /// Execute the action.
    ///
    /// Returns a status string by default so the skeleton compiles and
    /// runs immediately. Swap the body for the real workflow when you
    /// implement the feature — typically wrap fallible work in
    /// `?` and return the produced value.
    pub async fn execute(&self) -> Result<String, FrameworkError> {{
        Ok("{struct_name} executed".to_string())
    }}
}}
"#,
        name = name,
        struct_name = struct_name
    )
}

/// Template for generating a new Inertia page with `make:inertia`.
///
/// Dispatches to the right per-frontend snippet so the generated file
/// compiles in the user's actual project.
pub fn inertia_page_template(component_name: &str, frontend: Frontend) -> String {
    let ext = frontend.page_ext();
    match frontend {
        Frontend::React => format!(
            r#"export default function {component_name}() {{
  return (
    <div className="font-sans p-8 max-w-xl mx-auto">
      <h1 className="text-3xl font-bold">{component_name}</h1>
      <p className="mt-2">
        Edit <code className="bg-gray-100 px-1 rounded">frontend/src/pages/{component_name}.{ext}</code> to get started.
      </p>
    </div>
  )
}}
"#,
            component_name = component_name,
            ext = ext,
        ),
        Frontend::Svelte => format!(
            r#"<div class="font-sans p-8 max-w-xl mx-auto">
  <h1 class="text-3xl font-bold">{component_name}</h1>
  <p class="mt-2">
    Edit <code class="bg-gray-100 px-1 rounded">frontend/src/pages/{component_name}.{ext}</code> to get started.
  </p>
</div>
"#,
            component_name = component_name,
            ext = ext,
        ),
        Frontend::Vue => format!(
            r#"<script setup lang="ts">
</script>

<template>
  <div class="font-sans p-8 max-w-xl mx-auto">
    <h1 class="text-3xl font-bold">{component_name}</h1>
    <p class="mt-2">
      Edit <code class="bg-gray-100 px-1 rounded">frontend/src/pages/{component_name}.{ext}</code> to get started.
    </p>
  </div>
</template>
"#,
            component_name = component_name,
            ext = ext,
        ),
    }
}

/// Template for generating new error with make:error command
pub fn error_template(struct_name: &str) -> String {
    // Convert PascalCase to human readable message
    let mut message = String::new();
    for (i, c) in struct_name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            message.push(' ');
            message.push(c.to_lowercase().next().unwrap());
        } else {
            message.push(c);
        }
    }

    format!(
        r#"//! {struct_name} error

use suprnova::domain_error;

#[domain_error(status = 500, message = "{message}")]
pub struct {struct_name};
"#,
        struct_name = struct_name,
        message = message
    )
}

/// Template for models/mod.rs
pub fn models_mod() -> &'static str {
    include_str!("files/backend/models/mod.rs.tpl")
}

// Actions templates

pub fn actions_mod() -> &'static str {
    include_str!("files/backend/actions/mod.rs.tpl")
}

pub fn example_action() -> &'static str {
    include_str!("files/backend/actions/example_action.rs.tpl")
}

// Config templates

pub fn config_mod() -> &'static str {
    include_str!("files/backend/config/mod.rs.tpl")
}

pub fn config_database() -> &'static str {
    include_str!("files/backend/config/database.rs.tpl")
}

pub fn config_mail() -> &'static str {
    include_str!("files/backend/config/mail.rs.tpl")
}

pub fn bootstrap() -> &'static str {
    include_str!("files/backend/bootstrap.rs.tpl")
}

// Migrations templates

pub fn migrations_mod() -> &'static str {
    include_str!("files/backend/migrations/mod.rs.tpl")
}

// migrate_bin removed - migrations now integrated into main binary

// Frontend templates
//
// Per-framework submodules. Add a new framework by mirroring the structure
// in `files/frontend/<name>/` and adding a submodule below.

use std::fs;
use std::path::Path;

/// Which frontend framework the user picked when scaffolding the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frontend {
    React,
    Svelte,
    Vue,
}

impl Frontend {
    pub fn as_str(&self) -> &'static str {
        match self {
            Frontend::React => "react",
            Frontend::Svelte => "svelte",
            Frontend::Vue => "vue",
        }
    }

    /// File written at `frontend/src/<main_file_name>`, the Vite entry point.
    fn main_file_name(&self) -> &'static str {
        match self {
            Frontend::React => "main.tsx",
            Frontend::Svelte => "main.ts",
            Frontend::Vue => "main.ts",
        }
    }

    /// Extension for page components.
    pub fn page_ext(&self) -> &'static str {
        match self {
            Frontend::React => "tsx",
            Frontend::Svelte => "svelte",
            Frontend::Vue => "vue",
        }
    }

    /// Read the frontend choice from the `SUPRNOVA_FRONTEND` env var,
    /// honoring any `.env` already loaded into the process. Defaults to
    /// Svelte when unset or unparseable — matches the framework's
    /// `InertiaConfig::default()` behavior.
    pub fn detect_from_env() -> Self {
        match std::env::var("SUPRNOVA_FRONTEND") {
            Ok(s) => s.parse().unwrap_or(Frontend::Svelte),
            Err(_) => Frontend::Svelte,
        }
    }
}

impl std::str::FromStr for Frontend {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "react" => Ok(Frontend::React),
            "svelte" => Ok(Frontend::Svelte),
            "vue" | "vue3" => Ok(Frontend::Vue),
            other => Err(format!(
                "Unknown frontend '{}'. Supported: react, svelte, vue",
                other
            )),
        }
    }
}

pub mod react {
    pub fn package_json(project_name: &str) -> String {
        include_str!("files/frontend/react/package.json.tpl")
            .replace("{project_name}", project_name)
    }
    pub fn vite_config() -> &'static str {
        include_str!("files/frontend/react/vite.config.ts.tpl")
    }
    pub fn tsconfig() -> &'static str {
        include_str!("files/frontend/react/tsconfig.json.tpl")
    }
    pub fn index_html(project_title: &str) -> String {
        include_str!("files/frontend/react/index.html.tpl")
            .replace("{project_title}", project_title)
    }
    pub fn main_file() -> &'static str {
        include_str!("files/frontend/react/src/main.tsx.tpl")
    }
    pub fn home_page() -> &'static str {
        include_str!("files/frontend/react/src/pages/Home.tsx.tpl")
    }
    pub fn dashboard_page() -> &'static str {
        include_str!("files/frontend/react/src/pages/Dashboard.tsx.tpl")
    }
    pub fn login_page() -> &'static str {
        include_str!("files/frontend/react/src/pages/auth/Login.tsx.tpl")
    }
    pub fn register_page() -> &'static str {
        include_str!("files/frontend/react/src/pages/auth/Register.tsx.tpl")
    }
    pub fn inertia_props_types() -> &'static str {
        include_str!("files/frontend/react/src/types/inertia-props.ts.tpl")
    }
    pub fn app_css() -> &'static str {
        include_str!("files/frontend/react/src/app.css.tpl")
    }
}

pub mod svelte {
    pub fn package_json(project_name: &str) -> String {
        include_str!("files/frontend/svelte/package.json.tpl")
            .replace("{project_name}", project_name)
    }
    pub fn vite_config() -> &'static str {
        include_str!("files/frontend/svelte/vite.config.ts.tpl")
    }
    pub fn svelte_config() -> &'static str {
        include_str!("files/frontend/svelte/svelte.config.js.tpl")
    }
    pub fn tsconfig() -> &'static str {
        include_str!("files/frontend/svelte/tsconfig.json.tpl")
    }
    pub fn app_dts() -> &'static str {
        include_str!("files/frontend/svelte/src/app.d.ts.tpl")
    }
    pub fn app_css() -> &'static str {
        include_str!("files/frontend/svelte/src/app.css.tpl")
    }
    pub fn index_html(project_title: &str) -> String {
        include_str!("files/frontend/svelte/index.html.tpl")
            .replace("{project_title}", project_title)
    }
    pub fn main_file() -> &'static str {
        include_str!("files/frontend/svelte/src/main.ts.tpl")
    }
    pub fn home_page() -> &'static str {
        include_str!("files/frontend/svelte/src/pages/Home.svelte.tpl")
    }
    pub fn dashboard_page() -> &'static str {
        include_str!("files/frontend/svelte/src/pages/Dashboard.svelte.tpl")
    }
    pub fn login_page() -> &'static str {
        include_str!("files/frontend/svelte/src/pages/auth/Login.svelte.tpl")
    }
    pub fn register_page() -> &'static str {
        include_str!("files/frontend/svelte/src/pages/auth/Register.svelte.tpl")
    }
    pub fn inertia_props_types() -> &'static str {
        include_str!("files/frontend/svelte/src/types/inertia-props.ts.tpl")
    }
}

pub mod vue {
    pub fn package_json(project_name: &str) -> String {
        include_str!("files/frontend/vue/package.json.tpl")
            .replace("{project_name}", project_name)
    }
    pub fn vite_config() -> &'static str {
        include_str!("files/frontend/vue/vite.config.ts.tpl")
    }
    pub fn tsconfig() -> &'static str {
        include_str!("files/frontend/vue/tsconfig.json.tpl")
    }
    pub fn shims_dts() -> &'static str {
        include_str!("files/frontend/vue/src/shims-vue.d.ts.tpl")
    }
    pub fn index_html(project_title: &str) -> String {
        include_str!("files/frontend/vue/index.html.tpl")
            .replace("{project_title}", project_title)
    }
    pub fn main_file() -> &'static str {
        include_str!("files/frontend/vue/src/main.ts.tpl")
    }
    pub fn home_page() -> &'static str {
        include_str!("files/frontend/vue/src/pages/Home.vue.tpl")
    }
    pub fn dashboard_page() -> &'static str {
        include_str!("files/frontend/vue/src/pages/Dashboard.vue.tpl")
    }
    pub fn login_page() -> &'static str {
        include_str!("files/frontend/vue/src/pages/auth/Login.vue.tpl")
    }
    pub fn register_page() -> &'static str {
        include_str!("files/frontend/vue/src/pages/auth/Register.vue.tpl")
    }
    pub fn inertia_props_types() -> &'static str {
        include_str!("files/frontend/vue/src/types/inertia-props.ts.tpl")
    }
    pub fn app_css() -> &'static str {
        include_str!("files/frontend/vue/src/app.css.tpl")
    }
}

/// Write all the frontend files for the chosen framework under
/// `<project_path>/frontend/`.
pub fn scaffold_frontend(
    project_path: &Path,
    project_name: &str,
    project_title: &str,
    frontend: Frontend,
) -> Result<(), String> {
    let fe = project_path.join("frontend");
    let src = fe.join("src");
    let pages = src.join("pages");
    let auth = pages.join("auth");
    let types = src.join("types");
    for d in [&fe, &src, &pages, &auth, &types] {
        fs::create_dir_all(d).map_err(|e| format!("Failed to create {}: {}", d.display(), e))?;
    }

    let ext = frontend.page_ext();
    let main = src.join(frontend.main_file_name());

    let (pkg, vite, ts, index, main_src, home, dash, login, reg, props, css) = match frontend {
        Frontend::React => (
            react::package_json(project_name),
            react::vite_config().to_string(),
            react::tsconfig().to_string(),
            react::index_html(project_title),
            react::main_file().to_string(),
            react::home_page().to_string(),
            react::dashboard_page().to_string(),
            react::login_page().to_string(),
            react::register_page().to_string(),
            react::inertia_props_types().to_string(),
            react::app_css().to_string(),
        ),
        Frontend::Svelte => (
            svelte::package_json(project_name),
            svelte::vite_config().to_string(),
            svelte::tsconfig().to_string(),
            svelte::index_html(project_title),
            svelte::main_file().to_string(),
            svelte::home_page().to_string(),
            svelte::dashboard_page().to_string(),
            svelte::login_page().to_string(),
            svelte::register_page().to_string(),
            svelte::inertia_props_types().to_string(),
            svelte::app_css().to_string(),
        ),
        Frontend::Vue => (
            vue::package_json(project_name),
            vue::vite_config().to_string(),
            vue::tsconfig().to_string(),
            vue::index_html(project_title),
            vue::main_file().to_string(),
            vue::home_page().to_string(),
            vue::dashboard_page().to_string(),
            vue::login_page().to_string(),
            vue::register_page().to_string(),
            vue::inertia_props_types().to_string(),
            vue::app_css().to_string(),
        ),
    };

    let writes: &[(std::path::PathBuf, &str)] = &[
        (fe.join("package.json"), &pkg),
        (fe.join("vite.config.ts"), &vite),
        (fe.join("tsconfig.json"), &ts),
        (fe.join("index.html"), &index),
        (main, &main_src),
        (src.join("app.css"), &css),
        (pages.join(format!("Home.{}", ext)), &home),
        (pages.join(format!("Dashboard.{}", ext)), &dash),
        (auth.join(format!("Login.{}", ext)), &login),
        (auth.join(format!("Register.{}", ext)), &reg),
        (types.join("inertia-props.ts"), &props),
    ];

    for (path, content) in writes {
        fs::write(path, content)
            .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
    }

    // Per-framework extras.
    match frontend {
        Frontend::Svelte => {
            fs::write(fe.join("svelte.config.js"), svelte::svelte_config())
                .map_err(|e| format!("Failed to write svelte.config.js: {}", e))?;
            fs::write(src.join("app.d.ts"), svelte::app_dts())
                .map_err(|e| format!("Failed to write src/app.d.ts: {}", e))?;
        }
        Frontend::Vue => {
            fs::write(src.join("shims-vue.d.ts"), vue::shims_dts())
                .map_err(|e| format!("Failed to write src/shims-vue.d.ts: {}", e))?;
        }
        Frontend::React => {}
    }

    Ok(())
}

// ============================================================================
// API-only starter templates
// ============================================================================

pub mod api {
    pub fn cargo_toml(package_name: &str, project_name: &str) -> String {
        include_str!("files/api/Cargo.toml.tpl")
            .replace("{package_name}", package_name)
            .replace("{project_name}", project_name)
    }
    pub fn main_rs(package_name: &str, project_name: &str) -> String {
        include_str!("files/api/src/main.rs.tpl")
            .replace("{package_name}", package_name)
            .replace("{project_name}", project_name)
    }
    pub fn lib_rs() -> &'static str {
        include_str!("files/api/src/lib.rs.tpl")
    }
    pub fn bootstrap_rs() -> &'static str {
        include_str!("files/api/src/bootstrap.rs.tpl")
    }
    pub fn routes_rs() -> &'static str {
        include_str!("files/api/src/routes.rs.tpl")
    }
    pub fn config_mod_rs() -> &'static str {
        include_str!("files/api/src/config/mod.rs.tpl")
    }
    pub fn controllers_mod_rs() -> &'static str {
        include_str!("files/api/src/controllers/mod.rs.tpl")
    }
    pub fn controllers_users_rs() -> &'static str {
        include_str!("files/api/src/controllers/users.rs.tpl")
    }
    pub fn resources_mod_rs() -> &'static str {
        include_str!("files/api/src/resources/mod.rs.tpl")
    }
    pub fn resources_user_resource_rs() -> &'static str {
        include_str!("files/api/src/resources/user_resource.rs.tpl")
    }
    pub fn models_mod_rs() -> &'static str {
        include_str!("files/api/src/models/mod.rs.tpl")
    }
    pub fn models_user_rs() -> &'static str {
        include_str!("files/api/src/models/user.rs.tpl")
    }
    pub fn migrations_mod_rs() -> &'static str {
        include_str!("files/api/src/migrations/mod.rs.tpl")
    }
    pub fn migrations_create_users_rs() -> &'static str {
        include_str!("files/api/src/migrations/create_users_table.rs.tpl")
    }
    pub fn env(package_name: &str, project_name: &str, app_key: &str) -> String {
        include_str!("files/api/.env.tpl")
            .replace("{package_name}", package_name)
            .replace("{project_name}", project_name)
            .replace("{app_key}", app_key)
    }
    pub fn gitignore() -> &'static str {
        include_str!("files/api/.gitignore.tpl")
    }
}

/// Scaffold a JSON:API-only project under `project_path`.
///
/// No Inertia, no frontend. Registers `BearerTokenMiddleware` and
/// `IncludeMiddleware` globally; includes example `UserResource` with
/// `#[derive(Data)] #[json_resource("users")]`.
pub fn scaffold_api(
    project_path: &Path,
    project_name: &str,
    package_name: &str,
) -> Result<(), String> {
    let src = project_path.join("src");
    let config = src.join("config");
    let controllers = src.join("controllers");
    let resources = src.join("resources");
    let models = src.join("models");
    let migrations = src.join("migrations");

    for d in [&src, &config, &controllers, &resources, &models, &migrations] {
        fs::create_dir_all(d)
            .map_err(|e| format!("Failed to create {}: {}", d.display(), e))?;
    }

    let writes: &[(std::path::PathBuf, String)] = &[
        (
            project_path.join("Cargo.toml"),
            api::cargo_toml(package_name, project_name),
        ),
        (
            src.join("main.rs"),
            api::main_rs(package_name, project_name),
        ),
        (src.join("lib.rs"), api::lib_rs().to_string()),
        (src.join("bootstrap.rs"), api::bootstrap_rs().to_string()),
        (src.join("routes.rs"), api::routes_rs().to_string()),
        (config.join("mod.rs"), api::config_mod_rs().to_string()),
        (
            controllers.join("mod.rs"),
            api::controllers_mod_rs().to_string(),
        ),
        (
            controllers.join("users.rs"),
            api::controllers_users_rs().to_string(),
        ),
        (
            resources.join("mod.rs"),
            api::resources_mod_rs().to_string(),
        ),
        (
            resources.join("user_resource.rs"),
            api::resources_user_resource_rs().to_string(),
        ),
        (models.join("mod.rs"), api::models_mod_rs().to_string()),
        (models.join("user.rs"), api::models_user_rs().to_string()),
        (
            migrations.join("mod.rs"),
            api::migrations_mod_rs().to_string(),
        ),
        (
            migrations.join("m20240101_000001_create_users_table.rs"),
            api::migrations_create_users_rs().to_string(),
        ),
        (
            project_path.join(".env"),
            api::env(
                package_name,
                project_name,
                &crate::commands::key_generate::generate_app_key(),
            ),
        ),
        (
            project_path.join(".gitignore"),
            api::gitignore().to_string(),
        ),
    ];

    for (path, content) in writes {
        fs::write(path, content)
            .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
    }

    Ok(())
}

// Auth backend templates

pub fn auth_controller() -> &'static str {
    include_str!("files/backend/controllers/auth.rs.tpl")
}

pub fn dashboard_controller() -> &'static str {
    include_str!("files/backend/controllers/dashboard.rs.tpl")
}

pub fn authenticate_middleware() -> &'static str {
    include_str!("files/backend/middleware/authenticate.rs.tpl")
}

pub fn user_model() -> &'static str {
    include_str!("files/backend/models/user.rs.tpl")
}

// Auth migration templates

pub fn create_users_migration() -> &'static str {
    include_str!("files/backend/migrations/create_users_table.rs.tpl")
}

pub fn create_sessions_migration() -> &'static str {
    include_str!("files/backend/migrations/create_sessions_table.rs.tpl")
}

// Root templates

pub fn gitignore() -> &'static str {
    include_str!("files/root/gitignore.tpl")
}

pub fn env(project_name: &str, app_key: &str) -> String {
    include_str!("files/root/env.tpl")
        .replace("{project_name}", project_name)
        .replace("{app_key}", app_key)
}

pub fn env_example() -> &'static str {
    include_str!("files/root/env.example.tpl")
}

// Entity generation templates for db:sync command

/// Generate auto-generated entity file (regenerated on every sync)
pub fn entity_template(table_name: &str, columns: &[ColumnInfo]) -> String {
    let _struct_name = to_pascal_case(&singularize(table_name));

    // Generate column fields
    let column_fields: Vec<String> = columns
        .iter()
        .map(|col| {
            let rust_type = sql_type_to_rust_type(col);
            let mut attrs = Vec::new();

            if col.is_primary_key {
                attrs.push("    #[sea_orm(primary_key)]".to_string());
            }

            let field = format!("    pub {}: {},", col.name, rust_type);
            if attrs.is_empty() {
                field
            } else {
                format!("{}\n{}", attrs.join("\n"), field)
            }
        })
        .collect();

    // Find primary key columns (reserved for future use)
    let _pk_columns: Vec<&ColumnInfo> = columns.iter().filter(|c| c.is_primary_key).collect();

    format!(
        r#"// AUTO-GENERATED FILE - DO NOT EDIT
// Generated by `suprnova db:sync` - Changes will be overwritten
// Add custom code to src/models/{table_name}.rs instead

use sea_orm::entity::prelude::*;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "{table_name}")]
pub struct Model {{
{columns}
}}

// Note: Relation enum is required here for DeriveEntityModel macro.
// Define your actual relations in src/models/{table_name}.rs using the Related trait.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {{}}
"#,
        table_name = table_name,
        columns = column_fields.join("\n"),
    )
}

/// Generate user model file with Eloquent-like API (created only once, never overwritten)
pub fn user_model_template(table_name: &str, struct_name: &str, columns: &[ColumnInfo]) -> String {
    let model_setters = generate_model_setters(columns);
    let builder_fields = generate_builder_fields(columns);
    let builder_setters = generate_builder_setters(columns);
    let builder_to_active = generate_builder_to_active(columns);
    let model_to_active = generate_model_to_active(columns);
    let pk_field = columns
        .iter()
        .find(|c| c.is_primary_key)
        .map(|c| c.name.as_str())
        .unwrap_or("id");

    format!(
        r#"//! {struct_name} model
//!
//! This file contains custom implementations for the {struct_name} model.
//! The base entity is auto-generated in src/models/entities/{table_name}.rs
//!
//! This file is NEVER overwritten by `suprnova db:sync` - your custom code is safe here.

// Re-export the auto-generated entity
pub use super::entities::{table_name}::*;

use suprnova::database::{{ModelMut, QueryBuilder}};
use sea_orm::{{entity::prelude::*, Set}};

/// Type alias for convenient access
pub type {struct_name} = Model;

// ============================================================================
// ENTITY CONFIGURATION
// ============================================================================

impl ActiveModelBehavior for ActiveModel {{}}

impl suprnova::database::Model for Entity {{}}
impl suprnova::database::ModelMut for Entity {{}}

// ============================================================================
// ELOQUENT-LIKE API
// Fluent query builder and setter methods for {struct_name}
// ============================================================================

impl Model {{
    /// Start a new query builder
    ///
    /// # Example
    /// ```rust,ignore
    /// let records = {struct_name}::query().all().await?;
    /// let record = {struct_name}::query().filter(Column::Id.eq(1)).first().await?;
    /// ```
    pub fn query() -> QueryBuilder<Entity> {{
        QueryBuilder::new()
    }}

    /// Create a new record builder
    ///
    /// # Example
    /// ```rust,ignore
    /// let record = {struct_name}::create()
    ///     .set_field("value")
    ///     .insert()
    ///     .await?;
    /// ```
    pub fn create() -> {struct_name}Builder {{
        {struct_name}Builder::default()
    }}

{model_setters}
    /// Save changes to the database
    ///
    /// # Example
    /// ```rust,ignore
    /// let updated = record.set_field("new_value").update().await?;
    /// ```
    pub async fn update(self) -> Result<Self, suprnova::FrameworkError> {{
        let active = self.to_active_model();
        Entity::update_one(active).await
    }}

    /// Delete this record from the database
    ///
    /// # Example
    /// ```rust,ignore
    /// record.delete().await?;
    /// ```
    pub async fn delete(self) -> Result<u64, suprnova::FrameworkError> {{
        Entity::delete_by_pk(self.{pk_field}).await
    }}

    fn to_active_model(&self) -> ActiveModel {{
{model_to_active}
    }}
}}

// ============================================================================
// BUILDER
// For creating new records with fluent setter pattern
// ============================================================================

/// Builder for creating new {struct_name} records
#[derive(Default)]
pub struct {struct_name}Builder {{
{builder_fields}
}}

impl {struct_name}Builder {{
{builder_setters}
    /// Insert the record into the database
    ///
    /// # Example
    /// ```rust,ignore
    /// let record = {struct_name}::create()
    ///     .set_field("value")
    ///     .insert()
    ///     .await?;
    /// ```
    pub async fn insert(self) -> Result<Model, suprnova::FrameworkError> {{
        let active = self.build();
        Entity::insert_one(active).await
    }}

    fn build(self) -> ActiveModel {{
{builder_to_active}
    }}
}}

// ============================================================================
// CUSTOM METHODS
// Add your custom query and mutation methods below
// ============================================================================

// Example custom finder:
// impl Model {{
//     pub async fn find_by_email(email: &str) -> Result<Option<Self>, suprnova::FrameworkError> {{
//         Self::query().filter(Column::Email.eq(email)).first().await
//     }}
// }}

// ============================================================================
// RELATIONS
// Define relationships to other entities here
// ============================================================================

// Example: One-to-Many relation
// impl Entity {{
//     pub fn has_many_posts() -> RelationDef {{
//         Entity::has_many(super::posts::Entity).into()
//     }}
// }}

// Example: Belongs-To relation
// impl Entity {{
//     pub fn belongs_to_user() -> RelationDef {{
//         Entity::belongs_to(super::users::Entity)
//             .from(Column::UserId)
//             .to(super::users::Column::Id)
//             .into()
//     }}
// }}
"#,
        struct_name = struct_name,
        table_name = table_name,
        model_setters = model_setters,
        builder_fields = builder_fields,
        builder_setters = builder_setters,
        builder_to_active = builder_to_active,
        model_to_active = model_to_active,
        pk_field = pk_field,
    )
}

/// Generate entities/mod.rs (regenerated on every sync)
pub fn entities_mod_template(tables: &[TableInfo]) -> String {
    let mut content =
        String::from("// AUTO-GENERATED FILE - DO NOT EDIT\n// Generated by `suprnova db:sync`\n\n");

    for table in tables {
        content.push_str(&format!("pub mod {};\n", table.name));
    }

    content
}

// Helper functions for entity generation

fn sql_type_to_rust_type(col: &ColumnInfo) -> String {
    let col_type_upper = col.col_type.to_uppercase();
    let base_type = if col_type_upper.contains("INT") {
        if col_type_upper.contains("BIGINT") || col_type_upper.contains("INT8") {
            "i64"
        } else if col_type_upper.contains("SMALLINT") || col_type_upper.contains("INT2") {
            "i16"
        } else {
            "i32"
        }
    } else if col_type_upper.contains("TEXT")
        || col_type_upper.contains("VARCHAR")
        || col_type_upper.contains("CHAR")
        || col_type_upper.contains("CHARACTER")
    {
        "String"
    } else if col_type_upper.contains("BOOL") {
        "bool"
    } else if col_type_upper.contains("REAL") || col_type_upper.contains("FLOAT4") {
        "f32"
    } else if col_type_upper.contains("DOUBLE") || col_type_upper.contains("FLOAT8") {
        "f64"
    } else if col_type_upper.contains("TIMESTAMP") || col_type_upper.contains("DATETIME") {
        "DateTimeUtc"
    } else if col_type_upper.contains("DATE") {
        "Date"
    } else if col_type_upper.contains("TIME") {
        "Time"
    } else if col_type_upper.contains("UUID") {
        "Uuid"
    } else if col_type_upper.contains("JSON") {
        "Json"
    } else if col_type_upper.contains("BYTEA") || col_type_upper.contains("BLOB") {
        "Vec<u8>"
    } else if col_type_upper.contains("DECIMAL") || col_type_upper.contains("NUMERIC") {
        "Decimal"
    } else {
        "String" // fallback
    };

    if col.is_nullable {
        format!("Option<{}>", base_type)
    } else {
        base_type.to_string()
    }
}

fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;

    for c in s.chars() {
        if c == '_' || c == '-' || c == ' ' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_uppercase().next().unwrap());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }
    result
}

fn singularize(word: &str) -> String {
    if let Some(stem) = word.strip_suffix("ies") {
        format!("{}y", stem)
    } else if word.ends_with("es") && !word.ends_with("ses") && !word.ends_with("xes") {
        word[..word.len() - 2].to_string()
    } else if word.ends_with("s") && !word.ends_with("ss") && !word.ends_with("us") {
        word[..word.len() - 1].to_string()
    } else {
        word.to_string()
    }
}

// ============================================================================
// Eloquent-like API Code Generation Helpers
// ============================================================================

/// Generate setter methods for the Model (used for updates)
fn generate_model_setters(columns: &[ColumnInfo]) -> String {
    columns
        .iter()
        .filter(|c| !c.is_primary_key && !is_timestamp_field(&c.name))
        .map(|col| {
            let rust_type = sql_type_to_rust_type(col);
            let setter_input_type = get_setter_input_type(&rust_type, col.is_nullable);

            format!(
                r#"    /// Set the {} field
    pub fn set_{field}(mut self, value: {input_type}) -> Self {{
        self.{field} = {assignment};
        self
    }}

"#,
                col.name,
                field = col.name,
                input_type = setter_input_type,
                assignment = get_setter_assignment(&rust_type, col.is_nullable),
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Generate builder struct fields
fn generate_builder_fields(columns: &[ColumnInfo]) -> String {
    columns
        .iter()
        .filter(|c| !c.is_primary_key)
        .map(|col| {
            let rust_type = sql_type_to_rust_type(col);
            format!("    {}: Option<{}>,", col.name, rust_type)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Generate setter methods for the Builder (used for creates)
fn generate_builder_setters(columns: &[ColumnInfo]) -> String {
    columns
        .iter()
        .filter(|c| !c.is_primary_key && !is_timestamp_field(&c.name))
        .map(|col| {
            let rust_type = sql_type_to_rust_type(col);
            let setter_input_type = get_builder_setter_input_type(&rust_type, col.is_nullable);

            format!(
                r#"    /// Set the {} field
    pub fn set_{field}(mut self, value: {input_type}) -> Self {{
        self.{field} = Some({builder_assignment});
        self
    }}

"#,
                col.name,
                field = col.name,
                input_type = setter_input_type,
                builder_assignment = get_builder_setter_assignment(&rust_type, col.is_nullable),
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Generate code to convert Builder to ActiveModel
fn generate_builder_to_active(columns: &[ColumnInfo]) -> String {
    let mut lines = vec!["        ActiveModel {".to_string()];

    for col in columns {
        if col.is_primary_key {
            lines.push(format!(
                "            {}: sea_orm::ActiveValue::NotSet,",
                col.name
            ));
        } else {
            lines.push(format!(
                "            {field}: self.{field}.map(Set).unwrap_or(sea_orm::ActiveValue::NotSet),",
                field = col.name
            ));
        }
    }

    lines.push("        }".to_string());
    lines.join("\n")
}

/// Generate code to convert Model to ActiveModel for updates
fn generate_model_to_active(columns: &[ColumnInfo]) -> String {
    let mut lines = vec!["        ActiveModel {".to_string()];

    for col in columns {
        let rust_type = sql_type_to_rust_type(col);
        let needs_clone = needs_clone_for_type(&rust_type);

        if needs_clone {
            lines.push(format!(
                "            {field}: Set(self.{field}.clone()),",
                field = col.name
            ));
        } else {
            lines.push(format!(
                "            {field}: Set(self.{field}),",
                field = col.name
            ));
        }
    }

    lines.push("        }".to_string());
    lines.join("\n")
}

/// Check if field is a timestamp field (auto-managed)
fn is_timestamp_field(name: &str) -> bool {
    matches!(name, "created_at" | "updated_at" | "deleted_at")
}

/// Get the input type for a setter method on Model
fn get_setter_input_type(rust_type: &str, is_nullable: bool) -> String {
    if is_nullable {
        // For Option<String>, accept Option<impl Into<String>>
        if rust_type == "Option<String>" {
            "Option<impl Into<String>>".to_string()
        } else {
            rust_type.to_string()
        }
    } else if rust_type == "String" {
        "impl Into<String>".to_string()
    } else {
        rust_type.to_string()
    }
}

/// Get the assignment expression for a setter on Model
fn get_setter_assignment(rust_type: &str, is_nullable: bool) -> String {
    if is_nullable {
        if rust_type == "Option<String>" {
            "value.map(|v| v.into())".to_string()
        } else {
            "value".to_string()
        }
    } else if rust_type == "String" {
        "value.into()".to_string()
    } else {
        "value".to_string()
    }
}

/// Get the input type for a builder setter method
fn get_builder_setter_input_type(rust_type: &str, is_nullable: bool) -> String {
    if is_nullable {
        // For nullable fields in builder, accept the inner type (not Option)
        if rust_type == "Option<String>" {
            "impl Into<String>".to_string()
        } else if rust_type.starts_with("Option<") && rust_type.ends_with(">") {
            // Extract inner type from Option<T>
            rust_type[7..rust_type.len() - 1].to_string()
        } else {
            rust_type.to_string()
        }
    } else if rust_type == "String" {
        "impl Into<String>".to_string()
    } else {
        rust_type.to_string()
    }
}

/// Get the assignment expression for a builder setter
fn get_builder_setter_assignment(rust_type: &str, is_nullable: bool) -> String {
    if is_nullable {
        // Wrap in Some for nullable fields
        if rust_type == "Option<String>" {
            "Some(value.into())".to_string()
        } else {
            "Some(value)".to_string()
        }
    } else if rust_type == "String" {
        "value.into()".to_string()
    } else {
        "value".to_string()
    }
}

/// Check if a type needs .clone() when converting
fn needs_clone_for_type(rust_type: &str) -> bool {
    // Types that implement Copy don't need clone
    let copy_types = [
        "i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128", "f32", "f64", "bool",
    ];

    // Check if it's a Copy type
    if copy_types.contains(&rust_type) {
        return false;
    }

    // Option<Copy> types also don't need clone
    for copy_type in &copy_types {
        if rust_type == format!("Option<{}>", copy_type) {
            return false;
        }
    }

    // Everything else needs clone (String, Option<String>, DateTimeUtc, etc.)
    true
}

// ============================================================================
// Docker Templates
// ============================================================================

/// Generate Dockerfile for production deployment
pub fn dockerfile_template(package_name: &str) -> String {
    include_str!("files/docker/Dockerfile.tpl").replace("{package_name}", package_name)
}

/// Generate .dockerignore file
pub fn dockerignore_template() -> &'static str {
    include_str!("files/docker/dockerignore.tpl")
}

/// Generate docker-compose.yml for local development
pub fn docker_compose_template(
    project_name: &str,
    include_mailpit: bool,
    include_minio: bool,
) -> String {
    let mailpit_service = if include_mailpit {
        include_str!("files/docker/mailpit.service.tpl").replace("{project_name}", project_name)
    } else {
        String::new()
    };

    let minio_service = if include_minio {
        include_str!("files/docker/minio.service.tpl").replace("{project_name}", project_name)
    } else {
        String::new()
    };

    let additional_volumes = if include_minio {
        "\n  minio_data:".to_string()
    } else {
        String::new()
    };

    include_str!("files/docker/docker-compose.yml.tpl")
        .replace("{project_name}", project_name)
        .replace("{mailpit_service}", &mailpit_service)
        .replace("{minio_service}", &minio_service)
        .replace("{additional_volumes}", &additional_volumes)
}

// ============================================================================
// Schedule Templates
// ============================================================================

/// Template for schedule.rs registration file
pub fn schedule_rs() -> &'static str {
    include_str!("files/backend/schedule.rs.tpl")
}

/// Template for tasks/mod.rs
pub fn tasks_mod() -> &'static str {
    include_str!("files/backend/tasks/mod.rs.tpl")
}

// schedule_bin removed - scheduler now integrated into main binary

/// Template for generating new scheduled task with make:task command.
///
/// Emits a real, working `Task` impl that logs a structured start/finish
/// event. The skeleton runs cleanly the first time the scheduler invokes
/// it; users replace the body with their actual job (cleanup, reminders,
/// nightly aggregates, etc).
pub fn task_template(file_name: &str, struct_name: &str) -> String {
    format!(
        r#"//! {struct_name} scheduled task
//!
//! Created with `suprnova make:task {file_name}`.

use std::time::Instant;

use async_trait::async_trait;
use suprnova::{{Task, TaskResult}};

/// {struct_name} - A scheduled task.
///
/// Register the task in `src/schedule.rs` with the fluent API; the
/// skeleton below times its own run and prints a structured log line on
/// each invocation so it works end-to-end the first time you wire it up.
///
/// # Example Registration
///
/// ```rust,ignore
/// // In src/schedule.rs
/// use crate::tasks::{file_name};
///
/// schedule.add(
///     schedule.task({struct_name}::new())
///         .daily()
///         .at("03:00")
///         .name("{file_name}")
///         .description("{struct_name} scheduled task")
/// );
/// ```
pub struct {struct_name};

impl {struct_name} {{
    /// Create a new instance of this task.
    pub fn new() -> Self {{
        Self
    }}
}}

impl Default for {struct_name} {{
    fn default() -> Self {{
        Self::new()
    }}
}}

#[async_trait]
impl Task for {struct_name} {{
    async fn handle(&self) -> TaskResult {{
        let started_at = Instant::now();
        println!("[{struct_name}] task started");

        // Replace this with the real job. The skeleton ships as a
        // no-op success so the task can be scheduled and observed
        // before the implementation is filled in.

        println!(
            "[{struct_name}] task finished in {{}} ms",
            started_at.elapsed().as_millis(),
        );
        Ok(())
    }}
}}
"#,
        file_name = file_name,
        struct_name = struct_name
    )
}
