pub mod app;
pub mod auth;
pub mod resources;
pub mod torii_integration;
pub mod authorization;
pub mod cache;
pub mod config;
pub mod container;
pub mod context;
pub mod crypto;
pub mod csrf;
pub mod data;
pub mod database;
pub mod error;
pub mod hashing;
pub mod http;
pub mod http_client;
pub mod events;
pub mod filesystem;
pub mod inertia;
pub mod logging;
pub mod middleware;
pub mod pagination;
pub mod routing;
pub mod schedule;
pub mod sse;
pub mod telemetry;
pub mod validation;
pub mod workflow;
pub mod server;
pub mod session;
pub mod testing;

extern crate self as suprnova;

pub use app::Application;
pub use auth::{Auth, Authenticatable, AuthMiddleware, GuestMiddleware, UserProvider};
pub use torii_integration::{
    init_torii, middleware::BearerTokenMiddleware, Session, SessionToken, ToriiConfig, User, UserId,
};
pub use authorization::{Gate, Policy};
pub use cache::{Cache, CacheConfig, CacheStore, InMemoryCache, RedisCache};
pub use config::{env, env_optional, env_required, AppConfig, Config, Environment, ServerConfig};
pub use container::{App, Container};
pub use context::{Context, ContextStore};
pub use crypto::{Crypt, EncryptionKey};
pub use csrf::{csrf_field, csrf_meta_tag, csrf_token, CsrfMiddleware};
pub use data::{current_include_set, scope_include_set, Field, IncludeError, IncludeMiddleware, IsRelationLoaded, RequestIncludeSet};
pub use database::{
    AutoRouteBinding, Database, DatabaseConfig, DatabaseType, DbConnection, Model, ModelMut,
    RouteBinding, DB,
};
pub use error::{AppError, FrameworkError, HttpError, ValidationErrors};
pub use hashing::{hash, needs_rehash, verify, DEFAULT_COST as HASH_DEFAULT_COST};
pub use http::{
    json, text, Cookie, CookieOptions, FormRequest, FromParam, FromRequest, HttpResponse, Redirect,
    Request, Response, ResponseExt, SameSite,
};
pub use http::body::{
    collect_body_with_cap, global_max_request_body_bytes, set_global_max_request_body_bytes,
    DEFAULT_MAX_REQUEST_BODY_BYTES,
};
pub use http::upload::validators::{Image, MaxSize, MimeAllowlist, MimeType};
pub use http::upload::{
    global_max_multipart_body_bytes, global_upload_spill_threshold, parse_multipart_streaming,
    parse_multipart_streaming_with_cap, set_global_max_multipart_body_bytes,
    set_global_upload_spill_threshold, MultipartPayload, MultipartRequestHooks, MultipartValue,
    UploadedFile, UploadedFileBacking, DEFAULT_MAX_MULTIPART_BODY_BYTES,
    DEFAULT_UPLOAD_SPILL_THRESHOLD,
};
pub use http_client::{
    assert_not_sent, assert_sent, fake_response, ClientResponse, FailOnRealCallsGuard, Http,
    RecordedRequest, RequestBuilder,
};
pub use session::{
    session, session_mut, SessionConfig, SessionData, SessionMiddleware, SessionStore,
};
pub use logging::{
    current_request_id, init_subscriber, LogConfig, LogFormat, RequestId, RequestIdMiddleware,
};
pub use events::{ErrorOccurred, Event, EventDispatcher, EventFacade, Listener};
pub use filesystem::{copy_between_disks, AzBlobConfig, GcsConfig, S3Config, Storage};
pub use inertia::{
    DeferConfig, DeferOptions, EncryptHistoryMiddleware, Frontend, Inertia, Inertia303Middleware,
    InertiaConfig, InertiaRegistry, InertiaRequestExt, InertiaResponse, InertiaSharedData,
    InertiaVersionMiddleware, MergeConfig, MergeStrategy, OnceConfig, OnceOptions, PartialFilter,
    Prop, PropFuture, PropResolver, ScrollConfig, ScrollMetadata, VersionResolver,
};
pub use pagination::{
    CursorDirection, CursorPaginator, IntoInertiaScroll, LengthAwarePaginator, Paginated, Pagination,
};
pub use resources::{
    AsRelationshipValue,
    IncludeResolutionError,
    IncludeTree,
    IntoJsonResource,
    JsonApiBuilder,
    JsonApiResponse,
    PushIncluded,
    RelationshipValue,
    RequestFieldsetSet,
    Resource,
    ResourceIdentifier,
    current_fieldset,
    scope_fieldset,
};
pub use middleware::{
    register_global_middleware, Middleware, MiddlewareFuture, MiddlewareRegistry, Next,
};
pub use routing::{
    route, validate_route_path,
    // Internal functions used by macros (hidden from docs)
    __delete_impl, __fallback_impl, __get_impl, __post_impl, __put_impl,
    FallbackDefBuilder, GroupBuilder, GroupDef, GroupItem, GroupRoute, GroupRouter,
    IntoGroupItem, RouteBuilder, RouteDefBuilder, Router,
};
pub use schedule::{CronExpression, DayOfWeek, Schedule, Task, TaskBuilder, TaskEntry, TaskResult};
pub use sse::SseEvent;
pub use telemetry::{
    init_telemetry, CounterHandle, GaugeHandle, HistogramHandle, Metrics, OtelConfig,
    TelemetryGuard,
};
pub use validation::rule::{
    async_rules,
    rules,
    rules::{
        Alpha, AlphaNum, Between, Boolean, Confirmed, Different, Email, In, Integer, Max, Min,
        NotIn, Numeric, Required, RequiredIf, RequiredUnless, RequiredWith, Same, Url, Uuid,
    },
    AsyncRule,
    ContextualRule,
    FormContext,
    Rule,
    Unique,
};
pub use workflow::{
    start_named, StepStatus, WorkflowConfig, WorkflowContext, WorkflowHandle, WorkflowStatus,
    WorkflowWorker,
};
pub use server::{handle_request, Server};

