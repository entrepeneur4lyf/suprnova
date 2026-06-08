//! Middleware aliases, named groups, and priority ordering.
//!
//! Mirrors three Laravel kernel surfaces that previously had no Suprnova
//! analogue:
//!
//! - **`middlewareAliases`** — string-keyed lookups so consumers can refer
//!   to `"auth"` / `"throttle"` instead of a fully-qualified type. Laravel
//!   uses the alias map in `Kernel::$middlewareAliases`.
//! - **`middlewareGroups`** — string-keyed bundles of middleware that
//!   expand at resolution time. Laravel's `web` and `api` groups are the
//!   canonical examples.
//! - **`middlewarePriority`** — an ordered list of TypeIds the registry
//!   should sort to the front of the chain regardless of registration
//!   order. The Laravel kernel ships a built-in priority list ensuring
//!   `SubstituteBindings` always runs after `StartSession`, etc.
//!
//! These three registries are intentionally separate from
//! [`MiddlewareRegistry`] — they're lookup tables, not execution slots.
//! The registry consults them at boot time when materialising its global
//! chain. They are also process-global so the bootstrap macros can write
//! into them without having to thread a config object through.

use super::{BoxedMiddleware, Middleware, into_boxed};
use std::any::TypeId;
use std::sync::{OnceLock, RwLock};

/// A factory closure that produces a fresh `BoxedMiddleware`. Used by
/// the alias and group registries because a `Middleware: 'static` trait
/// object can't be cheaply cloned for repeated registrations — we
/// instantiate per registration site via a factory instead.
pub type MiddlewareFactory = std::sync::Arc<dyn Fn() -> BoxedMiddleware + Send + Sync>;

/// Stored shape of the alias registry — extracted to a `type` alias so
/// the `OnceLock<RwLock<...>>` declaration below doesn't trip
/// `clippy::type_complexity`.
type AliasMap = Vec<(String, MiddlewareFactory)>;

/// Stored shape of the named-group registry. Each entry maps a group
/// name to its ordered list of alias names. Aliased for the same
/// type-complexity reason as [`AliasMap`].
type GroupMap = Vec<(String, Vec<String>)>;

/// Process-global alias registry. `(name, factory)` pairs.
static ALIAS_REGISTRY: OnceLock<RwLock<AliasMap>> = OnceLock::new();

/// Process-global named-group registry. `(group_name, Vec<alias_names>)`.
static GROUP_REGISTRY: OnceLock<RwLock<GroupMap>> = OnceLock::new();

/// Process-global middleware priority list (TypeIds). Order matters: the
/// first TypeId is sorted to the front of the chain.
static PRIORITY_REGISTRY: OnceLock<RwLock<Vec<TypeId>>> = OnceLock::new();

fn alias_lock() -> &'static RwLock<AliasMap> {
    ALIAS_REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

fn group_lock() -> &'static RwLock<GroupMap> {
    GROUP_REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

fn priority_lock() -> &'static RwLock<Vec<TypeId>> {
    PRIORITY_REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

/// Register a named middleware alias.
///
/// Equivalent to Laravel's
/// `Kernel::$middlewareAliases['auth' => AuthMiddleware::class]`. The
/// alias is the lookup key; the closure produces a fresh boxed
/// middleware on demand. The factory is invoked once per
/// [`resolve_middleware_alias`] / [`resolve_middleware_group`] hit, so
/// per-route registration produces independent instances.
///
/// Registration is **last-wins** for the same name — re-registering an
/// alias swaps the factory rather than panicking, mirroring Laravel's
/// reassignable kernel array. This keeps test setup and hot-reload
/// flows simple.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::middleware::register_middleware_alias;
///
/// register_middleware_alias("auth", || AuthMiddleware);
/// ```
pub fn register_middleware_alias<F, M>(name: &str, factory: F)
where
    F: Fn() -> M + Send + Sync + 'static,
    M: Middleware + 'static,
{
    let factory: MiddlewareFactory = std::sync::Arc::new(move || into_boxed(factory()));
    let lock = alias_lock();
    let mut guard = match lock.write() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let name_owned = name.to_string();
    if let Some(slot) = guard.iter_mut().find(|(n, _)| n == &name_owned) {
        slot.1 = factory;
    } else {
        guard.push((name_owned, factory));
    }
}

/// Look up a registered alias and produce a fresh `BoxedMiddleware` from
/// its factory. Returns `None` if no alias with that name was registered.
///
/// Used by the router / group builder when consumers refer to middleware
/// by name (`.middleware_alias("auth")`) instead of by type.
pub fn resolve_middleware_alias(name: &str) -> Option<BoxedMiddleware> {
    let lock = alias_lock();
    let guard = match lock.read() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, factory)| factory())
}

