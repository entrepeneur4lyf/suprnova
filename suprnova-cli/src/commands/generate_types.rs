use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use syn::visit::Visit;
use syn::{Attribute, Fields, GenericArgument, ItemStruct, PathArguments, Type};
use walkdir::WalkDir;

use crate::ui;

/// Represents a parsed InertiaProps/Data struct
#[derive(Debug, Clone)]
pub struct InertiaPropsStruct {
    pub name: String,
    /// Generic type parameter names (e.g. `["T"]` for `struct Foo<T>`).
    pub type_params: Vec<String>,
    pub fields: Vec<StructField>,
}

/// Flags derived from `#[data(...)]` field attributes.
#[derive(Debug, Clone, Default)]
pub struct DataFieldFlags {
    /// Field is only sent from client → server (excluded from output type)
    pub input_only: bool,
    /// Field is only sent from server → client (excluded from input type)
    pub output_only: bool,
    /// Runtime-only opt-in for sparse fieldsets; no TS effect
    pub allow_include: bool,
    /// Lazily-loaded prop; treated as output-only for TS purposes
    pub lazy: bool,
}

#[derive(Debug, Clone)]
pub struct StructField {
    pub name: String,
    pub ty: RustType,
    pub data_flags: DataFieldFlags,
}

#[derive(Debug, Clone)]
pub enum RustType {
    String,
    Number,
    Bool,
    Option(Box<RustType>),
    Vec(Box<RustType>),
    HashMap(Box<RustType>, Box<RustType>),
    /// `Field<T>` — serialises as `T | null`; optional on the wire
    Field(Box<RustType>),
    /// `Prop<T>` — deferred/lazy prop; optional, never null
    Prop(Box<RustType>),
    Custom(String),
}

/// Visitor that collects structs with #[derive(InertiaProps)] or #[derive(Data)]
struct InertiaPropsVisitor {
    structs: Vec<InertiaPropsStruct>,
}

impl InertiaPropsVisitor {
    fn new() -> Self {
        Self {
            structs: Vec::new(),
        }
    }

