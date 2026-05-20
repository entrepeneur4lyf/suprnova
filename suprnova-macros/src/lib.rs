//! Procedural macros for the Suprnova framework
//!
//! This crate provides compile-time validated macros for:
//! - Inertia.js responses with component validation
//! - Named route redirects with route validation
//! - Service auto-registration
//! - Handler attribute for controller methods
//! - FormRequest for validated request data
//! - Jest-like testing with describe! and test! macros

use proc_macro::TokenStream;

mod command;
mod console_derive;
mod data;
mod describe;
mod domain_error;
mod factory;
mod handler;
mod inertia;
mod injectable;
mod model;
mod model_attribute;
mod multipart;
mod notification_mail;
mod suprnova_test;
mod redirect;
mod request;
mod service;
mod test_macro;
mod utils;
mod policy;
mod workflow;
mod workflow_step;

/// Derive macro for `Data` — composite derive that produces `Serialize`
/// (skipping `#[data(input_only)]` fields) and `Deserialize` (rejecting
/// payloads that set `#[data(output_only)]` fields). Also registers
/// `#[data(allow_include)]` fields into the runtime include allowlist
/// via `inventory::submit!`.
///
/// # Example
///
/// ```rust,ignore
/// #[derive(Data, Validate)]
/// struct UserDto {
///     pub id: i64,
///     pub name: String,
///
///     #[data(input_only)]
///     #[validate(length(min = 8))]
///     pub password: String,
///
///     #[data(output_only)]
///     pub computed_handle: String,
/// }
/// ```
#[proc_macro_derive(Data, attributes(data, json_resource))]
pub fn derive_data(input: TokenStream) -> TokenStream {
    data::derive_data_impl(input)
}

/// Derive macro for generating `Serialize` implementation for Inertia props
///
/// # Example
///
/// ```rust,ignore
/// #[derive(InertiaProps)]
/// struct HomeProps {
///     title: String,
///     user: User,
/// }
/// ```
#[proc_macro_derive(InertiaProps)]
pub fn derive_inertia_props(input: TokenStream) -> TokenStream {
    inertia::derive_inertia_props_impl(input)
}

/// Create an Inertia response with compile-time component validation.
///
/// # Signature
///
/// ```ignore
/// inertia_response!(req, "Component", Props [, InertiaConfig])
/// ```
///
/// The leading `req` arg is the current `Request` (or `&Request`). The
/// macro reads the request URL, the `X-Inertia*` headers, and the
/// `X-Inertia-Partial-*` filtering headers off it.
///
/// # Examples
///
/// ## With a typed struct (recommended for type safety):
/// ```rust,ignore
/// #[derive(InertiaProps)]
/// struct HomeProps {
///     title: String,
///     user: User,
/// }
///
/// pub async fn index(req: Request) -> Response {
///     inertia_response!(&req, "Home", HomeProps { title: "Welcome".into(), user })
/// }
/// ```
///
/// ## With JSON-like syntax (for quick prototyping):
/// ```rust,ignore
/// inertia_response!(&req, "Dashboard", { "user": { "name": "John" } })
/// ```
///
/// This macro validates that the component file exists at compile time.
/// It accepts `.svelte`, `.tsx`, `.jsx`, and `.vue` extensions in
/// `frontend/src/pages/`. If no matching file exists, you'll get a compile
/// error with suggestions.
#[proc_macro]
pub fn inertia_response(input: TokenStream) -> TokenStream {
    inertia::inertia_response_impl(input)
}

/// Create a redirect to a named route with compile-time validation
///
/// # Examples
///
/// ```rust,ignore
/// // Simple redirect
/// redirect!("users.index").into()
///
/// // Redirect with route parameters
/// redirect!("users.show").with("id", "42").into()
///
/// // Redirect with query parameters
/// redirect!("users.index").query("page", "1").into()
/// ```
///
/// This macro validates that the route name exists at compile time.
/// If the route doesn't exist, you'll get a compile error with suggestions.
#[proc_macro]
pub fn redirect(input: TokenStream) -> TokenStream {
    redirect::redirect_impl(input)
}