/// Whether an alias by this name has been registered.
pub fn has_middleware_alias(name: &str) -> bool {
    let lock = alias_lock();
    let guard = match lock.read() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.iter().any(|(n, _)| n == name)
}

/// All currently-registered alias names (snapshot). Order matches
/// registration order. Useful for diagnostic CLI surfaces.
pub fn registered_middleware_aliases() -> Vec<String> {
    let lock = alias_lock();
    let guard = match lock.read() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.iter().map(|(n, _)| n.clone()).collect()
}

/// Remove a registered alias by name. Idempotent — returns `true` if a
/// binding was removed, `false` if no such alias existed. Exposed so
/// tests and hot-reload tooling can teardown cleanly.
pub fn clear_middleware_alias(name: &str) -> bool {
    let lock = alias_lock();
    let mut guard = match lock.write() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let before = guard.len();
    guard.retain(|(n, _)| n != name);
    before != guard.len()
}

/// Wipe every registered alias. Test-only convenience; the production
/// boot path never needs this.
#[doc(hidden)]
pub fn clear_all_middleware_aliases_for_test() {
    let lock = alias_lock();
    let mut guard = match lock.write() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.clear();
}

/// Define a named middleware group as a list of alias names.
///
/// Mirrors Laravel's `Kernel::$middlewareGroups['web' => [EncryptCookies::class, ...]]`.
/// Each entry in `aliases` must resolve via
/// [`resolve_middleware_alias`] when the group is consulted — calling
/// [`resolve_middleware_group`] on a group whose entries can't be
/// resolved returns an `Err` listing the missing names.
///
/// Registration is last-wins for the same group name. Recursive groups
/// (a group referencing another group) ARE supported via a single
/// pass — see [`resolve_middleware_group`].
pub fn register_middleware_group(name: &str, aliases: impl IntoIterator<Item = String>) {
    let aliases: Vec<String> = aliases.into_iter().collect();
    let lock = group_lock();
    let mut guard = match lock.write() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let name_owned = name.to_string();
    if let Some(slot) = guard.iter_mut().find(|(n, _)| n == &name_owned) {
        slot.1 = aliases;
    } else {
        guard.push((name_owned, aliases));
    }
}

/// Errors that can surface from group resolution.
#[derive(Debug, PartialEq, Eq)]
pub enum MiddlewareResolveError {
    /// The group itself is not registered.
    UnknownGroup(String),
    /// The group exists but references an alias that wasn't registered.
    UnknownAlias {
        /// Name of the group whose definition references the missing alias.
        group: String,
        /// The alias name that couldn't be resolved.
        missing: String,
    },
    /// The group references another group that doesn't exist.
    UnknownNestedGroup {
        /// Name of the outer group whose definition references the missing group.
        group: String,
        /// The nested group name that couldn't be resolved.
        missing: String,
    },
    /// A nested group references itself (direct or via a chain). Detected
    /// so we don't loop forever on a misconfigured group definition.
    CycleDetected {
        /// Name of the group at which the cycle was detected.
        group: String,
    },
}

impl std::fmt::Display for MiddlewareResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownGroup(name) => write!(f, "unknown middleware group: '{name}'"),
            Self::UnknownAlias { group, missing } => {
                write!(
                    f,
                    "middleware group '{group}' references unknown alias '{missing}'"
                )
            }
            Self::UnknownNestedGroup { group, missing } => {
                write!(
                    f,
                    "middleware group '{group}' references unknown nested group '{missing}'"
                )
            }
            Self::CycleDetected { group } => {
                write!(f, "middleware group '{group}' contains a cyclic reference")
            }
        }
    }
}

impl std::error::Error for MiddlewareResolveError {}

/// Resolve a registered group into a flat list of `BoxedMiddleware`,
/// expanding nested group references along the way.
///
/// Nested groups: an entry in a group's alias list whose name matches a
/// registered group is recursively expanded. Cycle detection prevents
/// infinite recursion on a misconfigured definition.
pub fn resolve_middleware_group(
    name: &str,
) -> Result<Vec<BoxedMiddleware>, MiddlewareResolveError> {
    let mut visited: Vec<String> = Vec::new();
    let mut out: Vec<BoxedMiddleware> = Vec::new();
    resolve_group_inner(name, &mut visited, &mut out)?;
    Ok(out)
}