    fn has_inertia_props_derive(&self, attrs: &[Attribute]) -> bool {
        for attr in attrs {
            if attr.path().is_ident("derive")
                && let Ok(nested) = attr.parse_args_with(
                    syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
                )
            {
                for path in nested {
                    if path.is_ident("InertiaProps") {
                        return true;
                    }
                    // Also check for suprnova::InertiaProps
                    if path.segments.len() == 2 {
                        let first = &path.segments[0].ident;
                        let second = &path.segments[1].ident;
                        if first == "suprnova" && second == "InertiaProps" {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    fn has_data_derive(&self, attrs: &[Attribute]) -> bool {
        for attr in attrs {
            if attr.path().is_ident("derive")
                && let Ok(nested) = attr.parse_args_with(
                    syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
                )
            {
                for path in nested {
                    if path.is_ident("Data") {
                        return true;
                    }
                    // Also check for suprnova::Data
                    if path.segments.len() == 2 {
                        let first = &path.segments[0].ident;
                        let second = &path.segments[1].ident;
                        if first == "suprnova" && second == "Data" {
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
                        if let PathArguments::AngleBracketed(args) = &segment.arguments
                            && let Some(GenericArgument::Type(inner_ty)) = args.args.first()
                        {
                            return RustType::Option(Box::new(self.parse_type(inner_ty)));
                        }
                        RustType::Option(Box::new(RustType::Custom("unknown".to_string())))
                    }
                    "Vec" => {
                        if let PathArguments::AngleBracketed(args) = &segment.arguments
                            && let Some(GenericArgument::Type(inner_ty)) = args.args.first()
                        {
                            return RustType::Vec(Box::new(self.parse_type(inner_ty)));
                        }
                        RustType::Vec(Box::new(RustType::Custom("unknown".to_string())))
                    }
                    "HashMap" | "BTreeMap" => {
                        if let PathArguments::AngleBracketed(args) = &segment.arguments {
                            let mut iter = args.args.iter();
                            if let (
                                Some(GenericArgument::Type(key_ty)),
                                Some(GenericArgument::Type(val_ty)),
                            ) = (iter.next(), iter.next())
                            {
                                return RustType::HashMap(
                                    Box::new(self.parse_type(key_ty)),
                                    Box::new(self.parse_type(val_ty)),
                                );
                            }
                        }
                        RustType::HashMap(
                            Box::new(RustType::String),
                            Box::new(RustType::Custom("unknown".to_string())),
                        )
                    }
                    "Field" => {
                        if let PathArguments::AngleBracketed(args) = &segment.arguments
                            && let Some(GenericArgument::Type(inner_ty)) = args.args.first()
                        {
                            return RustType::Field(Box::new(self.parse_type(inner_ty)));
                        }
                        RustType::Field(Box::new(RustType::Custom("unknown".to_string())))
                    }
                    "Prop" => {
                        if let PathArguments::AngleBracketed(args) = &segment.arguments
                            && let Some(GenericArgument::Type(inner_ty)) = args.args.first()
                        {
                            return RustType::Prop(Box::new(self.parse_type(inner_ty)));
                        }
                        RustType::Prop(Box::new(RustType::Custom("unknown".to_string())))
                    }
                    other => RustType::Custom(other.to_string()),
                }
            }
            Type::Reference(type_ref) => {
                // Handle &str as String
                if let Type::Path(inner) = &*type_ref.elem
                    && inner
                        .path
                        .segments
                        .last()
                        .map(|s| s.ident == "str")
                        .unwrap_or(false)
                {
                    return RustType::String;
                }
                self.parse_type(&type_ref.elem)
            }
            _ => RustType::Custom("unknown".to_string()),
        }
    }
}

/// Parse `#[data(...)]` attributes on a field into `DataFieldFlags`.
fn parse_data_flags(attrs: &[Attribute]) -> DataFieldFlags {
    let mut flags = DataFieldFlags::default();
    for attr in attrs {
        if !attr.path().is_ident("data") {
            continue;
        }
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("input_only") {
                flags.input_only = true;
            } else if meta.path.is_ident("output_only") {
                flags.output_only = true;
            } else if meta.path.is_ident("allow_include") {
                flags.allow_include = true;
            } else if meta.path.is_ident("lazy") {
                flags.lazy = true;
            }
            Ok(())
        });
    }
    flags
}

impl<'ast> Visit<'ast> for InertiaPropsVisitor {
    fn visit_item_struct(&mut self, node: &'ast ItemStruct) {
        if self.has_inertia_props_derive(&node.attrs) || self.has_data_derive(&node.attrs) {
            let name = node.ident.to_string();

            let type_params: Vec<String> = node
                .generics
                .type_params()
                .map(|tp| tp.ident.to_string())
                .collect();

            let fields = match &node.fields {
                Fields::Named(named) => named
                    .named
                    .iter()
                    .filter_map(|f| {
                        f.ident.as_ref().map(|ident| StructField {
                            name: ident.to_string(),
                            ty: self.parse_type(&f.ty),
                            data_flags: parse_data_flags(&f.attrs),
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            };

            self.structs.push(InertiaPropsStruct {
                name,
                type_params,
                fields,
            });
        }

        // Continue visiting nested items
        syn::visit::visit_item_struct(self, node);
    }
}

/// Scan all Rust files in the src directory for InertiaProps/Data structs
pub fn scan_inertia_props(project_path: &Path) -> Vec<InertiaPropsStruct> {
    let src_path = project_path.join("src");
    let mut all_structs = Vec::new();
    visit_path_into(&src_path, &mut all_structs);
    all_structs
}

/// Walk a directory tree and collect all InertiaProps/Data structs into `out`.
fn visit_path_into(root: &Path, out: &mut Vec<InertiaPropsStruct>) {
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "rs").unwrap_or(false))
    {
        if let Ok(content) = fs::read_to_string(entry.path())
            && let Ok(syntax) = syn::parse_file(&content)
        {
            let mut visitor = InertiaPropsVisitor::new();
            visitor.visit_file(&syntax);
            out.extend(visitor.structs);
        }
    }
}

/// Convert a RustType to TypeScript type string
fn rust_type_to_ts(ty: &RustType) -> String {
    match ty {
        RustType::String => "string".to_string(),
        RustType::Number => "number".to_string(),
        RustType::Bool => "boolean".to_string(),
        RustType::Option(inner) => format!("{} | null", rust_type_to_ts(inner)),
        RustType::Vec(inner) => format!("Array<{}>", rust_type_to_ts(inner)),
        RustType::HashMap(key, val) => {
            format!("Record<{}, {}>", rust_type_to_ts(key), rust_type_to_ts(val))
        }
        RustType::Field(inner) => format!("{} | null", rust_type_to_ts(inner)),
        RustType::Prop(inner) => rust_type_to_ts(inner),
        RustType::Custom(name) => name.clone(),
    }
}

/// Return the optional marker for a field's TS declaration.
/// `Field<T>` and `Prop<T>` are optional (may be absent on the wire).
fn optional_marker(ty: &RustType) -> &'static str {
    match ty {
        RustType::Field(_) | RustType::Prop(_) => "?",
        _ => "",
    }
}

/// Sort structs topologically so dependencies come first
fn topological_sort(structs: &[InertiaPropsStruct]) -> Vec<&InertiaPropsStruct> {
    let struct_map: HashMap<_, _> = structs.iter().map(|s| (s.name.clone(), s)).collect();
    let struct_names: HashSet<_> = structs.iter().map(|s| s.name.clone()).collect();

    // Build dependency graph
    let mut deps: HashMap<String, HashSet<String>> = HashMap::new();
    for s in structs {
        let mut s_deps = HashSet::new();
        for field in &s.fields {
            collect_type_deps(&field.ty, &mut s_deps, &struct_names);
        }
        deps.insert(s.name.clone(), s_deps);
    }

    // Kahn's algorithm for topological sort
    let mut in_degree: HashMap<String, usize> =
        struct_names.iter().map(|n| (n.clone(), 0)).collect();
    for s_deps in deps.values() {
        for dep in s_deps {
            if let Some(count) = in_degree.get_mut(dep) {
                *count += 1;
            }
        }
    }

    let mut queue: Vec<_> = in_degree
        .iter()
        .filter(|&(_, &count)| count == 0)
        .map(|(name, _)| name.clone())
        .collect();
    let mut result = Vec::new();

    while let Some(name) = queue.pop() {
        if let Some(s) = struct_map.get(&name) {
            result.push(*s);
        }
        if let Some(s_deps) = deps.get(&name) {
            for dep in s_deps {
                if let Some(count) = in_degree.get_mut(dep) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        queue.push(dep.clone());
                    }
                }
            }
        }
    }

    result
}

fn collect_type_deps(ty: &RustType, deps: &mut HashSet<String>, known: &HashSet<String>) {
    match ty {
        RustType::Custom(name) if known.contains(name) => {
            deps.insert(name.clone());
        }
        RustType::Option(inner) | RustType::Vec(inner) => {
            collect_type_deps(inner, deps, known);
        }
        RustType::Field(inner) | RustType::Prop(inner) => {
            collect_type_deps(inner, deps, known);
        }
        RustType::HashMap(key, val) => {
            collect_type_deps(key, deps, known);
            collect_type_deps(val, deps, known);
        }
        _ => {}
    }
}

/// Emit paired output + (optionally) input TypeScript interfaces for one struct.
///
/// A paired `<Name>Input` interface is emitted whenever any field carries an
/// `input_only`, `output_only`, or `lazy` flag — i.e. whenever the input and
/// output shapes differ.
fn emit_ts_for_struct(s: &InertiaPropsStruct) -> String {
    let has_flags = s
        .fields
        .iter()
        .any(|f| f.data_flags.input_only || f.data_flags.output_only || f.data_flags.lazy);

    // Build generic type parameter suffix, e.g. "<T>" or "<A, B>" or "".
    let generics = if s.type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", s.type_params.join(", "))
    };

    let mut out = String::new();

    // Output interface — what the frontend RECEIVES
    out.push_str(&format!("export interface {}{} {{\n", s.name, generics));
    for f in s.fields.iter().filter(|f| !f.data_flags.input_only) {
        out.push_str(&format!(
            "  {}{}: {};\n",
            f.name,
            optional_marker(&f.ty),
            rust_type_to_ts(&f.ty)
        ));
    }
    out.push_str("}\n\n");

    // Input interface — what the frontend SENDS (only when shapes differ)
    if has_flags {
        out.push_str(&format!(
            "export interface {}Input{} {{\n",
            s.name, generics
        ));
        // Exclude output_only AND lazy fields (lazy props are output-only in nature)
        for f in s
            .fields
            .iter()
            .filter(|f| !f.data_flags.output_only && !f.data_flags.lazy)
        {
            out.push_str(&format!(
                "  {}{}: {};\n",
                f.name,
                optional_marker(&f.ty),
                rust_type_to_ts(&f.ty)
            ));
        }
        out.push_str("}\n\n");
    }

    out
}

/// Generate TypeScript interfaces from the structs.
///
/// This is the canonical emission path; both the file-write entry point and
/// the in-memory `generate_types_string` helper call through here.
pub fn generate_typescript(structs: &[InertiaPropsStruct]) -> String {
    let sorted = topological_sort(structs);

    let mut output = String::new();
    output.push_str("// This file is auto-generated by Suprnova. Do not edit manually.\n");
    output.push_str("// Run `suprnova generate-types` to regenerate.\n\n");

    for s in sorted {
        output.push_str(&emit_ts_for_struct(s));
    }

    output
}

/// Input source for `generate_types_string`.
// Used exclusively from integration tests (suprnova-cli/tests/), which are
// separate compilation units invisible to the dead_code lint on the binary target.
#[allow(dead_code)]
pub enum ScanInput {
    /// Parse a single Rust source string (for testing without a filesystem walk).
    Source(&'static str),
    /// Walk a directory tree (production code path).
    Walk(std::path::PathBuf),
}

/// Generate TypeScript type declarations from a given source, returning the
/// result as a `String` without writing to disk.
///
/// Both the test harness and `generate_types_to_file` delegate here so that
/// a single emission path is always exercised.
// Used exclusively from integration tests (suprnova-cli/tests/), which are
// separate compilation units invisible to the dead_code lint on the binary target.
#[allow(dead_code)]
pub fn generate_types_string(input: ScanInput) -> String {
    let structs: Vec<InertiaPropsStruct> = match input {
        ScanInput::Source(src) => {
            let syntax = syn::parse_file(src).expect("ScanInput::Source: invalid Rust");
            let mut visitor = InertiaPropsVisitor::new();
            visitor.visit_file(&syntax);
            visitor.structs
        }
        ScanInput::Walk(root) => {
            let mut out = Vec::new();
            visit_path_into(&root, &mut out);
            out
        }
    };

    // Emit without the file-level header comment so tests get clean output.
    let sorted = topological_sort(&structs);
    let mut output = String::new();
    for s in sorted {
        output.push_str(&emit_ts_for_struct(s));
    }
    output
}

/// Generate types and write to the output file
pub fn generate_types_to_file(project_path: &Path, output_path: &Path) -> Result<usize, String> {
    let structs = scan_inertia_props(project_path);

    if structs.is_empty() {
        return Ok(0);
    }

    // Ensure output directory exists
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create output directory: {}", e))?;
    }

    let typescript = generate_typescript(&structs);
    fs::write(output_path, typescript)
        .map_err(|e| format!("Failed to write TypeScript file: {}", e))?;

    Ok(structs.len())
}

/// Main entry point for the generate-types command
pub fn run(output: Option<String>, watch: bool) {
    let project_path = Path::new(".");

    // Validate Suprnova project
    let cargo_toml = project_path.join("Cargo.toml");
    if !cargo_toml.exists() {
        ui::error("Not a Suprnova project (no Cargo.toml found)");
        std::process::exit(1);
    }

    let output_path = output
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| project_path.join("frontend/src/types/inertia-props.ts"));

    ui::info("Scanning for InertiaProps structs...");

    match generate_types_to_file(project_path, &output_path) {
        Ok(0) => {
            ui::warning("No InertiaProps structs found.");
        }
        Ok(count) => {
            ui::info(&format!("Found {} InertiaProps struct(s)", count));
            ui::success(&format!("Generated {}", output_path.display()));
        }
        Err(e) => {
            ui::error(&e);
            std::process::exit(1);
        }
    }

    generate_route_types(project_path);

    if watch {
        ui::hint("Watching for changes...");
        if let Err(e) = start_watcher(project_path, &output_path) {
            ui::error(&format!("Failed to start watcher: {}", e));
            std::process::exit(1);
        }
    }
}

/// Generate route types
fn generate_route_types(project_path: &Path) {
    let routes_output = project_path.join("frontend/src/types/routes.ts");

    ui::info("Scanning routes for type-safe generation...");

    match super::generate_routes::generate_routes_to_file(project_path, &routes_output) {
        Ok(0) => {
            ui::warning("No routes found in src/routes.rs");
        }
        Ok(count) => {
            ui::info(&format!("Found {} route(s)", count));
            ui::success(&format!("Generated {}", routes_output.display()));
        }
        Err(e) => {
            ui::warning(&format!("Route generation error: {}", e));
        }
    }
}

/// Start file watcher for automatic type regeneration
fn start_watcher(project_path: &Path, output_path: &Path) -> Result<(), String> {
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc::channel;
    use std::time::Duration;

    let (tx, rx) = channel();
    let src_path = project_path.join("src");

    let mut watcher = RecommendedWatcher::new(
        move |res| {
            if let Ok(event) = res {
                let _ = tx.send(event);
            }
        },
        Config::default().with_poll_interval(Duration::from_secs(1)),
    )
    .map_err(|e| format!("Failed to create watcher: {}", e))?;

    watcher
        .watch(&src_path, RecursiveMode::Recursive)
        .map_err(|e| format!("Failed to watch directory: {}", e))?;

    ui::hint(&format!("Watching {} for changes", src_path.display()));

    let output_path = output_path.to_path_buf();
    let project_path = project_path.to_path_buf();

    loop {
        match rx.recv() {
            Ok(event) => {
                // Check if it's a Rust file change
                let is_rust_change = event
                    .paths
                    .iter()
                    .any(|p| p.extension().map(|e| e == "rs").unwrap_or(false));

                if is_rust_change {
                    ui::hint("Detected changes, regenerating types...");
                    match generate_types_to_file(&project_path, &output_path) {
                        Ok(count) => {
                            ui::success(&format!("Regenerated {} type(s)", count));
                        }
                        Err(e) => {
                            ui::error(&format!("Failed to regenerate: {}", e));
                        }
                    }
                }
            }
            Err(e) => {
                return Err(format!("Watch error: {}", e));
            }
        }
    }
}