/// Mark a trait as a service for the App container
///
/// This attribute macro automatically adds `Send + Sync + 'static` bounds
/// to your trait, making it suitable for use with the dependency injection
/// container.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::service;
///
/// #[service]
/// pub trait HttpClient {
///     async fn get(&self, url: &str) -> Result<String, Error>;
/// }
///
/// // This expands to:
/// pub trait HttpClient: Send + Sync + 'static {
///     async fn get(&self, url: &str) -> Result<String, Error>;
/// }
/// ```
///
/// Then you can use it with the App container:
///
/// ```rust,ignore
/// // Register
/// App::bind::<dyn HttpClient>(Arc::new(RealHttpClient::new()));
///
/// // Resolve
/// let client: Arc<dyn HttpClient> = App::make::<dyn HttpClient>().unwrap();
/// ```
#[proc_macro_attribute]
pub fn service(attr: TokenStream, input: TokenStream) -> TokenStream {
    service::service_impl(attr, input)
}

/// Attribute macro to auto-register a concrete type as a singleton
///
/// This macro automatically:
/// 1. Derives `Default` and `Clone` for the struct
/// 2. Registers it as a singleton in the App container at startup
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::injectable;
///
/// #[injectable]
/// pub struct AppState {
///     pub counter: u32,
/// }
///
/// // Automatically registered at startup
/// // Resolve via:
/// let state: AppState = App::get().unwrap();
/// ```
#[proc_macro_attribute]
pub fn injectable(_attr: TokenStream, input: TokenStream) -> TokenStream {
    injectable::injectable_impl(input)
}

/// Define a domain error with automatic HTTP response conversion
///
/// This macro automatically:
/// 1. Derives `Debug` and `Clone` for the type
/// 2. Implements `Display`, `Error`, and `HttpError` traits
/// 3. Implements `From<T> for FrameworkError` for seamless `?` usage
///
/// # Attributes
///
/// - `status`: HTTP status code (default: 500)
/// - `message`: Error message for Display (default: struct name converted to sentence)
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::domain_error;
///
/// #[domain_error(status = 404, message = "User not found")]
/// pub struct UserNotFoundError {
///     pub user_id: i32,
/// }
///
/// // Usage in controller - just use ? operator
/// pub async fn get_user(id: i32) -> Result<User, FrameworkError> {
///     users.find(id).ok_or(UserNotFoundError { user_id: id })?
/// }
/// ```
#[proc_macro_attribute]
pub fn domain_error(attr: TokenStream, input: TokenStream) -> TokenStream {
    domain_error::domain_error_impl(attr, input)
}

/// Attribute macro for controller handler methods
///
/// Transforms handler functions to automatically extract typed parameters
/// from HTTP requests using the `FromRequest` trait.
///
/// # Examples
///
/// ## With Request parameter:
/// ```rust,ignore
/// use suprnova::{handler, Request, Response, json_response};
///
/// #[handler]
/// pub async fn index(req: Request) -> Response {
///     json_response!({ "message": "Hello" })
/// }
/// ```
///
/// ## With FormRequest parameter:
/// ```rust,ignore
/// use suprnova::{handler, Response, json_response, request};
///
/// #[request]
/// pub struct CreateUserRequest {
///     #[validate(email)]
///     pub email: String,
/// }
///
/// #[handler]
/// pub async fn store(form: CreateUserRequest) -> Response {
///     // `form` is already validated - returns 422 if invalid
///     json_response!({ "email": form.email })
/// }
/// ```
///
/// ## Without parameters:
/// ```rust,ignore
/// #[handler]
/// pub async fn health_check() -> Response {
///     json_response!({ "status": "ok" })
/// }
/// ```
#[proc_macro_attribute]
pub fn handler(attr: TokenStream, input: TokenStream) -> TokenStream {
    handler::handler_impl(attr, input)
}

