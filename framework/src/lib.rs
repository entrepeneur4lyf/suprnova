pub mod app;
pub mod auth;
pub mod resources;
pub mod torii_integration;
pub mod authorization;
pub mod broadcasting;
pub mod bus;
pub mod cache;
pub mod config;
pub mod container;
pub mod context;
pub mod crypto;
pub mod csrf;
pub mod data;
pub mod database;
pub mod eloquent;
pub mod error;
pub(crate) mod lock;
pub mod hashing;
pub mod http;
pub mod http_client;
pub mod idempotency;
pub mod events;
pub mod filesystem;
pub mod inertia;
pub mod logging;
pub mod middleware;
pub mod pagination;
pub mod queue;
pub mod routing;
pub mod schedule;
pub mod sse;
pub mod telemetry;
pub mod validation;
pub mod web_push;
pub mod workflow;
pub mod ws;
pub mod server;
pub mod session;
pub mod testing;
pub mod rate_limit;
pub mod mail;
pub mod auth_flows;
pub mod features;
pub mod notifications;
pub mod factory;
pub mod seed;
pub mod console;
pub mod supervisor;
pub mod prelude;

extern crate self as suprnova;

pub use app::Application;
pub use auth::{Auth, Authenticatable, AuthMiddleware, GuestMiddleware, UserProvider};
pub use torii_integration::{
    init_torii, middleware::BearerTokenMiddleware, LockoutStatus, Session, SessionToken,
    ToriiConfig, User, UserId,
};
pub use authorization::{Gate, Policy};
pub use cache::{Cache, CacheConfig, CacheStore, InMemoryCache, LockGuard, RedisCache};
pub use config::{env, env_optional, env_required, AppConfig, Config, Environment, ServerConfig};
pub use container::{App, Container};
pub use context::{Context, ContextStore};
pub use crypto::{Crypt, EncryptionKey};
pub use csrf::{csrf_field, csrf_meta_tag, csrf_token, CsrfMiddleware};
pub use data::{current_include_set, scope_include_set, Field, IncludeError, IncludeMiddleware, IsRelationLoaded, RequestIncludeSet};
pub use database::{
    AutoRouteBinding, Database, DatabaseConfig, DatabaseType, DbConnection, DbTableBuilder,
    DynamicRow, EntityExt, EntityExtMut, RouteBinding, Transaction, TxHandle, DB,
};

// SeaORM type aliasing — Suprnova design principle #4: SeaORM is an
// implementation detail; consumers reach for `suprnova::*` and never
// `use sea_orm::*`. Every type a user would name in handler / model /
// migration code is re-exported here.
pub use sea_orm::{
    ActiveModelBehavior, ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait,
    DatabaseConnection, DatabaseTransaction, DeriveActiveEnum, EntityName, EntityTrait,
    Iden, IntoActiveModel, ModelTrait, NotSet, PrimaryKeyToColumn, PrimaryKeyTrait,
    QueryFilter, QueryOrder, QuerySelect, RelationDef, RelationTrait, Schema, Select,
    Set, TransactionTrait, TryGetable,
};
pub use sea_orm::sea_query;
pub use sea_orm::strum::IntoEnumIterator as Iterable;

// Top-level escape hatch (spec 02-seaorm-aliasing §Rationale): the full
// `sea_orm` module is reachable as `suprnova::sea_orm::*` so users who
// need a type we haven't aliased can still get to it without adding
// `sea_orm` to their Cargo.toml. The aliased names above remain the
// documented surface; this is the "I know what I'm doing" path.
pub use ::sea_orm;
pub use error::{AppError, FrameworkError, HttpError, ValidationErrors};
pub use hashing::{hash, needs_rehash, verify, DEFAULT_COST as HASH_DEFAULT_COST};
pub use idempotency::{Idempotency, Idempotent};
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
    CursorDirection, CursorPaginator, IntoInertiaScroll, LengthAwarePaginator, Paginated,
    Paginator, Pagination,
};
pub use broadcasting::{BroadcastEnvelope, BroadcastHub, BroadcastListener, Broadcastable, BroadcastingWsHandler, InMemoryBroadcastHub};
pub use bus::{Bus, Dispatched};
pub use queue::{BackoffSchedule, Envelope, EnvelopeError, Job, Queue};
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
    __delete_impl, __fallback_impl, __get_impl, __post_impl, __put_impl, __ws_impl,
    FallbackDefBuilder, GroupBuilder, GroupDef, GroupItem, GroupRoute, GroupRouter,
    IntoGroupItem, RouteBuilder, RouteDefBuilder, Router, WsRouteDef,
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
pub use rate_limit::{RateLimitMiddleware, RateLimiter, SlidingWindowConfig};
pub use server::{handle_request, Server};
pub use web_push::{
    ContentEncoding, PushResponse, SubscriptionInfo, VapidClaims, VapidKey, VapidSigner,
    WebPushClient, WebPushError,
};
pub use factory::{persist_via_seaorm, Factory, FactoryBuilder, Persistable, Sequence};
pub use seed::Seeder;
pub use console::{dispatch_argv, CommandEntry, CommandHandler, TypedCommand};
pub use supervisor::{RestartPolicy, Supervisor, SupervisorEntry, SupervisorRegistry};

