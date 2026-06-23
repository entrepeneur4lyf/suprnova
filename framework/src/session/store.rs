//! Session storage abstraction

use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;

use crate::error::FrameworkError;

/// Internal session key used by [`SessionData::flash_input`] for the
/// old-input bag. Pairs with [`SessionData::get_old_input`] and
/// [`SessionData::has_old_input`]. Mirrors Laravel's `_old_input`
/// convention (`Illuminate/Session/Store.php:553-556`).
const OLD_INPUT_KEY: &str = "_old_input";

/// Session data container
///
/// Holds all session data including user authentication state and CSRF token.
#[derive(Clone, Debug, Default)]
pub struct SessionData {
    /// Unique session identifier
    pub id: String,
    /// Key-value data stored in the session
    pub data: HashMap<String, serde_json::Value>,
    /// Authenticated user ID (if any)
    pub user_id: Option<String>,
    /// CSRF token for this session
    pub csrf_token: String,
    /// Whether the session has been modified
    pub dirty: bool,
}

impl SessionData {
    /// Create a new session with the given ID
    pub fn new(id: String, csrf_token: String) -> Self {
        Self {
            id,
            data: HashMap::new(),
            user_id: None,
            csrf_token,
            dirty: false,
        }
    }

    /// Get a value from the session
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use suprnova::session::SessionData;
    /// # let session = SessionData::new("sid".into(), "tok".into());
    /// let name: Option<String> = session.get("name");
    /// ```
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.data
            .get(key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Put a value into the session
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use suprnova::session::SessionData;
    /// # let mut session = SessionData::new("sid".into(), "tok".into());
    /// session.put("name", "John");
    /// session.put("count", 42);
    /// ```
    pub fn put<T: Serialize>(&mut self, key: &str, value: T) {
        if let Ok(v) = serde_json::to_value(value) {
            self.data.insert(key.to_string(), v);
            self.dirty = true;
        }
    }

    /// Remove a value from the session
    ///
    /// Returns the removed value if it existed.
    pub fn forget(&mut self, key: &str) -> Option<serde_json::Value> {
        let removed = self.data.remove(key);
        // Only dirty the session when something was actually removed —
        // forgetting an absent key must leave a read-only request clean so it
        // isn't forced through the write (and fail-closed) path needlessly.
        if removed.is_some() {
            self.dirty = true;
        }
        removed
    }

    /// Check if the session has a key
    pub fn has(&self, key: &str) -> bool {
        self.data.contains_key(key)
    }

    /// Flash a value to the session (available only for next request)
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use suprnova::session::SessionData;
    /// # let mut session = SessionData::new("sid".into(), "tok".into());
    /// session.flash("success", "Item saved successfully!");
    /// ```
    pub fn flash<T: Serialize>(&mut self, key: &str, value: T) {
        self.put(&format!("_flash.new.{}", key), value);
    }

    /// Get a flashed value (clears it after reading)
    pub fn get_flash<T: DeserializeOwned>(&mut self, key: &str) -> Option<T> {
        let flash_key = format!("_flash.old.{}", key);
        let value = self.get(&flash_key);
        if value.is_some() {
            self.forget(&flash_key);
        }
        value
    }

    /// Age flash data (move new flash to old, clear old)
    pub fn age_flash_data(&mut self) {
        // Remove old flash data
        let old_keys: Vec<String> = self
            .data
            .keys()
            .filter(|k| k.starts_with("_flash.old."))
            .cloned()
            .collect();
        let had_old = !old_keys.is_empty();
        for key in old_keys {
            self.data.remove(&key);
        }

        // Move new flash data to old
        let new_keys: Vec<String> = self
            .data
            .keys()
            .filter(|k| k.starts_with("_flash.new."))
            .cloned()
            .collect();
        let had_new = !new_keys.is_empty();
        for key in new_keys {
            if let Some(value) = self.data.remove(&key) {
                let old_key = key.replace("_flash.new.", "_flash.old.");
                self.data.insert(old_key, value);
            }
        }

        if had_new || had_old {
            self.dirty = true;
        }
    }

    /// Clear all session data (keeps ID and regenerates CSRF)
    pub fn flush(&mut self) {
        self.data.clear();
        self.user_id = None;
        self.dirty = true;
    }

    /// Check if the session has been modified
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the session as clean (after saving)
    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    // ------------------------------------------------------------------
    // Laravel `Store` facade completions — pure HashMap-level methods.
    // Each mirrors a `Illuminate/Session/Store.php` method by name.
    // ------------------------------------------------------------------

    /// Get a value and forget it in a single shot. Mirrors Laravel's
    /// `Store::pull($key, $default)` (`Store.php:345-348`). Non-atomic
    /// on every backend (same as Laravel — `Store::pull` itself reads
    /// `Arr::pull` synchronously on a PHP array).
    pub fn pull<T: DeserializeOwned>(&mut self, key: &str) -> Option<T> {
        let v = self.get::<T>(key);
        if v.is_some() {
            self.forget(key);
        }
        v
    }

    /// Append a value onto an array-shaped session value. Mirrors
    /// Laravel's `Store::push($key, $value)` (`Store.php:429-436`).
    ///
    /// Non-array existing values are overwritten with `[value]`.
    pub fn push<T: Serialize>(&mut self, key: &str, value: T) {
        let Ok(v) = serde_json::to_value(value) else {
            return;
        };
        let mut arr: Vec<serde_json::Value> = match self.data.get(key) {
            Some(serde_json::Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        arr.push(v);
        self.data
            .insert(key.to_string(), serde_json::Value::Array(arr));
        self.dirty = true;
    }

    /// Increment a numeric session value, creating it as `0 + amount`
    /// if missing. Mirrors Laravel's `Store::increment` (`Store.php:445-450`).
    pub fn increment(&mut self, key: &str, amount: i64) -> i64 {
        let cur: i64 = self.get(key).unwrap_or(0);
        let next = cur.saturating_add(amount);
        self.put(key, next);
        next
    }

    /// Decrement a numeric session value. Mirrors Laravel's
    /// `Store::decrement` (`Store.php:459-462`).
    pub fn decrement(&mut self, key: &str, amount: i64) -> i64 {
        self.increment(key, -amount)
    }

    /// Get-or-compute-and-put. Mirrors Laravel's `Store::remember`
    /// (`Store.php:411-420`). The closure runs only on cache miss; the
    /// computed value is persisted to the session and returned.
    pub fn remember<T, F>(&mut self, key: &str, default: F) -> T
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> T,
    {
        if let Some(v) = self.get::<T>(key) {
            return v;
        }
        let v = default();
        self.put(key, &v);
        v
    }

    /// Return true if any of the listed keys is present. Mirrors
    /// Laravel's `Store::hasAny` (`Store.php:319-324`).
    pub fn has_any(&self, keys: &[&str]) -> bool {
        keys.iter().any(|k| self.has(k))
    }

    /// Return true if every listed key is present. Mirrors Laravel's
    /// `Store::has` when passed an array (`Store.php:306-311`).
    pub fn has_all(&self, keys: &[&str]) -> bool {
        keys.iter().all(|k| self.has(k))
    }

    /// Return true when the key is absent. Mirrors Laravel's
    /// `Store::missing` (`Store.php:295-298`).
    pub fn missing(&self, key: &str) -> bool {
        !self.has(key)
    }

    /// Borrow the full session data map. Mirrors Laravel's
    /// `Store::all()` (`Store.php:247-250`). Returned by reference
    /// rather than by clone so callers don't pay the deep-copy cost on
    /// the hot path; clone explicitly if owned data is needed.
    pub fn all(&self) -> &HashMap<String, serde_json::Value> {
        &self.data
    }

    /// Clone the subset of session data matching `keys`. Mirrors
    /// Laravel's `Store::only` (`Store.php:258-261`).
    pub fn only(&self, keys: &[&str]) -> HashMap<String, serde_json::Value> {
        let mut out = HashMap::new();
        for k in keys {
            if let Some(v) = self.data.get(*k) {
                out.insert((*k).to_string(), v.clone());
            }
        }
        out
    }

    /// Clone every session entry *except* those in `keys`. Mirrors
    /// Laravel's `Store::except` (`Store.php:269-272`).
    pub fn except(&self, keys: &[&str]) -> HashMap<String, serde_json::Value> {
        let bad: std::collections::HashSet<&str> = keys.iter().copied().collect();
        self.data
            .iter()
            .filter(|(k, _)| !bad.contains(k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Flush every key and re-populate from `kvs`. Mirrors Laravel's
    /// `Store::replace($attributes)` (`Store.php:381-384`) — note that
    /// Laravel's `replace` is itself a `put($array)` so the existing
    /// keys survive; we go further and `flush` first because Suprnova's
    /// `put` only ever takes one key at a time and re-implementing
    /// Laravel's "merge if array" overload is the wrong shape for
    /// statically-typed Rust. Callers wanting the merge shape should
    /// call [`Self::put_many`] directly.
    pub fn replace<T>(&mut self, kvs: &[(&str, T)])
    where
        T: Serialize + Clone,
    {
        self.flush();
        for (k, v) in kvs {
            self.put(k, v.clone());
        }
    }

    /// Bulk put. The merge-shaped half of Laravel's `Store::put($array)`
    /// (`Store.php:393-402`) — pass-through over the existing single-key
    /// `put`. Convenience for migrating `session(['k1' => v1, 'k2' => v2])`
    /// calls.
    pub fn put_many<T>(&mut self, kvs: &[(&str, T)])
    where
        T: Serialize + Clone,
    {
        for (k, v) in kvs {
            self.put(k, v.clone());
        }
    }

    /// Forget many keys in one call. Mirrors Laravel's
    /// `Store::forget(array)` (`Store.php:585-588`).
    pub fn forget_many(&mut self, keys: &[&str]) {
        for k in keys {
            self.forget(k);
        }
    }

    /// Flash for the current request only. The value lands directly
    /// in `_flash.old.{key}` so the request that called `now` CAN read
    /// the value via `get_flash`, but the very next request's
    /// `age_flash_data` clears it. Mirrors Laravel's `Store::now`
    /// (`Store.php:489-496`).
    pub fn now<T: Serialize>(&mut self, key: &str, value: T) {
        if let Ok(v) = serde_json::to_value(value) {
            self.data.insert(format!("_flash.old.{}", key), v);
            self.dirty = true;
        }
    }

    /// Re-flash every value currently in `_flash.old.*` for one more
    /// request. Mirrors Laravel's `Store::reflash` (`Store.php:503-508`).
    pub fn reflash(&mut self) {
        let olds: Vec<String> = self
            .data
            .keys()
            .filter(|k| k.starts_with("_flash.old."))
            .cloned()
            .collect();
        let had = !olds.is_empty();
        for old in olds {
            if let Some(v) = self.data.remove(&old) {
                let new = old.replace("_flash.old.", "_flash.new.");
                self.data.insert(new, v);
            }
        }
        if had {
            self.dirty = true;
        }
    }

    /// Re-flash a specific subset of the current flash data. Mirrors
    /// Laravel's `Store::keep($keys)` (`Store.php:516-521`).
    pub fn keep(&mut self, keys: &[&str]) {
        let mut moved = false;
        for key in keys {
            let old = format!("_flash.old.{}", key);
            let new = format!("_flash.new.{}", key);
            if let Some(v) = self.data.remove(&old) {
                self.data.insert(new, v);
                moved = true;
            }
        }
        if moved {
            self.dirty = true;
        }
    }

    /// Flash form input for the next request as a single bag under
    /// `_flash.new._old_input`. Pairs with [`Self::has_old_input`] /
    /// [`Self::get_old_input`]. Mirrors Laravel's `Store::flashInput`
    /// (`Store.php:553-556`).
    pub fn flash_input(&mut self, input: HashMap<String, serde_json::Value>) {
        self.flash(OLD_INPUT_KEY, input);
    }

    /// Borrow the old-input bag. Returns an empty map when no input
    /// was flashed by the previous request. Mirrors Laravel's
    /// `Store::getOldInput` when called with no argument
    /// (`Store.php:370-373`).
    pub fn old_input(&self) -> HashMap<String, serde_json::Value> {
        let flash_key = format!("_flash.old.{}", OLD_INPUT_KEY);
        self.data
            .get(&flash_key)
            .and_then(|v| match v {
                serde_json::Value::Object(map) => Some(
                    map.iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<HashMap<String, serde_json::Value>>(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Get a single old-input value. Mirrors Laravel's
    /// `Store::getOldInput($key, $default)` (`Store.php:370-373`).
    pub fn get_old_input<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let bag = self.old_input();
        bag.get(key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Returns `true` when old-input data exists for the given key,
    /// or for any key when `None` is passed. Mirrors Laravel's
    /// `Store::hasOldInput` (`Store.php:356-361`).
    pub fn has_old_input(&self, key: Option<&str>) -> bool {
        let bag = self.old_input();
        match key {
            None => !bag.is_empty(),
            Some(k) => bag.contains_key(k),
        }
    }

    /// Drain the per-bag validation-errors flash bags written by
    /// [`crate::http::Redirect::with_errors`] /
    /// [`crate::http::Redirect::with_errors_bag`]. Returns a JSON
    /// `Map<bag_name, errors_object>` shaped like Laravel's
    /// `ViewErrorBag` (the per-bag value is `{ field: [messages] }`).
    ///
    /// Bag keys are recovered by walking the session for
    /// `_flash.old.errors.<bag>` entries (the standard flash-age path
    /// writes flashes to `.new.*` and ages them to `.old.*` on the
    /// next request — so by the time an Inertia response handles the
    /// redirect destination, the flash is in `.old.*`).
    ///
    /// Called by `InertiaResponse::resolve` to seed the `errors` prop
    /// so a `redirect()->withErrors(...)` flow naturally surfaces
    /// validation messages on the destination page.
    pub fn pull_errors_flash(&mut self) -> serde_json::Map<String, serde_json::Value> {
        let prefix = "_flash.old.errors.";
        // Collect matching keys first to avoid borrowing `self.data`
        // mutably while iterating it.
        let keys: Vec<String> = self
            .data
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        let mut out = serde_json::Map::new();
        for full_key in keys {
            let bag = match full_key.strip_prefix(prefix) {
                Some(b) => b.to_string(),
                None => continue,
            };
            if let Some(value) = self.data.remove(&full_key) {
                self.dirty = true;
                out.insert(bag, value);
            }
        }
        out
    }

    /// Read the previous URL the user visited. Mirrors Laravel's
    /// `Store::previousUrl()` (`Store.php:791-794`). The previous URL
    /// is written by [`crate::session::SessionMiddleware`] on every
    /// successful GET request that isn't an Inertia partial or a
    /// JSON-API call. Powers `redirect()->back()` in the routing layer.
    pub fn previous_url(&self) -> Option<String> {
        self.get("_previous.url")
    }

    /// Write the previous URL. Mirrors Laravel's
    /// `Store::setPreviousUrl` (`Store.php:802-805`).
    pub fn set_previous_url(&mut self, url: impl Into<String>) {
        self.put("_previous.url", url.into());
    }

    /// Read the previous route name. Mirrors Laravel's
    /// `Store::previousRoute()` (`Store.php:812-815`).
    pub fn previous_route(&self) -> Option<String> {
        self.get("_previous.route")
    }

    /// Write the previous route name. Mirrors Laravel's
    /// `Store::setPreviousRoute` (`Store.php:823-826`).
    pub fn set_previous_route(&mut self, route: impl Into<String>) {
        self.put("_previous.route", route.into());
    }

    /// Returns `true` when a previous URL is recorded — short-circuit
    /// for `previous_url().is_some()`. Mirrors Laravel's
    /// `Store::hasPreviousUri` (`Store.php:765-768`).
    pub fn has_previous_uri(&self) -> bool {
        self.previous_url().is_some()
    }

    /// Stamp the session as "the user just confirmed their password
    /// at the current time." Mirrors Laravel's `Store::passwordConfirmed`
    /// (`Store.php:833-836`). The timestamp is read by a
    /// `RequirePassword`-style middleware to decide whether to force
    /// re-confirmation on sensitive routes.
    pub fn password_confirmed(&mut self) {
        let now = chrono::Utc::now().timestamp();
        self.put("auth.password_confirmed_at", now);
    }

    /// Read the timestamp of the most recent password confirmation,
    /// if any. Sibling of [`Self::password_confirmed`].
    pub fn password_confirmed_at(&self) -> Option<i64> {
        self.get("auth.password_confirmed_at")
    }
}

/// Returns true when `id` matches the shape minted by
/// [`super::generate_session_id`] — 40 lowercase-alphanumeric
/// characters. Mirrors Laravel's `Store::isValidId`
/// (`Illuminate/Session/Store.php:712-715`).
pub fn is_valid_session_id(id: &str) -> bool {
    id.len() == 40
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// Session store trait for different backends
///
/// Implement this trait to create custom session storage backends.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Read a session by its ID
    ///
    /// Returns None if the session doesn't exist or has expired.
    async fn read(&self, id: &str) -> Result<Option<SessionData>, FrameworkError>;

    /// Write a session to storage
    ///
    /// Creates a new session if it doesn't exist, updates if it does.
    async fn write(&self, session: &SessionData) -> Result<(), FrameworkError>;

    /// Destroy a session by its ID
    async fn destroy(&self, id: &str) -> Result<(), FrameworkError>;

    /// Destroy every session belonging to a given `user_id`.
    ///
    /// Called after security-state transitions (password reset, 2FA
    /// change, account compromise recovery) to ensure stolen sessions
    /// cannot outlive the credential change. Returns the number of
    /// rows deleted.
    async fn destroy_for_user(&self, user_id: &str) -> Result<u64, FrameworkError>;

    /// Garbage collect expired sessions
    ///
    /// Returns the number of sessions cleaned up.
    async fn gc(&self) -> Result<u64, FrameworkError>;
}

#[cfg(test)]
mod dirty_tracking_tests {
    use super::*;

    #[test]
    fn forget_absent_key_does_not_dirty_session() {
        let mut s = SessionData::new("sid".into(), "tok".into());
        assert!(!s.is_dirty(), "a fresh session starts clean");
        // Forgetting a key that was never set must leave the session clean so
        // a read-only request isn't forced through the write (fail-closed) path.
        let removed = s.forget("never_set");
        assert!(removed.is_none());
        assert!(
            !s.is_dirty(),
            "forgetting an absent key must not dirty the session"
        );
    }

    #[test]
    fn forget_present_key_dirties_and_returns_value() {
        let mut s = SessionData::new("sid".into(), "tok".into());
        s.put("k", 42);
        let removed = s.forget("k");
        assert_eq!(removed, Some(serde_json::json!(42)));
        assert!(s.is_dirty());
    }
}
