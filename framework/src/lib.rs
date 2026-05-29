pub mod app;
pub mod auth;
pub mod auth_flows;
pub mod authorization;
pub mod broadcasting;
pub mod bus;
pub mod cache;
pub mod config;
pub mod console;
pub mod container;
pub mod context;
pub mod cors;
pub mod crypto;
pub mod csrf;
pub mod data;
pub mod database;
pub mod eloquent;
pub mod error;
pub mod events;
pub mod factory;
pub mod features;
pub mod filesystem;
pub mod hashing;
pub mod http;
pub mod http_client;
pub mod idempotency;
pub mod inertia;
pub(crate) mod lock;
pub mod logging;
pub mod mail;
pub mod middleware;
pub mod notifications;
pub mod pagination;
pub mod payments;
pub mod prelude;
pub mod queue;
pub mod rate_limit;
pub mod resources;
pub mod routing;
pub mod schedule;
pub mod seed;
pub mod server;
pub mod session;
pub mod sse;
pub mod supervisor;
pub mod telemetry;
pub mod testing;
pub mod timeout;
pub mod torii_integration;
pub mod validation;
pub mod vector;
pub mod web_push;
pub mod workflow;
pub mod ws;

extern crate self as suprnova;

pub use app::Application;
/// The Suprnova framework version (the `suprnova` crate version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub use app::maintenance::{
    CacheMaintenanceMode, FileMaintenanceMode, MaintenanceMiddleware, MaintenanceMode,
    MaintenancePayload, maintenance_mode,
};
pub use app::paths::{
    base_path, config_path, database_path, lang_path, public_path, resource_path, set_base_path,
    storage_path, use_config_path, use_database_path, use_lang_path, use_public_path,
    use_resource_path, use_storage_path,
};
pub use auth::{
    Auth, AuthConfig, AuthManager, AuthMiddleware, Authenticatable, BasicAuthMiddleware,
    Credentials, DatabaseUserProvider, EloquentUserProvider, GenericUser, Guard, GuardConfig,
    GuardDriver, GuestMiddleware, SessionGuard, StatefulGuard, TokenGuard, UserProvider,
};
pub use authorization::{Authorizable, Gate};
// The crate root binds `Response` to the HTTP response contract, so the
// authorization decision type is exported here under an unambiguous alias.
// Its Laravel-spelled home is `suprnova::authorization::Response`.
pub use authorization::Response as GateResponse;
pub use cache::{Cache, CacheConfig, CacheStore, InMemoryCache, LockGuard, RedisCache};
pub use config::{AppConfig, Config, Environment, ServerConfig, env, env_optional, env_required};
pub use container::{App, Container};
pub use context::{Context, ContextStore};
pub use crypto::{Crypt, EncryptionKey};
pub use csrf::{CsrfMiddleware, OriginPolicy, csrf_field, csrf_meta_tag, csrf_token};
pub use data::{
    Field, IncludeError, IncludeMiddleware, IsRelationLoaded, RequestIncludeSet,
    current_include_set, scope_include_set, with_include_overrides,
};
pub use database::{
    AutoRouteBinding, ConnectionEstablished, ConnectionRegistry, DB, Database, DatabaseBusy,
    DatabaseConfig, DatabaseType, DbConnection, DbTableBuilder, DynamicRow, EntityExt,
    EntityExtMut, PRIMARY_CONNECTION_NAME, QueryExecuted, QueryListener,
    READ_REPLICA_CONNECTION_NAME, ReadWriteType, RouteBinding, Transaction, TransactionBeginning,
    TransactionCommitted, TransactionRolledBack, TxHandle, UrlSource,
};
pub use torii_integration::{
    LockoutStatus, Session, SessionToken, ToriiConfig, User, UserId, init_torii,
    middleware::BearerTokenMiddleware,
};

