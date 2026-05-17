//! Typed config resolution from environment variables via [`envy`].
//!
//! `Config::resolve::<MyConfig>()` deserializes the process's
//! environment into a typed struct. Field names map to env vars
//! UPPER_SNAKE: a `pub mail_host: String` field reads `MAIL_HOST`.
//! `#[serde(default = "...")]` and `#[serde(rename = "...")]` work
//! as usual.
//!
//! [`resolve_prefixed`] reads only env vars sharing a prefix, with
//! the prefix stripped before mapping to fields:
//!
//! ```ignore
//! // env: APP_NAME=suprnova, APP_DEBUG=true
//! #[derive(Deserialize)]
//! struct AppCfg { name: String, debug: bool }
//! let cfg: AppCfg = Config::resolve_prefixed("APP_").unwrap();
//! assert_eq!(cfg.name, "suprnova");
//! ```
//!
//! Why not subsume the existing `Config::get::<T>()` repository?
//! The repository holds runtime-registered handles; `resolve` is a
//! one-shot env → struct deserializer. They cover different
//! lifecycles. Use `resolve` to build a struct from env, then
//! `Config::register(struct)` if you want it available via `get`
//! later.

use crate::error::FrameworkError;
use serde::de::DeserializeOwned;

/// Deserialize the current process's environment into `T` via envy.
///
/// Field name conversion follows envy's default — `pub mail_host` →
/// `MAIL_HOST`. Use `#[serde(rename = "...")]` to override a field's
/// env-var name explicitly, and `#[serde(default = "...")]` to give a
/// missing field a fallback.
pub fn resolve<T: DeserializeOwned>() -> Result<T, FrameworkError> {
    envy::from_env::<T>().map_err(|e| {
        FrameworkError::internal(format!(
            "config: failed to resolve {} from env: {e}",
            std::any::type_name::<T>()
        ))
    })
}

/// Like [`resolve`] but only considers env vars starting with
/// `prefix`. The prefix is stripped before mapping to struct fields,
/// so `prefix = "MAIL_"` + a `pub host` field reads `MAIL_HOST`.
pub fn resolve_prefixed<T: DeserializeOwned>(prefix: &str) -> Result<T, FrameworkError> {
    envy::prefixed(prefix).from_env::<T>().map_err(|e| {
        FrameworkError::internal(format!(
            "config: failed to resolve {} from env with prefix `{}`: {e}",
            std::any::type_name::<T>(),
            prefix
        ))
    })
}