// Re-export async_trait for middleware implementations
pub use async_trait::async_trait;
// Re-export the async_trait crate under a doc-hidden name so that
// proc-macros generated by suprnova can write
// `#[::suprnova::__async_trait::async_trait]` without requiring consumers to
// depend on async-trait directly.
#[doc(hidden)]
pub use async_trait as __async_trait;

// Re-export inventory for #[service(ConcreteType)] macro
#[doc(hidden)]
pub use inventory;

// Re-export indexmap so consumers implementing InertiaSharedData
// don't need to depend on it separately.
pub use indexmap;

// Re-export for macro usage
#[doc(hidden)]
pub use serde_json;

// Re-export serde for InertiaProps derive macro
pub use serde;

// Re-export validator for FormRequest validation
pub use validator;
pub use validator::Validate;

// Re-export the proc-macros for compile-time component validation and type safety
pub use suprnova_macros::domain_error;
pub use suprnova_macros::handler;
pub use suprnova_macros::inertia_response;
pub use suprnova_macros::injectable;
pub use suprnova_macros::redirect;
pub use suprnova_macros::request;
pub use suprnova_macros::service;
pub use suprnova_macros::policy;
pub use suprnova_macros::workflow;
pub use suprnova_macros::workflow_step;
pub use suprnova_macros::Data;
pub use suprnova_macros::FormRequest as FormRequestDerive;
pub use suprnova_macros::InertiaProps;
pub use suprnova_macros::MultipartRequest;
pub use suprnova_macros::suprnova_test;

// Re-export Jest-like testing macros
pub use suprnova_macros::describe;
pub use suprnova_macros::test;

#[macro_export]
macro_rules! json_response {
    ($($json:tt)+) => {
        Ok($crate::HttpResponse::json($crate::serde_json::json!($($json)+)))
    };
}

#[macro_export]
macro_rules! text_response {
    ($text:expr) => {
        Ok($crate::HttpResponse::text($text))
    };
}

/// Register global middleware that runs on every request
///
/// Global middleware is registered in `bootstrap.rs` and runs in registration order,
/// before any route-specific middleware.
///
/// # Example
///
/// ```rust,ignore
/// // In bootstrap.rs
/// use suprnova::global_middleware;
/// use crate::middleware;
///
/// pub fn register() {
///     global_middleware!(middleware::LoggingMiddleware);
///     global_middleware!(middleware::CorsMiddleware);
/// }
/// ```
#[macro_export]
macro_rules! global_middleware {
    ($middleware:expr) => {
        $crate::register_global_middleware($middleware)
    };
}

/// Create an expectation for fluent assertions
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::expect;
///
/// expect!(actual).to_equal(expected);
/// expect!(result).to_be_ok();
/// expect!(vec).to_have_length(3);
/// ```
///
/// On failure, shows clear output:
/// ```text
/// Test: "returns all todos"
///   at src/actions/todo_action.rs:25
///
///   expect!(actual).to_equal(expected)
///
///   Expected: 0
///   Received: 3
/// ```
#[macro_export]
macro_rules! expect {
    ($value:expr) => {
        $crate::testing::Expect::new($value, concat!(file!(), ":", line!()))
    };
}