#[doc(hidden)]
pub use clap as __clap;
pub use mail::{Address, Attachment, Mail, MailFake, Mailable, SendMailJob};
pub use auth_flows::{
    BruteForce, EmailVerification, EmailVerificationMail, EnrollmentResponse,
    LoginThrottleMiddleware, PasswordChangedMail, PasswordReset, PasswordResetMail, TwoFactor,
    TwoFactorUser,
};
// Phase 13 — feature flags.
//
// `Feature`, `Evaluator`, and `EvaluatorRef` re-export cleanly at the
// crate root. `Context` and the `context!` macro cannot — both names
// collide with the framework's own per-request context module
// (`crate::context`). Consumers reach for the featureflag context as
// `suprnova::features::Context` and the macro as
// `featureflag::context!` (the crate is in scope transitively); we
// expose the rest of the primitives + the non-colliding macros here.
pub use features::{Evaluator, EvaluatorRef, Feature};
pub use featureflag::{feature, is_enabled};
// Phase 10 — Eloquent. Foundation primitives land in 10A; relationships
// (10B) and collections/pagination/observers (10C) extend the same
// `eloquent` module. The `ModelEntry` registry is populated at compile
// time by `#[suprnova::model]` (Task 3) and walked at boot by Phase 8
// (Admin), `model:prune`, and future tooling.
pub use eloquent::{
    find_model_by_table, find_morph_type, find_morph_type_by_id, find_relation, models,
    morph_types, prune_all, prune_all_dry, prune_one, relations, relations_of, unguarded,
    AggregateKind, AsArray, AsArrayObject, AsBool, AsCollection, AsDate, AsDateTime, AsDecimal,
    AsEncrypted, AsEncryptedArray, AsEncryptedCollection, AsEncryptedObject, AsEnum, AsFloat,
    AsHashed, AsImmutableDate, AsImmutableDateTime, AsInt, AsJson, AsObject, AsOptionalDateTime,
    AsString, AsTimestamp, Attrs, BelongsTo, BelongsToMany, Builder, Cast, Collection, Direction,
    DynCast, EagerLoadCache, EagerLoadDispatch, EloquentModel, Fillable, FirstOrCreate, GlobalScope,
    HasMany, HasManyThrough, HasOne, HasOneThrough, IntoColumn, IntoDynCast, IntoVal, LazyCollection,
    MassPrunable, Model, ModelEntry, MorphMany, MorphOne, MorphTo, MorphToMany, MorphTypeEntry,
    MorphedByMany, Prunable, PrunerEntry, Relation, RelationEntry, RelationKind, ReplicateExt,
    ScopeRegistry, SoftDeletes, Touchable,
};
// Phase 10C T1 — model lifecycle events. The 16 per-type event
// structs (`Created`, `Saving`, ...) are macro-emitted into each
// model's `events::` submodule; the cross-model shared types
// (`EventResult`, listener traits, dispatch helpers) re-export here
// so user code reaches them as `suprnova::EventResult`,
// `suprnova::CancellableListener`, etc.
pub use eloquent::events::{
    dispatch_after, dispatch_cancellable, listen_cancellable, CancellableListener, EventResult,
    ModelEventHooks,
};
// Phase 10C T2a — lifecycle observers. Users implement `Observer<M>`
// on their observer struct (zero-sized or `Arc`-clonable); the
// `#[suprnova::observer(M)]` macro (T2b) walks the impl block and
// registers per-method listeners through the `ObserverEntry` inventory.
// `bootstrap_observers` drains the inventory at startup.
pub use eloquent::observers::{
    bootstrap_observers, Observer, ObserverEntry, ObserverInstallFuture,
};
// `casts!` macro is `#[macro_export]` in eloquent/casts/mod.rs — re-exported
// at the crate root automatically. No `pub use` needed here.
pub use notifications::{
    Channel, DynNotification, Notifiable, Notification, NotificationDispatcher,
    NotificationFactory, Notify, SendNotificationJob,
};
pub use notifications::channels::broadcast::BroadcastChannelStub;
pub use notifications::channels::database::DatabaseChannel;
pub use notifications::channels::mail::{
    register_mail_renderer, MailChannel, MailRendering, NotificationMailable,
};
pub use notifications::channels::webpush::WebPushChannel;