// SeaORM type aliasing — Suprnova design principle #4: SeaORM is an
// implementation detail; consumers reach for `suprnova::*` and never
// `use sea_orm::*`. Every type a user would name in handler / model /
// migration code is re-exported here.
pub use sea_orm::sea_query;
pub use sea_orm::strum::IntoEnumIterator as Iterable;
pub use sea_orm::{
    ActiveModelBehavior, ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait,
    DatabaseConnection, DatabaseTransaction, DbErr, DeriveActiveEnum, EntityName, EntityTrait,
    Iden, IntoActiveModel, ModelTrait, NotSet, PrimaryKeyToColumn, PrimaryKeyTrait, QueryFilter,
    QueryOrder, QuerySelect, RelationDef, RelationTrait, Schema, Select, Set, SqlErr,
    TransactionTrait, TryGetable,
};

// Top-level escape hatch (spec 02-seaorm-aliasing §Rationale): the full
// `sea_orm` module is reachable as `suprnova::sea_orm::*` so users who
// need a type we haven't aliased can still get to it without adding
// `sea_orm` to their Cargo.toml. The aliased names above remain the
// documented surface; this is the "I know what I'm doing" path.
pub use ::sea_orm;
pub use broadcasting::{
    BroadcastEnvelope, BroadcastHub, BroadcastListener, Broadcastable, BroadcastingWsHandler,
    InMemoryBroadcastHub,
};
pub use bus::{Bus, Dispatched};
pub use console::{CommandEntry, CommandHandler, TypedCommand, dispatch_argv};
pub use cors::{AllowedHeaders, AllowedOrigins, CorsConfig, CorsMiddleware};
pub use error::{AppError, FrameworkError, HttpError, ValidationErrors};
pub use events::{
    ErrorOccurred, Event, EventDispatcher, EventFacade, EventFakeGuard, Listener, QueuedListener,
    Subscriber,
};
pub use factory::{Factory, FactoryBuilder, Persistable, Sequence, persist_via_seaorm};
pub use filesystem::{
    AzBlobConfig, ChecksumAlgorithm, DiskExt, GcsConfig, S3Config, Storage, copy_between_disks,
};
pub use hashing::{
    Algorithm as HashAlgorithm, Argon2Options, Argon2iHasher, Argon2idHasher, BcryptHasher,
    BcryptOptions, DEFAULT_COST as HASH_DEFAULT_COST, DEFAULT_ROUNDS as HASH_DEFAULT_ROUNDS,
    HashConfig, HashInfo, Hasher, MAX_BCRYPT_PASSWORD_BYTES, hash, info as hash_info, is_hashed,
    needs_rehash, verify,
};
pub use http::body::{
    DEFAULT_MAX_REQUEST_BODY_BYTES, collect_body_with_cap, global_max_request_body_bytes,
    set_global_max_request_body_bytes,
};
pub use http::upload::validators::{Image, MaxSize, MimeAllowlist, MimeType};
pub use http::upload::{
    DEFAULT_MAX_MULTIPART_BODY_BYTES, DEFAULT_MAX_MULTIPART_PARTS, DEFAULT_UPLOAD_SPILL_THRESHOLD,
    MultipartLimits, MultipartPayload, MultipartRequestHooks, MultipartValue, UploadedFile,
    UploadedFileBacking, global_max_multipart_body_bytes, global_max_multipart_parts,
    global_upload_spill_threshold, parse_multipart_streaming, parse_multipart_streaming_with_cap,
    parse_multipart_streaming_with_limits, set_global_max_multipart_body_bytes,
    set_global_max_multipart_parts, set_global_upload_spill_threshold,
    upload_tempfiles_spilled_total,
};
pub use http::{
    Cookie, CookieOptions, FormRequest, FromParam, FromRequest, HttpResponse, Redirect,
    RedirectRouteBuilder, Request, Response, ResponseExt, SameSite, abort_if, abort_unless,
    abort_with, json, text,
};
pub use http_client::{
    ClientResponse, FailOnRealCallsGuard, Http, RecordedRequest, RequestBuilder, assert_not_sent,
    assert_sent, fake_response,
};
pub use idempotency::{Idempotency, Idempotent, Replay};
pub use inertia::{
    DeferConfig, DeferOptions, EncryptHistoryMiddleware, Frontend, Inertia, Inertia303Middleware,
    InertiaConfig, InertiaRegistry, InertiaRequestExt, InertiaResponse, InertiaSharedData,
    InertiaVersionMiddleware, MergeConfig, MergeStrategy, OnceConfig, OnceOptions, PartialFilter,
    Prop, PropFuture, PropResolver, ScrollConfig, ScrollMetadata, VersionResolver,
};
pub use logging::{
    LogConfig, LogFormat, RequestId, RequestIdMiddleware, current_request_id, init_subscriber,
    spawn_with_request_id,
};
pub use middleware::{
    Middleware, MiddlewareFactory, MiddlewareFuture, MiddlewareRegistry, MiddlewareResolveError,
    Next, Pipeline, Terminable, TerminationSnapshot, append_middleware_priority,
    clear_middleware_alias, clear_middleware_group, dispatch_termination, get_global_middleware,
    global_middleware_count, has_global_middleware, has_middleware_alias, has_middleware_group,
    has_terminable, middleware_priority, prepend_global_middleware, prepend_middleware_priority,
    register_global_middleware, register_middleware_alias, register_middleware_group,
    register_terminable, registered_middleware_aliases, registered_middleware_groups,
    registered_terminables, resolve_middleware_alias, resolve_middleware_group, terminable_count,
};
pub use pagination::{
    CursorDirection, CursorPaginator, IntoInertiaScroll, LengthAwarePaginator, Paginated,
    Pagination, Paginator,
};
pub use queue::{
    BackoffSchedule, Batch, BatchCallback, BatchOptions, BatchRepository, ChainLink,
    DatabaseFailedJobStore, DatabaseQueueDriver, Envelope, EnvelopeError, FailOnException,
    FailedJob, FailedJobStore, Job, JobMiddleware, JobMiddlewareNext, JobOutcome, ManuallyFailed,
    MaxAttemptsExceeded, MemoryBatchRepository, MemoryFailedJobStore, MemoryQueueDriver,
    NullFailedJobStore, NullQueueDriver, PendingBatch, PendingChain, Queue, QueueDriver,
    RateLimited, RedisQueueDriver, Reservation, ReservationToken, Skip, SkipIfBatchCancelled,
    SyncQueueDriver, ThrottlesExceptions, TimeoutExceeded, UpdatedBatchJobCounts,
    WithoutOverlapping,
};
pub use rate_limit::{
    BackendErrorPolicy, GlobalLimit, Limit, LimitResult, RateLimitMiddleware, RateLimiter,
    RateLimiterDriver, SlidingWindowConfig, ThrottleRequestsMiddleware, Unlimited,
};
pub use resources::{
    AsRelationshipValue, IncludeResolutionError, IncludeTree, IntoJsonResource, JsonApi,
    JsonApiBuilder, JsonApiInfo, JsonApiResponse, Maybe, MissingValue, PushIncluded,
    RelationshipValue, RequestFieldsetSet, Resource, ResourceIdentifier, current_fieldset,
    insert_maybe, scope_fieldset, strip_missing_values,
};
pub use routing::{
    // Internal functions used by macros (hidden from docs)
    __any_impl,
    __delete_impl,
    __fallback_impl,
    __get_impl,
    __head_impl,
    __options_impl,
    __patch_impl,
    __post_impl,
    __put_impl,
    __ws_impl,
    FallbackDefBuilder,
    GroupBuilder,
    GroupDef,
    GroupItem,
    GroupRoute,
    GroupRouter,
    IntoGroupItem,
    ResourceAction,
    ResourceController,
    ResourceRoutes,
    RouteBuilder,
    RouteDefBuilder,
    Router,
    SignatureVerdict,
    WsRouteDef,
    clear_route_names_for_test,
    redirect,
    redirect_to,
    route,
    sign_route,
    sign_url,
    url,
    validate_route_path,
    verify_signature,
};
pub use schedule::{CronExpression, DayOfWeek, Schedule, Task, TaskBuilder, TaskEntry, TaskResult};
pub use seed::Seeder;
pub use server::{Server, handle_request, handle_request_with_peer};
pub use session::{
    DatabaseSessionDriver, SessionConfig, SessionData, SessionMiddleware, SessionStore,
    auth_user_id, clear_auth_user, destroy_all_for_user, generate_csrf_token, generate_session_id,
    get_csrf_token, invalidate_session, is_authenticated, is_valid_session_id,
    regenerate_csrf_token, regenerate_session_id, session, session_mut, set_auth_user,
};
pub use sse::SseEvent;
pub use supervisor::{RestartPolicy, Supervisor, SupervisorEntry, SupervisorRegistry};
pub use telemetry::{
    CounterHandle, GaugeHandle, HistogramHandle, Metrics, OtelConfig, TelemetryGuard,
    init_telemetry,
};
pub use timeout::TimeoutMiddleware;
pub use validation::rule::{
    AsyncRule, ContextualRule, FormContext, Rule, Unique, async_rules, rules,
    rules::{
        Alpha, AlphaNum, Between, Boolean, Confirmed, Different, Email, HttpUrl, In, Integer, Max,
        Min, NotIn, Numeric, Required, RequiredIf, RequiredUnless, RequiredWith, Same, Url, Uuid,
    },
};
#[cfg(feature = "vector-pinecone")]
pub use vector::PineconeVectorDriver;
pub use vector::{
    MariaDbDistance, MariaDbVectorDriver, MemoryVectorDriver, QdrantDistance, QdrantVectorDriver,
    SUPRNOVA_ID_PAYLOAD_KEY, Vector, VectorDriver, VectorItem, VectorMatch, VectorRegistry,
    VectorStore,
};
pub use web_push::{
    ContentEncoding, EndpointPolicy, PushResponse, SubscriptionInfo, VapidClaims, VapidKey,
    VapidSigner, WebPushClient, WebPushError,
};
pub use workflow::{
    StepStatus, WorkflowConfig, WorkflowContext, WorkflowHandle, WorkflowStatus, WorkflowWorker,
    start_named,
};
// Phase 12 — payments. Money + Currency are the foundational primitives;
// every payment DTO builds on them. Re-exported at the crate root so
// consumers write `suprnova::Money` / `suprnova::Currency`.
pub use payments::{
    Currency, MockPaymentProvider, Money, PaymentProviderEntry, PaymentProviderRegistry,
};