fn resolve_group_inner(
    name: &str,
    visited: &mut Vec<String>,
    out: &mut Vec<BoxedMiddleware>,
) -> Result<(), MiddlewareResolveError> {
    if visited.iter().any(|v| v == name) {
        return Err(MiddlewareResolveError::CycleDetected {
            group: name.to_string(),
        });
    }
    visited.push(name.to_string());

    let aliases = {
        let lock = group_lock();
        let guard = match lock.read() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    let aliases = aliases.ok_or_else(|| MiddlewareResolveError::UnknownGroup(name.to_string()))?;

    for entry in aliases {
        // Group reference takes precedence over alias lookup so nesting
        // works when both registries share a name (Laravel resolves the
        // same way).
        if is_registered_group(&entry) {
            // Recurse — but pass through any UnknownAlias / nested error.
            resolve_group_inner(&entry, visited, out).map_err(|e| match e {
                MiddlewareResolveError::UnknownGroup(missing) => {
                    MiddlewareResolveError::UnknownNestedGroup {
                        group: name.to_string(),
                        missing,
                    }
                }
                other => other,
            })?;
            continue;
        }
        let resolved = resolve_middleware_alias(&entry).ok_or_else(|| {
            MiddlewareResolveError::UnknownAlias {
                group: name.to_string(),
                missing: entry.clone(),
            }
        })?;
        out.push(resolved);
    }
    Ok(())
}

fn is_registered_group(name: &str) -> bool {
    let lock = group_lock();
    let guard = match lock.read() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.iter().any(|(n, _)| n == name)
}

/// Whether a group by this name has been registered.
pub fn has_middleware_group(name: &str) -> bool {
    is_registered_group(name)
}

/// All currently-registered group names (snapshot). Order matches
/// registration order.
pub fn registered_middleware_groups() -> Vec<String> {
    let lock = group_lock();
    let guard = match lock.read() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.iter().map(|(n, _)| n.clone()).collect()
}

/// Remove a registered group by name. Returns `true` if a binding was
/// removed.
pub fn clear_middleware_group(name: &str) -> bool {
    let lock = group_lock();
    let mut guard = match lock.write() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let before = guard.len();
    guard.retain(|(n, _)| n != name);
    before != guard.len()
}

/// Wipe every registered group. Test-only convenience.
#[doc(hidden)]
pub fn clear_all_middleware_groups_for_test() {
    let lock = group_lock();
    let mut guard = match lock.write() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.clear();
}

/// Prepend a middleware type to the priority list. Types earlier in
/// the list sort to the front of the chain. Laravel's
/// `prependToMiddlewarePriority`.
pub fn prepend_middleware_priority<M: Middleware + 'static>() {
    let tid = TypeId::of::<M>();
    let lock = priority_lock();
    let mut guard = match lock.write() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if !guard.contains(&tid) {
        guard.insert(0, tid);
    }
}

/// Append a middleware type to the priority list. Laravel's
/// `appendToMiddlewarePriority`.
pub fn append_middleware_priority<M: Middleware + 'static>() {
    let tid = TypeId::of::<M>();
    let lock = priority_lock();
    let mut guard = match lock.write() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if !guard.contains(&tid) {
        guard.push(tid);
    }
}

/// Read the current priority list as a snapshot of TypeIds.
pub fn middleware_priority() -> Vec<TypeId> {
    let lock = priority_lock();
    let guard = match lock.read() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.clone()
}

