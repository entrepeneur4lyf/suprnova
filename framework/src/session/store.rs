//! Session storage abstraction

use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;

use crate::error::FrameworkError;

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
    /// ```rust,ignore
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
    /// ```rust,ignore
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
        self.dirty = true;
        self.data.remove(key)
    }

    /// Check if the session has a key
    pub fn has(&self, key: &str) -> bool {
        self.data.contains_key(key)
    }

    /// Flash a value to the session (available only for next request)
    ///
    /// # Example
    ///
    /// ```rust,ignore
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