pub use ws::{WebSocketHandler, WsConfig, WsSocket};

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

// Doc-hidden re-export of the Eloquent dispatcher seal. The
// `#[suprnova::model]` macro emits
// `impl ::suprnova::__private_eloquent::Sealed for <Model>`; user
// crates cannot reach the same name without depending on the framework
// `__private_eloquent` re-export deliberately, so user-written `impl
// EagerLoadDispatch for X` fails to compile (`the trait bound `X:
// __sealed::Sealed` is not satisfied`).
#[doc(hidden)]
pub use eloquent::relations::__sealed as __private_eloquent;

// Re-export indexmap so consumers implementing InertiaSharedData
// don't need to depend on it separately.
pub use indexmap;

// Re-export for macro usage
#[doc(hidden)]
pub use serde_json;

// Re-export serde for InertiaProps derive macro
pub use serde;

// Re-export chrono so macros (e.g. the `#[suprnova::model]` timestamp
// injection in T9) can emit `::suprnova::chrono::Utc::now()` without
// requiring downstream user crates to add `chrono` to their
// `[dependencies]`. Public (not doc-hidden) because `DateTime<Utc>`
// is a Laravel-shape column type users name in their own structs.
pub use chrono;

// Re-export Tera for the `#[derive(NotificationMailable)]` macro — the
// generated `to_mail` references `::suprnova::__tera::{Context, Tera}`
// so consumers don't need to add `tera` to their `[dependencies]`.
#[doc(hidden)]
pub use tera as __tera;

// Re-export fake for the `#[derive(Factory)]` macro and for consumers
// who want to hand-write `Mailable::definition`-style code referencing
// `::suprnova::__fake::Faker.fake()`. The public re-exports below cover
// the common surface: `Dummy` derive (struct auto-fill), `Fake` trait
// (`.fake()` method), `Faker` (universal generator).
#[doc(hidden)]
pub use fake as __fake;
pub use fake::{Dummy, Fake, Faker};

// Re-export validator for FormRequest validation
pub use validator;
pub use validator::Validate;

// Re-export the proc-macros for compile-time component validation and type safety
pub use suprnova_macros::accessor;
pub use suprnova_macros::command;
pub use suprnova_macros::Command;
pub use suprnova_macros::domain_error;
pub use suprnova_macros::handler;
pub use suprnova_macros::inertia_response;
pub use suprnova_macros::injectable;
pub use suprnova_macros::model;
pub use suprnova_macros::mutator;
pub use suprnova_macros::observer;
pub use suprnova_macros::prunable;
pub use suprnova_macros::redirect;
pub use suprnova_macros::request;
pub use suprnova_macros::scopes;
pub use suprnova_macros::service;
pub use suprnova_macros::policy;
pub use suprnova_macros::workflow;
pub use suprnova_macros::workflow_step;
pub use suprnova_macros::Data;
pub use suprnova_macros::FormRequest as FormRequestDerive;
pub use suprnova_macros::InertiaProps;
// Derives + traits live in separate namespaces, so the `Factory`
// derive re-export coexists with the `Factory` trait re-export above.
// Same pattern as `serde::Serialize` (trait + derive same name).
pub use suprnova_macros::Factory;
pub use suprnova_macros::MultipartRequest;
pub use suprnova_macros::NotificationMailable;
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
