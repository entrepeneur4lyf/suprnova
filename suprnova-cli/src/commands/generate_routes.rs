//! Route generation for type-safe frontend integration
//!
//! Generates TypeScript route helpers compatible with Inertia.js v2+ UrlMethodPair interface.
//! This allows type-safe navigation with:
//! - `router.visit(controllers.user.show({ id: '123' }))`
//! - `form.submit(controllers.todo.store({ title: 'Task', completed: false }))`
//! - `<Link href={controllers.user.index()}>Users</Link>`

use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use syn::visit::Visit;
use syn::{Attribute, Fields, FnArg, ItemFn, ItemStruct, Type};
use walkdir::WalkDir;

use crate::ui;

/// HTTP methods for routes
#[derive(Debug, Clone, PartialEq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "get" => Some(HttpMethod::Get),
            "post" => Some(HttpMethod::Post),
            "put" => Some(HttpMethod::Put),
            "patch" => Some(HttpMethod::Patch),
            "delete" => Some(HttpMethod::Delete),
            _ => None,
        }
    }

    fn to_ts_method(&self) -> &'static str {
        match self {
            HttpMethod::Get => "get",
            HttpMethod::Post => "post",
            HttpMethod::Put => "put",
            HttpMethod::Patch => "patch",
            HttpMethod::Delete => "delete",
        }
    }
}

/// A path parameter extracted from route patterns like /users/{id}
#[derive(Debug, Clone)]
pub struct PathParam {
    pub name: String,
}

/// A parsed route definition from routes.rs
#[derive(Debug, Clone)]
pub struct RouteDefinition {
    pub method: HttpMethod,
    pub path: String,
    pub handler_module: String, // e.g., "controllers::user"
    pub handler_fn: String,     // e.g., "show"
    pub name: Option<String>,   // e.g., "users.show"
    pub path_params: Vec<PathParam>,
}

/// Information about a handler function
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HandlerInfo {
    pub name: String,
    pub has_handler_attr: bool,
    pub request_type: Option<String>,
}

/// A form request struct definition
#[derive(Debug, Clone)]
pub struct FormRequestStruct {
    pub name: String,
    pub fields: Vec<FormRequestField>,
}

#[derive(Debug, Clone)]
pub struct FormRequestField {
    pub name: String,
    pub ty: RustType,
}

/// Rust type representation for TypeScript conversion
#[derive(Debug, Clone)]
pub enum RustType {
    String,
    Number,
    Bool,
    Option(Box<RustType>),
    Vec(Box<RustType>),
    Custom(String),
}

/// A complete route ready for TypeScript generation
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct GeneratedRoute {
    pub definition: RouteDefinition,
    pub handler_info: Option<HandlerInfo>,
    pub request_struct: Option<FormRequestStruct>,
}