/// Wipe the priority list. Test-only convenience.
#[doc(hidden)]
pub fn clear_middleware_priority_for_test() {
    let lock = priority_lock();
    let mut guard = match lock.write() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{Request, Response};
    use crate::middleware::{Middleware, Next};
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// Tests touch the process-global registries, so they all share
    /// this serial group to keep snapshot assertions reproducible.
    static SERIAL_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct AuthMw;
    #[async_trait]
    impl Middleware for AuthMw {
        async fn handle(&self, request: Request, next: Next) -> Response {
            next(request).await
        }
    }

    struct ThrottleMw;
    #[async_trait]
    impl Middleware for ThrottleMw {
        async fn handle(&self, request: Request, next: Next) -> Response {
            next(request).await
        }
    }

    struct CorsMw;
    #[async_trait]
    impl Middleware for CorsMw {
        async fn handle(&self, request: Request, next: Next) -> Response {
            next(request).await
        }
    }

    fn reset_all() {
        clear_all_middleware_aliases_for_test();
        clear_all_middleware_groups_for_test();
        clear_middleware_priority_for_test();
    }

    #[test]
    fn aliases_register_resolve_and_clear() {
        let _guard = SERIAL_TEST_LOCK.lock().unwrap();
        reset_all();

        assert!(!has_middleware_alias("auth"));
        register_middleware_alias("auth", || AuthMw);
        assert!(has_middleware_alias("auth"));
        assert!(resolve_middleware_alias("auth").is_some());
        assert!(resolve_middleware_alias("missing").is_none());
        assert_eq!(registered_middleware_aliases(), vec!["auth".to_string()]);

        assert!(clear_middleware_alias("auth"));
        assert!(!has_middleware_alias("auth"));
        assert!(!clear_middleware_alias("auth"));
    }

    #[test]
    fn aliases_re_registration_is_last_wins() {
        let _guard = SERIAL_TEST_LOCK.lock().unwrap();
        reset_all();

        register_middleware_alias("auth", || AuthMw);
        register_middleware_alias("auth", || ThrottleMw);
        // Still one alias under the same name.
        assert_eq!(registered_middleware_aliases().len(), 1);
        // Resolution succeeds — the second registration won.
        assert!(resolve_middleware_alias("auth").is_some());
    }

    #[test]
    fn group_expands_to_underlying_aliases() {
        let _guard = SERIAL_TEST_LOCK.lock().unwrap();
        reset_all();

        register_middleware_alias("auth", || AuthMw);
        register_middleware_alias("throttle", || ThrottleMw);
        register_middleware_group("api", ["auth".to_string(), "throttle".to_string()]);
        let mws = resolve_middleware_group("api").expect("api group resolves");
        assert_eq!(mws.len(), 2);
    }

    #[test]
    fn group_with_missing_alias_errors_with_name() {
        let _guard = SERIAL_TEST_LOCK.lock().unwrap();
        reset_all();

        register_middleware_alias("auth", || AuthMw);
        register_middleware_group("api", ["auth".to_string(), "throttle".to_string()]);
        let err = resolve_middleware_group("api")
            .err()
            .expect("resolve must err");
        match err {
            MiddlewareResolveError::UnknownAlias { group, missing } => {
                assert_eq!(group, "api");
                assert_eq!(missing, "throttle");
            }
            other => panic!("expected UnknownAlias, got {other:?}"),
        }
    }

    #[test]
    fn group_unknown_returns_unknown_group_error() {
        let _guard = SERIAL_TEST_LOCK.lock().unwrap();
        reset_all();

        let err = resolve_middleware_group("ghost")
            .err()
            .expect("resolve must err");
        assert_eq!(
            err,
            MiddlewareResolveError::UnknownGroup("ghost".to_string())
        );
    }

    #[test]
    fn group_supports_nested_groups() {
        let _guard = SERIAL_TEST_LOCK.lock().unwrap();
        reset_all();

        register_middleware_alias("auth", || AuthMw);
        register_middleware_alias("throttle", || ThrottleMw);
        register_middleware_alias("cors", || CorsMw);
        register_middleware_group("base", ["auth".to_string()]);
        register_middleware_group(
            "api",
            [
                "base".to_string(),
                "throttle".to_string(),
                "cors".to_string(),
            ],
        );

        let mws = resolve_middleware_group("api").expect("api resolves");
        assert_eq!(mws.len(), 3);
    }

    #[test]
    fn group_cycle_detected() {
        let _guard = SERIAL_TEST_LOCK.lock().unwrap();
        reset_all();

        register_middleware_group("a", ["b".to_string()]);
        register_middleware_group("b", ["a".to_string()]);
        let err = resolve_middleware_group("a")
            .err()
            .expect("resolve must err");
        match err {
            MiddlewareResolveError::CycleDetected { group } => {
                assert!(group == "a" || group == "b");
            }
            other => panic!("expected CycleDetected, got {other:?}"),
        }
    }

    #[test]
    fn group_lookup_introspection() {
        let _guard = SERIAL_TEST_LOCK.lock().unwrap();
        reset_all();

        assert!(!has_middleware_group("web"));
        register_middleware_group("web", Vec::<String>::new());
        assert!(has_middleware_group("web"));
        assert_eq!(registered_middleware_groups(), vec!["web".to_string()]);
        assert!(clear_middleware_group("web"));
        assert!(!has_middleware_group("web"));
    }

    #[test]
    fn priority_appends_unique() {
        let _guard = SERIAL_TEST_LOCK.lock().unwrap();
        reset_all();

        append_middleware_priority::<AuthMw>();
        append_middleware_priority::<ThrottleMw>();
        // Duplicate append is a no-op.
        append_middleware_priority::<AuthMw>();

        let snapshot = middleware_priority();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0], TypeId::of::<AuthMw>());
        assert_eq!(snapshot[1], TypeId::of::<ThrottleMw>());
    }

    #[test]
    fn priority_prepend_lifts_to_front() {
        let _guard = SERIAL_TEST_LOCK.lock().unwrap();
        reset_all();

        append_middleware_priority::<AuthMw>();
        prepend_middleware_priority::<ThrottleMw>();
        let snapshot = middleware_priority();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0], TypeId::of::<ThrottleMw>());
        assert_eq!(snapshot[1], TypeId::of::<AuthMw>());
    }
}