/// Derive macro for FormRequest trait
///
/// Generates the `FormRequest` trait implementation for a struct.
/// The struct must also derive `serde::Deserialize` and `validator::Validate`.
///
/// For the cleanest DX, use the `#[request]` attribute macro instead,
/// which handles all derives automatically.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{FormRequest, Deserialize, Validate};
///
/// #[derive(Deserialize, Validate, FormRequest)]
/// pub struct CreateUserRequest {
///     #[validate(email)]
///     pub email: String,
///
///     #[validate(length(min = 8))]
///     pub password: String,
/// }
/// ```
#[proc_macro_derive(FormRequest, attributes(form_request))]
pub fn derive_form_request(input: TokenStream) -> TokenStream {
    request::derive_request_impl(input)
}

/// Attribute macro for clean request data definition
///
/// This is the recommended way to define validated request types.
/// It automatically adds the necessary derives and generates the trait impl.
///
/// Works with both:
/// - `application/json` - JSON request bodies
/// - `application/x-www-form-urlencoded` - HTML form submissions
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::request;
///
/// #[request]
/// pub struct CreateUserRequest {
///     #[validate(email)]
///     pub email: String,
///
///     #[validate(length(min = 8))]
///     pub password: String,
/// }
///
/// // This can now be used directly in handlers:
/// #[handler]
/// pub async fn store(form: CreateUserRequest) -> Response {
///     // Automatically validated - returns 422 with errors if invalid
///     json_response!({ "email": form.email })
/// }
/// ```
#[proc_macro_attribute]
pub fn request(attr: TokenStream, input: TokenStream) -> TokenStream {
    request::request_attr_impl(attr, input)
}

/// Attribute macro for database-enabled tests
///
/// This macro simplifies writing tests that need database access by automatically
/// setting up an in-memory SQLite database with migrations applied.
///
/// By default, it uses `crate::migrations::Migrator` as the migrator type,
/// following Suprnova's convention for migration location.
///
/// # Examples
///
/// ## Basic usage (recommended):
/// ```rust,ignore
/// use suprnova::suprnova_test;
/// use suprnova::testing::TestDatabase;
///
/// #[suprnova_test]
/// async fn test_user_creation(db: TestDatabase) {
///     // db is an in-memory SQLite database with all migrations applied
///     // Any code using DB::connection() will use this test database
///     let action = CreateUserAction::new();
///     let user = action.execute("test@example.com").await.unwrap();
///     assert!(user.id > 0);
/// }
/// ```
///
/// ## Without TestDatabase parameter:
/// ```rust,ignore
/// #[suprnova_test]
/// async fn test_action_without_direct_db_access() {
///     // Database is set up but not directly accessed
///     // Actions using DB::connection() still work
///     let action = MyAction::new();
///     action.execute().await.unwrap();
/// }
/// ```
///
/// ## With custom migrator:
/// ```rust,ignore
/// #[suprnova_test(migrator = my_crate::CustomMigrator)]
/// async fn test_with_custom_migrator(db: TestDatabase) {
///     // Uses custom migrator instead of default
/// }
/// ```
#[proc_macro_attribute]
pub fn suprnova_test(attr: TokenStream, input: TokenStream) -> TokenStream {
    suprnova_test::suprnova_test_impl(attr, input)
}

/// Derive macro for typed console commands.
///
/// Goes on top of `#[derive(clap::Parser)]`. Reads
/// `#[console(name = "...", description = "...")]` for Suprnova-side
/// metadata; clap's `#[command(...)]` continues to drive arg-parsing
/// config. The struct must also implement
/// [`suprnova::TypedCommand`](https://docs.rs/suprnova) so the
/// generated runner knows where to send the parsed args.
///
/// # Example
///
/// ```rust,ignore
/// use async_trait::async_trait;
/// use suprnova::{Command, FrameworkError, TypedCommand};
///
/// #[derive(clap::Parser, Command)]
/// #[console(name = "greet", description = "Greet someone")]
/// pub struct Greet {
///     #[arg(short, long)]
///     name: Option<String>,
///     #[arg(long)]
///     loud: bool,
/// }
///
/// #[async_trait]
/// impl TypedCommand for Greet {
///     async fn run(self) -> Result<(), FrameworkError> {
///         let target = self.name.unwrap_or_else(|| "world".into());
///         let prefix = if self.loud { "HELLO" } else { "Hello" };
///         println!("{prefix}, {target}!");
///         Ok(())
///     }
/// }
/// ```
#[proc_macro_derive(Command, attributes(console))]
pub fn derive_command(input: TokenStream) -> TokenStream {
    console_derive::derive_command_impl(input)
}