/// Parse routes.rs file content and extract route definitions
pub fn parse_routes_file(content: &str) -> Vec<RouteDefinition> {
    let mut routes = Vec::new();

    // Pattern to match route definitions like:
    // get!("/path", controllers::module::function).name("route.name")
    // post!("/path/{id}", controllers::module::function)
    let route_pattern = Regex::new(
        r#"(get|post|put|patch|delete)!\s*\(\s*"([^"]+)"\s*,\s*([a-zA-Z_][a-zA-Z0-9_:]*)\s*\)(?:\s*\.name\s*\(\s*"([^"]+)"\s*\))?"#
    ).unwrap();

    // Pattern to extract path parameters like {id}
    let param_pattern = Regex::new(r#"\{(\w+)\}"#).unwrap();

    for cap in route_pattern.captures_iter(content) {
        let method_str = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let path = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let handler_path = cap.get(3).map(|m| m.as_str()).unwrap_or("");
        let name = cap.get(4).map(|m| m.as_str().to_string());

        let method = match HttpMethod::from_str(method_str) {
            Some(m) => m,
            None => continue,
        };

        // Parse handler path: controllers::user::show -> (controllers::user, show)
        let parts: Vec<&str> = handler_path.rsplitn(2, "::").collect();
        let (handler_fn, handler_module) = if parts.len() == 2 {
            (parts[0].to_string(), parts[1].to_string())
        } else {
            continue;
        };

        // Extract path parameters
        let path_params: Vec<PathParam> = param_pattern
            .captures_iter(path)
            .filter_map(|cap| {
                cap.get(1).map(|m| PathParam {
                    name: m.as_str().to_string(),
                })
            })
            .collect();

        routes.push(RouteDefinition {
            method,
            path: path.to_string(),
            handler_module,
            handler_fn,
            name,
            path_params,
        });
    }

    routes
}

/// Visitor that collects handler functions with #[handler] attribute
struct HandlerVisitor {
    handlers: Vec<HandlerInfo>,
}

impl HandlerVisitor {
    fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    fn has_handler_attr(&self, attrs: &[Attribute]) -> bool {
        attrs.iter().any(|attr| attr.path().is_ident("handler"))
    }

    fn extract_request_type(&self, func: &ItemFn) -> Option<String> {
        // Get the first parameter's type
        if let Some(FnArg::Typed(pat_type)) = func.sig.inputs.first() {
            return self.type_to_string(&pat_type.ty);
        }
        None
    }

    fn type_to_string(&self, ty: &Type) -> Option<String> {
        match ty {
            Type::Path(type_path) => {
                let segments: Vec<String> = type_path
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect();

                let type_name = segments.last()?.clone();

                // Skip if it's Request type (not a form request)
                if type_name == "Request" {
                    return None;
                }

                Some(type_name)
            }
            _ => None,
        }
    }
}

impl<'ast> Visit<'ast> for HandlerVisitor {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let has_handler = self.has_handler_attr(&node.attrs);
        let request_type = if has_handler {
            self.extract_request_type(node)
        } else {
            None
        };

        self.handlers.push(HandlerInfo {
            name: node.sig.ident.to_string(),
            has_handler_attr: has_handler,
            request_type,
        });

        syn::visit::visit_item_fn(self, node);
    }
}

/// Visitor that collects #[form_request] structs
struct FormRequestVisitor {
    structs: Vec<FormRequestStruct>,
}

impl FormRequestVisitor {
    fn new() -> Self {
        Self {
            structs: Vec::new(),
        }
    }