pub use auth_flows::{
    BruteForce, EmailVerification, EmailVerificationMail, EnrollmentResponse,
    EnsureEmailVerifiedMiddleware, LoginThrottleMiddleware, PasswordChangedMail, PasswordReset,
    PasswordResetLinkSent, PasswordResetMail, TwoFactor, TwoFactorChallengeFailed,
    TwoFactorChallengeMiddleware, TwoFactorChallenged, TwoFactorUser,
};
#[doc(hidden)]
pub use clap as __clap;
pub use mail::{
    Address, Attachment, Mail, MailBuilder, MailFake, Mailable, MessageSending, MessageSent,
    OutgoingMessage, QueuedSnapshot, SendMailJob,
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
pub use featureflag::{feature, is_enabled};
pub use features::{Evaluator, EvaluatorRef, Feature};
// Phase 10 — Eloquent. Foundation primitives land in 10A; relationships
// (10B) and collections/pagination/observers (10C) extend the same
// `eloquent` module. The `ModelEntry` registry is populated at compile
// time by `#[suprnova::model]` (Task 3) and walked at boot by Phase 8
// (Admin), `model:prune`, and future tooling.
pub use eloquent::{
    AggregateKind, AsArray, AsArrayObject, AsBool, AsCollection, AsDate, AsDateTime, AsDecimal,
    AsEncrypted, AsEncryptedArray, AsEncryptedCollection, AsEncryptedObject, AsEnum, AsFloat,
    AsHashed, AsImmutableDate, AsImmutableDateTime, AsInt, AsJson, AsObject, AsOptionalDateTime,
    AsString, AsTimestamp, Attrs, BelongsTo, BelongsToMany, Builder, Cast, Collection, Direction,
    DynCast, EagerLoadCache, EagerLoadDispatch, EloquentModel, Fillable, FirstOrCreate,
    GlobalScope, HasMany, HasManyThrough, HasOne, HasOneThrough, IntoColumn, IntoDynCast, IntoVal,
    LazyCollection, MassPrunable, Model, ModelEntry, MorphMany, MorphOne, MorphTo, MorphToMany,
    MorphTypeEntry, MorphedByMany, Prunable, PrunerEntry, Relation, RelationEntry, RelationKind,
    ReplicateExt, ScopeRegistry, SoftDeletes, Touchable, find_model_by_table, find_morph_type,
    find_morph_type_by_id, find_relation, models, morph_types, prune_all, prune_all_dry, prune_one,
    relations, relations_of, unguarded,
};
// Phase 10C T1 — model lifecycle events. The 16 per-type event
// structs (`Created`, `Saving`, ...) are macro-emitted into each
// model's `events::` submodule; the cross-model shared types
// (`EventResult`, listener traits, dispatch helpers) re-export here
// so user code reaches them as `suprnova::EventResult`,
// `suprnova::CancellableListener`, etc.
pub use eloquent::events::{
    CancellableListener, EventResult, ModelEventHooks, dispatch_after, dispatch_cancellable,
    listen_cancellable,
};
// Phase 10C T2a — lifecycle observers. Users implement `Observer<M>`
// on their observer struct (zero-sized or `Arc`-clonable); the
// `#[suprnova::observer(M)]` macro (T2b) walks the impl block and
// registers per-method listeners through the `ObserverEntry` inventory.
// `bootstrap_observers` drains the inventory at startup.
pub use eloquent::observers::{
    Observer, ObserverEntry, ObserverInstallFuture, bootstrap_observers,
};
// `casts!` macro is `#[macro_export]` in eloquent/casts/mod.rs — re-exported
// at the crate root automatically. No `pub use` needed here.
pub use notifications::channels::broadcast::BroadcastChannel;
pub use notifications::channels::database::DatabaseChannel;
pub use notifications::channels::mail::{
    MailChannel, MailRendering, NotificationMailable, register_mail_renderer,
};
pub use notifications::channels::webpush::WebPushChannel;
pub use notifications::{
    AnonymousNotifiable, Channel, DynNotification, Notifiable, Notification,
    NotificationDispatcher, NotificationFactory, NotificationFailed, NotificationSending,
    NotificationSent, Notify, NotifyFakeGuard, SendNotificationJob, StoredNotification,
};

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
pub use suprnova_macros::Command;
pub use suprnova_macros::Data;
pub use suprnova_macros::FormRequest as FormRequestDerive;
pub use suprnova_macros::InertiaProps;
pub use suprnova_macros::accessor;
pub use suprnova_macros::command;
pub use suprnova_macros::domain_error;
pub use suprnova_macros::handler;
pub use suprnova_macros::inertia_response;
pub use suprnova_macros::injectable;
pub use suprnova_macros::model;
pub use suprnova_macros::mutator;
pub use suprnova_macros::observer;
pub use suprnova_macros::policy;
pub use suprnova_macros::prunable;
pub use suprnova_macros::redirect;
pub use suprnova_macros::request;
pub use suprnova_macros::scopes;
pub use suprnova_macros::service;
pub use suprnova_macros::workflow;
pub use suprnova_macros::workflow_step;
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
/// Registration is idempotent per middleware type — registering the same
/// type twice keeps the first registration — so re-running bootstrap won't
/// double-run a global middleware. Register every global BEFORE the server
/// is constructed: the server snapshots the registry at build time, so a
/// `global_middleware!` call made after `Server::from_config` / `Server::new`
/// does not retroactively apply to that server.
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