/// Attribute macro for registering an async fn as a console command.
///
/// Applied to `async fn(Vec<String>) -> Result<(), FrameworkError>`,
/// the macro preserves the function, generates an `inventory`-shaped
/// adapter, and submits a `CommandEntry` so `suprnova::console::dispatch_argv`
/// can find it.
///
/// # Attributes
///
/// - `name = "db:seed"` (required) — invocation name on the CLI
/// - `description = "..."` (optional) — help-line text
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{command, FrameworkError};
///
/// #[command(name = "db:seed", description = "Run all registered seeders")]
/// async fn db_seed(_args: Vec<String>) -> Result<(), FrameworkError> {
///     suprnova::seed::run_all().await
/// }
/// ```
#[proc_macro_attribute]
pub fn command(attr: TokenStream, input: TokenStream) -> TokenStream {
    command::command_impl(attr, input)
}

/// Attribute macro for defining durable workflows
#[proc_macro_attribute]
pub fn workflow(attr: TokenStream, input: TokenStream) -> TokenStream {
    workflow::workflow_impl(attr, input)
}

/// Attribute macro for defining durable workflow steps
#[proc_macro_attribute]
pub fn workflow_step(attr: TokenStream, input: TokenStream) -> TokenStream {
    workflow_step::workflow_step_impl(attr, input)
}

/// Group related tests with a descriptive name
///
/// Creates a module containing related tests, similar to Jest's describe blocks.
/// Supports nesting for hierarchical test organization.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{describe, test, expect};
/// use suprnova::testing::TestDatabase;
///
/// describe!("ListTodosAction", {
///     test!("returns empty list when no todos exist", async fn(db: TestDatabase) {
///         let action = ListTodosAction::new();
///         let todos = action.execute().await.unwrap();
///         expect!(todos).to_be_empty();
///     });
///
///     // Nested describe for grouping related tests
///     describe!("with pagination", {
///         test!("returns first page", async fn(db: TestDatabase) {
///             // ...
///         });
///     });
/// });
/// ```
#[proc_macro]
pub fn describe(input: TokenStream) -> TokenStream {
    describe::describe_impl(input)
}

/// Define an individual test case with a descriptive name
///
/// Creates a test function with optional TestDatabase parameter.
/// The test name is displayed in failure output for easy identification.
///
/// # Examples
///
/// ## Async test with database
/// ```rust,ignore
/// test!("creates a user", async fn(db: TestDatabase) {
///     let user = CreateUserAction::new().execute("test@example.com").await.unwrap();
///     expect!(user.email).to_equal("test@example.com".to_string());
/// });
/// ```
///
/// ## Async test without database
/// ```rust,ignore
/// test!("calculates sum", async fn() {
///     let result = calculate_sum(1, 2).await;
///     expect!(result).to_equal(3);
/// });
/// ```
///
/// ## Sync test
/// ```rust,ignore
/// test!("adds numbers", fn() {
///     expect!(1 + 1).to_equal(2);
/// });
/// ```
///
/// On failure, the test name is shown:
/// ```text
/// Test: "creates a user"
///   at src/actions/user_action.rs:25
///
///   expect!(actual).to_equal(expected)
///
///   Expected: "test@example.com"
///   Received: "wrong@email.com"
/// ```
#[proc_macro]
pub fn test(input: TokenStream) -> TokenStream {
    test_macro::test_impl(input)
}

/// Attribute macro for authorization policy classes.
///
/// Annotate an `impl` block with `#[policy(UserType, ResourceType)]` to
/// automatically register each method as a named Gate action. The gate name
/// is derived by combining the method name with the lowercased resource type:
/// `fn view(...)` on `Comment` → `"view-comment"`.
///
/// Call `suprnova::authorization::init_policies()` once at startup (or in
/// tests) to run all submitted registrations. `Server::serve` calls this
/// automatically.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::policy;
///
/// struct CommentPolicy;
///
/// #[policy(User, Comment)]
/// impl CommentPolicy {
///     fn view(_user: &User, _comment: &Comment) -> bool { true }
///     fn update(user: &User, comment: &Comment) -> bool {
///         comment.author_id == user.id
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn policy(attr: TokenStream, item: TokenStream) -> TokenStream {
    policy::policy(attr, item)
}