    fn has_form_request_attr(&self, attrs: &[Attribute]) -> bool {
        for attr in attrs {
            // Check for #[form_request]
            if attr.path().is_ident("form_request") {
                return true;
            }
            // Check for #[derive(FormRequest)]
            if attr.path().is_ident("derive") {
                if let Ok(nested) = attr.parse_args_with(
                    syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
                ) {
                    for path in nested {
                        if path.is_ident("FormRequest") {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    fn parse_type(&self, ty: &Type) -> RustType {
        match ty {
            Type::Path(type_path) => {
                let segment = type_path.path.segments.last().unwrap();
                let ident = segment.ident.to_string();

                match ident.as_str() {
                    "String" | "str" => RustType::String,
                    "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32"
                    | "u64" | "u128" | "usize" | "f32" | "f64" => RustType::Number,
                    "bool" => RustType::Bool,
                    "Option" => {
                        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                            if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                                return RustType::Option(Box::new(self.parse_type(inner_ty)));
                            }
                        }
                        RustType::Option(Box::new(RustType::Custom("unknown".to_string())))
                    }
                    "Vec" => {
                        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                            if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                                return RustType::Vec(Box::new(self.parse_type(inner_ty)));
                            }
                        }
                        RustType::Vec(Box::new(RustType::Custom("unknown".to_string())))
                    }
                    other => RustType::Custom(other.to_string()),
                }
            }
            Type::Reference(type_ref) => {
                if let Type::Path(inner) = &*type_ref.elem {
                    if inner
                        .path
                        .segments
                        .last()
                        .map(|s| s.ident == "str")
                        .unwrap_or(false)
                    {
                        return RustType::String;
                    }
                }
                self.parse_type(&type_ref.elem)
            }
            _ => RustType::Custom("unknown".to_string()),
        }
    }
}

impl<'ast> Visit<'ast> for FormRequestVisitor {
    fn visit_item_struct(&mut self, node: &'ast ItemStruct) {
        if self.has_form_request_attr(&node.attrs) {
            let name = node.ident.to_string();

            let fields = match &node.fields {
                Fields::Named(named) => named
                    .named
                    .iter()
                    .filter_map(|f| {
                        f.ident.as_ref().map(|ident| FormRequestField {
                            name: ident.to_string(),
                            ty: self.parse_type(&f.ty),
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            };

            self.structs.push(FormRequestStruct { name, fields });
        }

        syn::visit::visit_item_struct(self, node);
    }
}

/// Scan a controller file for handler functions
fn scan_controller_handlers(content: &str) -> Vec<HandlerInfo> {
    if let Ok(syntax) = syn::parse_file(content) {
        let mut visitor = HandlerVisitor::new();
        visitor.visit_file(&syntax);
        return visitor.handlers;
    }
    Vec::new()
}

/// Scan all Rust files for FormRequest structs
fn scan_form_requests(project_path: &Path) -> HashMap<String, FormRequestStruct> {
    let src_path = project_path.join("src");
    let mut form_requests = HashMap::new();

    for entry in WalkDir::new(&src_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "rs").unwrap_or(false))
    {
        if let Ok(content) = fs::read_to_string(entry.path()) {
            if let Ok(syntax) = syn::parse_file(&content) {
                let mut visitor = FormRequestVisitor::new();
                visitor.visit_file(&syntax);
                for s in visitor.structs {
                    form_requests.insert(s.name.clone(), s);
                }
            }
        }
    }

    form_requests
}

/// Resolve handler module to file path
/// e.g., "controllers::user" -> "src/controllers/user.rs"
fn resolve_module_to_file(project_path: &Path, module_path: &str) -> Option<std::path::PathBuf> {
    let parts: Vec<&str> = module_path.split("::").collect();
    if parts.is_empty() {
        return None;
    }

    // Try as a file directly: src/controllers/user.rs
    let file_path = project_path
        .join("src")
        .join(parts.join("/"))
        .with_extension("rs");
    if file_path.exists() {
        return Some(file_path);
    }

    // Try as module folder: src/controllers/user/mod.rs
    let mod_path = project_path
        .join("src")
        .join(parts.join("/"))
        .join("mod.rs");
    if mod_path.exists() {
        return Some(mod_path);
    }

    None
}

/// Scan routes and handlers to build GeneratedRoute list
pub fn scan_routes(project_path: &Path) -> Result<Vec<GeneratedRoute>, String> {
    // Read routes.rs
    let routes_file = project_path.join("src/routes.rs");
    if !routes_file.exists() {
        return Err("src/routes.rs not found".to_string());
    }

    let routes_content =
        fs::read_to_string(&routes_file).map_err(|e| format!("Failed to read routes.rs: {}", e))?;

    let route_definitions = parse_routes_file(&routes_content);

    // Scan all form requests
    let form_requests = scan_form_requests(project_path);

    // Process each route
    let mut generated_routes = Vec::new();

    for def in route_definitions {
        // Try to find the handler
        let handler_info = if let Some(controller_file) =
            resolve_module_to_file(project_path, &def.handler_module)
        {
            if let Ok(content) = fs::read_to_string(&controller_file) {
                let handlers = scan_controller_handlers(&content);
                handlers.into_iter().find(|h| h.name == def.handler_fn)
            } else {
                None
            }
        } else {
            None
        };

        // Find the form request struct if the handler has one
        let request_struct = handler_info
            .as_ref()
            .and_then(|h| h.request_type.as_ref())
            .and_then(|type_name| form_requests.get(type_name).cloned());

        generated_routes.push(GeneratedRoute {
            definition: def,
            handler_info,
            request_struct,
        });
    }

    Ok(generated_routes)
}

/// Convert RustType to TypeScript type string
fn rust_type_to_ts(ty: &RustType) -> String {
    match ty {
        RustType::String => "string".to_string(),
        RustType::Number => "number".to_string(),
        RustType::Bool => "boolean".to_string(),
        RustType::Option(inner) => format!("{} | null", rust_type_to_ts(inner)),
        RustType::Vec(inner) => format!("{}[]", rust_type_to_ts(inner)),
        RustType::Custom(name) => name.clone(),
    }
}

/// Generate TypeScript routes file
pub fn generate_typescript(routes: &[GeneratedRoute]) -> String {
    let mut output = String::new();

    output.push_str("// This file is auto-generated by Suprnova. Do not edit manually.\n");
    output.push_str("// Run `suprnova generate-types` to regenerate.\n");
    output.push_str("// Compatible with Inertia.js v2+ UrlMethodPair interface\n\n");

    output.push_str("import type { Method } from '@inertiajs/core';\n\n");

    // RouteConfig interface
    output.push_str("// Route configuration - compatible with Inertia's UrlMethodPair\n");
    output.push_str("export interface RouteConfig<TData = void> {\n");
    output.push_str("  url: string;\n");
    output.push_str("  method: Method;  // 'get' | 'post' | 'put' | 'patch' | 'delete'\n");
    output.push_str("  data?: TData;\n");
    output.push_str("}\n\n");

    // Collect all unique form request types
    let mut form_request_types: Vec<&FormRequestStruct> = routes
        .iter()
        .filter_map(|r| r.request_struct.as_ref())
        .collect();
    form_request_types.sort_by(|a, b| a.name.cmp(&b.name));
    form_request_types.dedup_by(|a, b| a.name == b.name);

    // Generate request type interfaces
    if !form_request_types.is_empty() {
        output.push_str("// Request types (from #[form_request] structs)\n");
        for form_req in &form_request_types {
            output.push_str(&format!("export interface {} {{\n", form_req.name));
            for field in &form_req.fields {
                let ts_type = rust_type_to_ts(&field.ty);
                output.push_str(&format!("  {}: {};\n", field.name, ts_type));
            }
            output.push_str("}\n\n");
        }
    }

    // Collect all path param types
    let routes_with_params: Vec<&GeneratedRoute> = routes
        .iter()
        .filter(|r| !r.definition.path_params.is_empty())
        .collect();

    if !routes_with_params.is_empty() {
        output.push_str("// Path parameter types\n");
        for route in &routes_with_params {
            let interface_name = generate_params_interface_name(route);
            output.push_str(&format!("export interface {} {{\n", interface_name));
            for param in &route.definition.path_params {
                output.push_str(&format!("  {}: string;\n", param.name));
            }
            output.push_str("}\n\n");
        }
    }

    // Group routes by module (first part of handler_module after "controllers::")
    let mut modules: HashMap<String, Vec<&GeneratedRoute>> = HashMap::new();
    for route in routes {
        let module_name = extract_controller_name(&route.definition.handler_module);
        modules.entry(module_name).or_default().push(route);
    }

    // Generate controllers object
    output.push_str("// Controller namespace - mirrors backend structure\n");
    output.push_str("export const controllers = {\n");

    let mut module_names: Vec<&String> = modules.keys().collect();
    module_names.sort();

    for (i, module_name) in module_names.iter().enumerate() {
        let module_routes = modules.get(*module_name).unwrap();
        output.push_str(&format!("  {}: {{\n", module_name));

        // Track used function names to handle duplicates
        let mut used_names: HashMap<String, usize> = HashMap::new();

        for (j, route) in module_routes.iter().enumerate() {
            // Generate unique function name for duplicate handlers
            let base_fn_name = &route.definition.handler_fn;
            let fn_name = if let Some(count) = used_names.get(base_fn_name) {
                // Use route name or path segment to make unique
                if let Some(name) = &route.definition.name {
                    // Use the last part of the route name: "home" from "home", "protected" from name
                    name.split('.').last().unwrap_or(base_fn_name).to_string()
                } else {
                    // Use path to create unique name
                    let path_name = route
                        .definition
                        .path
                        .trim_start_matches('/')
                        .replace(['/', '{', '}', '-'], "_");
                    if path_name.is_empty() {
                        format!("{}_{}", base_fn_name, count + 1)
                    } else {
                        path_name
                    }
                }
            } else {
                base_fn_name.clone()
            };
            *used_names.entry(base_fn_name.clone()).or_insert(0) += 1;

            let method = route.definition.method.to_ts_method();
            let has_params = !route.definition.path_params.is_empty();
            let has_data = route.request_struct.is_some();

            // Determine function signature
            let (params_signature, return_type) = if has_params && has_data {
                let params_type = generate_params_interface_name(route);
                let data_type = route.request_struct.as_ref().unwrap().name.clone();
                (
                    format!("params: {}, data: {}", params_type, data_type),
                    format!("RouteConfig<{}>", data_type),
                )
            } else if has_params {
                let params_type = generate_params_interface_name(route);
                (
                    format!("params: {}", params_type),
                    "RouteConfig".to_string(),
                )
            } else if has_data {
                let data_type = route.request_struct.as_ref().unwrap().name.clone();
                (
                    format!("data: {}", data_type),
                    format!("RouteConfig<{}>", data_type),
                )
            } else {
                (String::new(), "RouteConfig".to_string())
            };

            // Generate URL with params interpolation
            let url = if has_params {
                generate_url_with_params(&route.definition.path)
            } else {
                format!("'{}'", route.definition.path)
            };

            // Generate the function body
            let data_prop = if has_data { ", data" } else { "" };

            let comma = if j < module_routes.len() - 1 { "," } else { "" };
            output.push_str(&format!(
                "    {}: ({}): {} => ({{ url: {}, method: '{}'{} }}){}\n",
                fn_name, params_signature, return_type, url, method, data_prop, comma
            ));
        }

        let comma = if i < module_names.len() - 1 { "," } else { "" };
        output.push_str(&format!("  }}{}\n", comma));
    }

    output.push_str("} as const;\n\n");

    // Generate named routes lookup
    let named_routes: Vec<&GeneratedRoute> = routes
        .iter()
        .filter(|r| r.definition.name.is_some())
        .collect();

    if !named_routes.is_empty() {
        output.push_str("// Named routes lookup\n");
        output.push_str("export const routes = {\n");

        for (i, route) in named_routes.iter().enumerate() {
            let name = route.definition.name.as_ref().unwrap();
            let module = extract_controller_name(&route.definition.handler_module);
            let fn_name = &route.definition.handler_fn;
            let comma = if i < named_routes.len() - 1 { "," } else { "" };
            output.push_str(&format!(
                "  '{}': controllers.{}.{}{}\n",
                name, module, fn_name, comma
            ));
        }

        output.push_str("} as const;\n");
    }

    output
}

/// Generate params interface name from route
fn generate_params_interface_name(route: &GeneratedRoute) -> String {
    // Convert handler to PascalCase: user::show -> UserShowParams
    let module = extract_controller_name(&route.definition.handler_module);
    let fn_name = &route.definition.handler_fn;
    format!(
        "{}{}Params",
        to_pascal_case(&module),
        to_pascal_case(fn_name)
    )
}

/// Convert snake_case to PascalCase
fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect()
}

/// Extract controller name from module path
/// e.g., "controllers::user" -> "user"
fn extract_controller_name(module_path: &str) -> String {
    module_path
        .split("::")
        .last()
        .unwrap_or("unknown")
        .to_string()
}

/// Generate URL template string with params interpolation
fn generate_url_with_params(path: &str) -> String {
    // Manually replace {param} with ${params.param} for JS template literals
    let param_pattern = Regex::new(r#"\{(\w+)\}"#).unwrap();
    let mut result = path.to_string();

    for cap in param_pattern.captures_iter(path) {
        let full_match = cap.get(0).unwrap().as_str();
        let param_name = cap.get(1).unwrap().as_str();
        result = result.replace(full_match, &format!("${{params.{}}}", param_name));
    }

    format!("`{}`", result)
}

/// Generate routes and write to the output file
pub fn generate_routes_to_file(project_path: &Path, output_path: &Path) -> Result<usize, String> {
    let routes = scan_routes(project_path)?;

    if routes.is_empty() {
        return Ok(0);
    }

    // Ensure output directory exists
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create output directory: {}", e))?;
    }

    let typescript = generate_typescript(&routes);
    fs::write(output_path, typescript)
        .map_err(|e| format!("Failed to write TypeScript file: {}", e))?;

    Ok(routes.len())
}

/// Main entry point for route generation (standalone use)
#[allow(dead_code)]
pub fn run(output: Option<String>) {
    let project_path = Path::new(".");

    // Validate Suprnova project
    let cargo_toml = project_path.join("Cargo.toml");
    if !cargo_toml.exists() {
        ui::error("Not a Suprnova project (no Cargo.toml found)");
        std::process::exit(1);
    }

    let output_path = output
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| project_path.join("frontend/src/types/routes.ts"));

    ui::info("Scanning routes for type-safe generation...");

    match generate_routes_to_file(project_path, &output_path) {
        Ok(0) => {
            ui::warning("No routes found in src/routes.rs");
        }
        Ok(count) => {
            ui::info(&format!("Found {} route(s)", count));
            ui::success(&format!("Generated {}", output_path.display()));
        }
        Err(e) => {
            ui::error(&e);
            std::process::exit(1);
        }
    }
}