/// Derive macro for `MultipartRequest` — strongly-typed multipart extractor.
///
/// Annotate each field with `#[field("form_name")]` to bind it to the
/// matching multipart part. Supported field shapes:
///
/// - `UploadedFile<V>` — required file; 422 if absent
/// - `Option<UploadedFile<V>>` — optional file
/// - `Vec<UploadedFile<V>>` — collect every matching file part
/// - `T: FromStr` (e.g. `String`, `u32`) — required text field
/// - `Option<T>` — optional text field
/// - `Vec<T>` — collect every matching text part
///
/// The validator `V` defaults to `()` (accept anything). Built-in
/// validators live in `suprnova::http::upload::validators` and can be
/// composed via tuples: `UploadedFile<(Image, MaxSize<5_000_000>)>`.
///
/// `#[multipart(custom_hooks)]` on the struct suppresses the
/// auto-generated `impl MultipartRequestHooks for Self {}`, letting
/// users override `authorize` and `after_validation`.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::http::upload::{Image, MaxSize, UploadedFile};
/// use suprnova::MultipartRequest;
///
/// #[derive(MultipartRequest)]
/// pub struct AvatarUpload {
///     #[field("avatar")]
///     pub avatar: UploadedFile<(Image, MaxSize<5_242_880>)>,
///
///     #[field("caption")]
///     pub caption: Option<String>,
/// }
/// ```
#[proc_macro_derive(MultipartRequest, attributes(field, multipart))]
pub fn multipart_request(input: TokenStream) -> TokenStream {
    multipart::expand(input)
}

/// Derive macro for `NotificationMailable` — auto-generates `to_mail`
/// from a `#[mail(...)]` attribute.
///
/// The struct must also derive `Serialize` (used as the Tera template
/// context) and `Deserialize` (so the queued path can rebuild the
/// notification from the envelope payload), and must already implement
/// [`Notification`](::suprnova::notifications::Notification).
///
/// # Attributes
///
/// All values are string literals. Inline templates use [Tera](https://keats.github.io/tera/) syntax.
///
/// | Key             | Required? | Purpose                                                                    |
/// |-----------------|-----------|----------------------------------------------------------------------------|
/// | `subject`       | yes       | Subject Tera template — rendered with `self` as the JSON context.          |
/// | `html`          | ‡         | Inline HTML body Tera template.                                            |
/// | `html_template` | ‡         | Path to an HTML body Tera template, embedded via `include_str!`.           |
/// | `text`          | ‡         | Inline plain-text body Tera template.                                      |
/// | `text_template` | ‡         | Path to a plain-text body Tera template, embedded via `include_str!`.      |
/// | `from`          | no        | Sender email address — overrides the framework default `noreply@localhost`. |
/// | `from_name`     | no        | Display name for the sender. Requires `from`.                              |
/// | `cc`            | no        | Comma-separated CC list (e.g. `"a@x.com, b@y.com"`). Whitespace is ignored. |
/// | `bcc`           | no        | Comma-separated BCC list.                                                  |
/// | `reply_to`      | no        | Comma-separated Reply-To list.                                             |
///
/// ‡ At least one of `html` / `html_template` / `text` / `text_template`
///   must be present. `html` and `html_template` are mutually exclusive;
///   same for `text` and `text_template`.
///
/// # Compile-time errors
///
/// The derive refuses to compile when:
/// - `subject` is missing.
/// - Both `html` and `html_template` are set (or both `text` variants).
/// - Neither an HTML nor a text body is provided (empty-body invariant).
/// - `from_name` is set without `from`.
/// - Any unknown key is used.
///
/// The runtime empty-body guard in `MailChannel::deliver` stays as
/// defense in depth, but a misconfigured `#[mail(...)]` should never
/// reach that guard.
///
/// # Examples
///
/// ## Inline templates
///
/// ```rust,ignore
/// use serde::{Serialize, Deserialize};
/// use suprnova::{NotificationMailable, Notification};
///
/// #[derive(Serialize, Deserialize, NotificationMailable)]
/// #[mail(
///     subject = "Your order shipped — tracking {{ tracking }}",
///     html    = "<p>Tracking: <code>{{ tracking }}</code></p>",
///     text    = "Tracking: {{ tracking }}",
///     from    = "orders@suprnova.dev",
///     from_name = "Suprnova Orders",
/// )]
/// pub struct OrderShipped { pub tracking: String }
///
/// impl Notification for OrderShipped { /* notification_name + channels + data */ }
/// ```
///
/// ## File-backed templates
///
/// `html_template` and `text_template` paths are resolved relative to
/// the source file containing the derive (standard `include_str!`
/// behavior). A missing template file fails the build.
///
/// ```rust,ignore
/// #[derive(Serialize, Deserialize, NotificationMailable)]
/// #[mail(
///     subject       = "Order #{{ order_id }} shipped",
///     html_template = "templates/order_shipped.html",
///     text_template = "templates/order_shipped.txt",
/// )]
/// pub struct OrderShipped { pub order_id: u64 }
/// ```
#[proc_macro_derive(NotificationMailable, attributes(mail))]
pub fn derive_notification_mailable(input: TokenStream) -> TokenStream {
    notification_mail::derive_notification_mailable_impl(input)
}

/// `#[suprnova::model]` — declare a Suprnova/Eloquent model.
///
/// Generates SeaORM Entity, Model, ActiveModel, Column enum, Relation
/// stub, and Eloquent trait impls. Registers the model via
/// `inventory::submit!` so Phase 8 (Admin) can enumerate every model
/// at boot.
///
/// # Attribute keys (Phase 10A T3)
///
/// - `table = "..."` — SQL table name. Defaults to the naive plural of
///   the struct's snake-case name (`User` → `users`).
/// - `primary_key = "..."` — primary-key column name. Defaults to `"id"`.
/// - `key_type = "..."` — primary-key Rust type (parsed as a `Type`).
///   Defaults to `"i64"`.
/// - `auto_increment = true|false` — defaults to `true`.
/// - `connection = "..."` — multi-connection routing identifier.
///   Defaults to `"default"`.
///
/// Additional keys (`fillable`, `guarded`, `casts`, `timestamps`,
/// `created_at`, `updated_at`, `soft_deletes`, `soft_deletes_column`,
/// `appends`, `hidden`, `visible`, `mutators`, `touches`) are parsed
/// here but only consumed by later Phase 10A tasks.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::model;
///
/// #[model(table = "users")]
/// pub struct User {
///     pub id: i64,
///     pub name: String,
///     pub email: String,
/// }
/// ```
///
/// See `docs/core/eloquent.md` for the full reference.
#[proc_macro_attribute]
pub fn model(attr: TokenStream, item: TokenStream) -> TokenStream {
    model::expand(attr.into(), item.into())
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

/// `#[accessor]` — function-level attribute that marks an
/// `impl Model { ... }` method as an Eloquent attribute reader.
///
/// Applied to `fn name(&self) -> T`. When the field name appears in
/// `#[model(appends = [...])]`, the model's `to_json()` calls the
/// method and inserts the JSON-encoded result under that key.
///
/// The macro itself is a pass-through; the real wiring lives in the
/// struct-level `#[suprnova::model]` macro's `to_json` emission.
///
/// # Example
///
/// ```rust,ignore
/// #[suprnova::model(appends = ["full_name"])]
/// pub struct User {
///     pub id: i64,
///     pub first_name: String,
///     pub last_name: String,
/// }
///
/// impl User {
///     #[suprnova::accessor]
///     pub fn full_name(&self) -> String {
///         format!("{} {}", self.first_name, self.last_name)
///     }
/// }
///
/// // u.to_json() includes "full_name": "Alice X"
/// ```
#[proc_macro_attribute]
pub fn accessor(attr: TokenStream, item: TokenStream) -> TokenStream {
    model_attribute::accessor(attr.into(), item.into())
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

/// `#[mutator]` — function-level attribute that marks an
/// `impl Model { ... }` method as the routed write-path for a column.
///
/// Applied to
/// `fn set_<field>(&mut self, value: serde_json::Value) -> Result<(), FrameworkError>`.
/// When the field name appears in `#[model(mutators = [...])]`, the
/// model's `fill` / `create` / `update` path calls
/// `self.set_<field>(value)?` instead of direct field assignment. The
/// body owns the deserialise + transform.
///
/// Direct field assignment (`user.password = "raw"`) bypasses the
/// mutator — same as Laravel's `$user->password = ...` vs
/// `$user->fill(...)`.
///
/// # Example
///
/// ```rust,ignore
/// #[suprnova::model(fillable = ["password"], mutators = ["password"])]
/// pub struct User {
///     pub id: i64,
///     pub password: String,
/// }
///
/// impl User {
///     #[suprnova::mutator]
///     pub fn set_password(
///         &mut self,
///         value: serde_json::Value,
///     ) -> Result<(), suprnova::FrameworkError> {
///         let raw: String = serde_json::from_value(value).map_err(|e| {
///             suprnova::FrameworkError::validation("password", format!("{e}"))
///         })?;
///         self.password = bcrypt(raw);
///         Ok(())
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn mutator(attr: TokenStream, item: TokenStream) -> TokenStream {
    model_attribute::mutator(attr.into(), item.into())
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

/// `#[suprnova::prunable]` — register a `Prunable` / `MassPrunable`
/// impl with the `model:prune` runtime.
///
/// Wraps the user's `impl Prunable for T` (or `impl MassPrunable for T`)
/// block and submits a `PrunerEntry` into the inventory-backed
/// registry. At runtime, `suprnova::eloquent::prune_all` /
/// `prune_all_dry` / `prune_one` walk the registry; the `model:prune`
/// console command exposes the same surface on the CLI.
///
/// The macro detects which trait is being implemented via the trait
/// path's last segment — accepts both `Prunable` / `MassPrunable` and
/// fully qualified `suprnova::eloquent::Prunable` /
/// `suprnova::eloquent::MassPrunable`.
///
/// # Example
///
/// ```rust,ignore
/// use async_trait::async_trait;
/// use chrono::{Duration, Utc};
/// use suprnova::eloquent::Prunable;
///
/// #[suprnova::prunable]
/// #[async_trait]
/// impl Prunable for Session {
///     fn prunable() -> suprnova::Builder<Self> {
///         Self::query().filter_op(
///             "expires_at",
///             "<",
///             (Utc::now() - Duration::days(30)).to_rfc3339(),
///         )
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn prunable(_attr: TokenStream, item: TokenStream) -> TokenStream {
    model::prunable::expand(item.into())
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

/// Derive macro for `Factory` — generates a sibling marker struct plus
/// the `Factory` impl from a struct that implements `fake::Dummy`.
///
/// Applied to a model `User`, the derive emits:
///
/// ```rust,ignore
/// pub struct UserFactory;
///
/// impl ::suprnova::Factory for UserFactory {
///     type Model = User;
///     fn definition() -> User { ::suprnova::__fake::Faker.fake::<User>() }
/// }
/// ```
///
/// The marker's visibility matches the model's. The generated name
/// defaults to `<ModelName>Factory`; override via `#[factory(name = "...")]`.
///
/// The model must implement `fake::Dummy<fake::Faker>` — typically via
/// `#[derive(suprnova::Dummy)]`. `Dummy` is re-exported from suprnova so
/// consumers don't need a direct `fake` dependency.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{Dummy, Factory};
///
/// #[derive(Dummy, Factory)]
/// pub struct User {
///     pub id: i32,
///     pub name: String,
///     pub email: String,
/// }
///
/// // `UserFactory` exists; call:
/// let users = UserFactory::new().count(10).make_many();
/// ```
///
/// # Limitations
///
/// v1 only supports plain (non-generic) structs. Enums, unions, and
/// generics fail to compile with a clear error.
#[proc_macro_derive(Factory, attributes(factory))]
pub fn derive_factory(input: TokenStream) -> TokenStream {
    factory::derive_factory_impl(input)
}
