//! Encrypted + hashed casts.
//!
//! Five casts that mediate cryptographic transforms on the storage ↔
//! runtime boundary:
//!
//! - [`AsEncrypted`] — `String` ↔ AES-256-GCM ciphertext, both at rest
//!   and on the wire to/from the column.
//! - [`AsEncryptedArray<T>`] — `Vec<T>` via JSON-then-encrypt.
//! - [`AsEncryptedObject<T>`] — any `Serialize + DeserializeOwned` type
//!   via JSON-then-encrypt. Use when the runtime shape is a fixed
//!   struct.
//! - [`AsEncryptedCollection<T>`] — `Collection<T>` via JSON-then-encrypt.
//! - [`AsHashed`] — one-way bcrypt hash on write; the stored value
//!   matches what `from_storage` returns (matches Laravel's `hashed`
//!   cast). Idempotent across re-saves: an already-hashed value passes
//!   through unchanged, so `User::find().save()` does not re-bcrypt
//!   the hash into a hash-of-hash.
//!
//! All four `AsEncrypted*` casts share the [`Crypt`] facade from
//! [`crate::crypto`]. The facade must be initialised (via
//! `Server::from_config` in production or
//! [`crate::testing::install_test_encryption_key`] in tests) before any
//! of these casts run; an uninitialised facade surfaces as a clear
//! `FrameworkError::Internal` from the wrapped `Crypt::*` calls.

use std::marker::PhantomData;

use serde::{Serialize, de::DeserializeOwned};

use super::{Cast, DynCast, IntoDynCast};
use crate::Crypt;
use crate::crypto::CryptPurpose;
use crate::error::FrameworkError;

// ---- AsEncrypted ---------------------------------------------------------

/// Cast `String` ↔ AES-256-GCM-encrypted `String`. The on-disk column
/// holds URL-safe base64 of `nonce || ciphertext_with_tag`; each write
/// uses a fresh random nonce so two writes of the same plaintext produce
/// distinct ciphertexts. The runtime value is the decrypted UTF-8 string.
pub struct AsEncrypted;

impl Cast for AsEncrypted {
    type Runtime = String;
    type Storage = String;

    fn to_storage(v: &String) -> Result<String, FrameworkError> {
        Crypt::encrypt_string(CryptPurpose::Cast, v)
            .map_err(|e| FrameworkError::internal(format!("AsEncrypted: {e}")))
    }

    fn from_storage(s: &String) -> Result<String, FrameworkError> {
        Crypt::decrypt_string(CryptPurpose::Cast, s)
            .map_err(|e| FrameworkError::internal(format!("AsEncrypted: {e}")))
    }
}

struct AsEncryptedDyn;

impl DynCast for AsEncryptedDyn {
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        // Domain 7 audit D7-A — strict-validate input shape; was silently
        // coercing non-strings to "" and attempting to decrypt that.
        let wire = v
            .as_str()
            .ok_or_else(|| {
                FrameworkError::validation(
                    "AsEncrypted",
                    format!("dyn from_storage: expected JSON string, got {v:?}"),
                )
            })?
            .to_string();
        Ok(serde_json::Value::String(AsEncrypted::from_storage(&wire)?))
    }

    fn to_storage_json(&self, v: &serde_json::Value) -> Result<serde_json::Value, FrameworkError> {
        // Domain 7 audit D7-A — strict-validate. Critical for the
        // write path because a non-string input here used to be
        // silently encrypted as the empty string.
        let s = v
            .as_str()
            .ok_or_else(|| {
                FrameworkError::validation(
                    "AsEncrypted",
                    format!("dyn to_storage: expected JSON string, got {v:?}"),
                )
            })?
            .to_string();
        Ok(serde_json::Value::String(AsEncrypted::to_storage(&s)?))
    }
}

impl IntoDynCast for AsEncrypted {
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsEncryptedDyn)
    }
}

// ---- AsEncryptedArray<T> -------------------------------------------------

/// Cast `Vec<T>` ↔ AES-256-GCM-encrypted JSON array. The element type
/// `T` must be `Serialize + DeserializeOwned`. The pipeline is:
/// serialise to JSON → encrypt → base64 → store; reverse on read.
pub struct AsEncryptedArray<T>(PhantomData<T>);

impl<T> Cast for AsEncryptedArray<T>
where
    T: Serialize + DeserializeOwned + Send + Sync,
{
    type Runtime = Vec<T>;
    type Storage = String;

    fn to_storage(v: &Vec<T>) -> Result<String, FrameworkError> {
        let json = serde_json::to_string(v).map_err(|e| {
            FrameworkError::validation("AsEncryptedArray", format!("serialize: {e}"))
        })?;
        Crypt::encrypt_string(CryptPurpose::Cast, &json)
            .map_err(|e| FrameworkError::internal(format!("AsEncryptedArray encrypt: {e}")))
    }

    fn from_storage(s: &String) -> Result<Vec<T>, FrameworkError> {
        let plain = Crypt::decrypt_string(CryptPurpose::Cast, s)
            .map_err(|e| FrameworkError::internal(format!("AsEncryptedArray decrypt: {e}")))?;
        serde_json::from_str(&plain).map_err(|e| {
            FrameworkError::validation("AsEncryptedArray", format!("deserialize: {e}"))
        })
    }
}

struct AsEncryptedArrayDyn<T>(PhantomData<T>);

impl<T> DynCast for AsEncryptedArrayDyn<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        // Domain 7 audit D7-A — strict-validate input shape.
        let wire = v
            .as_str()
            .ok_or_else(|| {
                FrameworkError::validation(
                    "AsEncryptedArray",
                    format!("dyn from_storage: expected JSON string, got {v:?}"),
                )
            })?
            .to_string();
        let decrypted: Vec<T> = AsEncryptedArray::<T>::from_storage(&wire)?;
        Ok(serde_json::to_value(decrypted).expect("Vec<T> serialises"))
    }

    fn to_storage_json(&self, v: &serde_json::Value) -> Result<serde_json::Value, FrameworkError> {
        let parsed: Vec<T> = serde_json::from_value(v.clone())
            .map_err(|e| FrameworkError::validation("AsEncryptedArray", format!("dyn: {e}")))?;
        Ok(serde_json::Value::String(
            AsEncryptedArray::<T>::to_storage(&parsed)?,
        ))
    }
}

impl<T> IntoDynCast for AsEncryptedArray<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsEncryptedArrayDyn::<T>(PhantomData))
    }
}

// ---- AsEncryptedObject<T> ------------------------------------------------

/// Cast any `Serialize + DeserializeOwned` value ↔ AES-256-GCM-encrypted
/// JSON. Use when the runtime shape is a fixed struct (e.g. a
/// `Secret { ssn, dob }` record). For a `Vec<T>` runtime shape use
/// [`AsEncryptedArray`], for a `Collection<T>` use [`AsEncryptedCollection`].
pub struct AsEncryptedObject<T>(PhantomData<T>);

impl<T> Cast for AsEncryptedObject<T>
where
    T: Serialize + DeserializeOwned + Send + Sync,
{
    type Runtime = T;
    type Storage = String;

    fn to_storage(v: &T) -> Result<String, FrameworkError> {
        let json = serde_json::to_string(v).map_err(|e| {
            FrameworkError::validation("AsEncryptedObject", format!("serialize: {e}"))
        })?;
        Crypt::encrypt_string(CryptPurpose::Cast, &json)
            .map_err(|e| FrameworkError::internal(format!("AsEncryptedObject encrypt: {e}")))
    }

    fn from_storage(s: &String) -> Result<T, FrameworkError> {
        let plain = Crypt::decrypt_string(CryptPurpose::Cast, s)
            .map_err(|e| FrameworkError::internal(format!("AsEncryptedObject decrypt: {e}")))?;
        serde_json::from_str(&plain).map_err(|e| {
            FrameworkError::validation("AsEncryptedObject", format!("deserialize: {e}"))
        })
    }
}

struct AsEncryptedObjectDyn<T>(PhantomData<T>);

impl<T> DynCast for AsEncryptedObjectDyn<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        // Domain 7 audit D7-A — strict-validate input shape.
        let wire = v
            .as_str()
            .ok_or_else(|| {
                FrameworkError::validation(
                    "AsEncryptedObject",
                    format!("dyn from_storage: expected JSON string, got {v:?}"),
                )
            })?
            .to_string();
        let decrypted: T = AsEncryptedObject::<T>::from_storage(&wire)?;
        Ok(serde_json::to_value(decrypted).expect("T serialises"))
    }

    fn to_storage_json(&self, v: &serde_json::Value) -> Result<serde_json::Value, FrameworkError> {
        let parsed: T = serde_json::from_value(v.clone())
            .map_err(|e| FrameworkError::validation("AsEncryptedObject", format!("dyn: {e}")))?;
        Ok(serde_json::Value::String(
            AsEncryptedObject::<T>::to_storage(&parsed)?,
        ))
    }
}

impl<T> IntoDynCast for AsEncryptedObject<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsEncryptedObjectDyn::<T>(PhantomData))
    }
}

// ---- AsEncryptedCollection<T> -------------------------------------------

/// Cast `Collection<T>` ↔ AES-256-GCM-encrypted JSON array. Thin
/// wrapper around [`AsEncryptedArray`] that round-trips through the
/// framework's [`Collection`] type so users get the Eloquent-style
/// slice surface (`.len()`, indexing, iteration via `Deref`).
///
/// [`Collection`]: crate::eloquent::Collection
pub struct AsEncryptedCollection<T>(PhantomData<T>);

impl<T> Cast for AsEncryptedCollection<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + Clone,
{
    type Runtime = crate::eloquent::Collection<T>;
    type Storage = String;

    fn to_storage(v: &crate::eloquent::Collection<T>) -> Result<String, FrameworkError> {
        // Borrow the inner slice and clone into a Vec — the slice itself
        // serialises with the same shape as Vec<T> (a JSON array), but
        // taking a Vec keeps the `to_storage` signature on
        // AsEncryptedArray<T> uniform with the rest of the cast surface.
        AsEncryptedArray::<T>::to_storage(&v.as_slice().to_vec())
    }

    fn from_storage(s: &String) -> Result<crate::eloquent::Collection<T>, FrameworkError> {
        AsEncryptedArray::<T>::from_storage(s).map(crate::eloquent::Collection::from)
    }
}

struct AsEncryptedCollectionDyn<T>(PhantomData<T>);

impl<T> DynCast for AsEncryptedCollectionDyn<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + Clone + 'static,
{
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        // Domain 7 audit D7-A — strict-validate input shape.
        let wire = v
            .as_str()
            .ok_or_else(|| {
                FrameworkError::validation(
                    "AsEncryptedCollection",
                    format!("dyn from_storage: expected JSON string, got {v:?}"),
                )
            })?
            .to_string();
        let decrypted = AsEncryptedCollection::<T>::from_storage(&wire)?;
        Ok(serde_json::to_value(decrypted.into_vec()).expect("Vec<T> serialises"))
    }

    fn to_storage_json(&self, v: &serde_json::Value) -> Result<serde_json::Value, FrameworkError> {
        let parsed: Vec<T> = serde_json::from_value(v.clone()).map_err(|e| {
            FrameworkError::validation("AsEncryptedCollection", format!("dyn: {e}"))
        })?;
        let coll = crate::eloquent::Collection::<T>::from(parsed);
        Ok(serde_json::Value::String(
            AsEncryptedCollection::<T>::to_storage(&coll)?,
        ))
    }
}

impl<T> IntoDynCast for AsEncryptedCollection<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + Clone + 'static,
{
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsEncryptedCollectionDyn::<T>(PhantomData))
    }
}

// ---- AsHashed ------------------------------------------------------------

/// Cast a plaintext `String` to a hashed string on write using the
/// active driver (`HASH_DRIVER` — bcrypt by default; argon2i / argon2id
/// also supported). The runtime value is the hashed string — there is no
/// reverse direction.
///
/// ## Idempotence
///
/// `to_storage` is idempotent: a value that already looks like ANY
/// recognised hash (bcrypt `$2*$`, argon2i / argon2id PHC) passes through
/// unchanged. Without this guard, a roundtrip like
/// `User::find(id).await?.save().await?` would re-hash the existing hash
/// into a hash-of-hash, breaking `verify(plain, stored)` and invalidating
/// every existing password.
///
/// Mirrors Laravel's `hashed` cast: the idempotence check uses
/// `Hash::isHashed($value)`, which returns true for any recognised
/// algorithm.
pub struct AsHashed;

impl Cast for AsHashed {
    type Runtime = String;
    type Storage = String;

    fn to_storage(v: &String) -> Result<String, FrameworkError> {
        // Idempotent on re-save: skip rehashing an already-hashed value
        // regardless of algorithm.
        if crate::hashing::is_hashed(v) {
            return Ok(v.clone());
        }
        // `hashing::hash` already returns Result<String, FrameworkError> —
        // propagate directly, don't double-wrap.
        crate::hashing::hash(v)
    }

    fn from_storage(s: &String) -> Result<String, FrameworkError> {
        // Hashes don't reverse — Laravel's `hashed` cast does the same.
        Ok(s.clone())
    }
}

struct AsHashedDyn;

impl DynCast for AsHashedDyn {
    fn from_storage_json(
        &self,
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError> {
        // Hash never reverses; pass through whatever the DB returned.
        Ok(v.clone())
    }

    fn to_storage_json(&self, v: &serde_json::Value) -> Result<serde_json::Value, FrameworkError> {
        // Domain 7 audit D7-A — was `v.as_str().unwrap_or("")` which
        // would silently bcrypt the empty string if user code routed a
        // non-string value through here (e.g. via a future `with_casts`
        // write-path wiring). The damage shape: every row's password
        // column overwritten with bcrypt("") and no error returned.
        // Strict-validation eliminates that footgun by construction.
        let s = v
            .as_str()
            .ok_or_else(|| {
                FrameworkError::validation(
                    "AsHashed",
                    format!("dyn to_storage: expected JSON string, got {v:?}"),
                )
            })?
            .to_string();
        Ok(serde_json::Value::String(AsHashed::to_storage(&s)?))
    }
}

impl IntoDynCast for AsHashed {
    fn into_dyn() -> Box<dyn DynCast> {
        Box::new(AsHashedDyn)
    }
}

#[cfg(test)]
mod tests {
    //! Unit-level coverage for cast logic that doesn't need a database.
    //! Integration tests live in
    //! `framework/tests/eloquent_casts_encrypted.rs`.

    use super::*;

    #[test]
    fn as_hashed_treats_canonical_bcrypt_as_already_hashed() {
        // Build a canonical 60-char bcrypt-shaped string and confirm the
        // algorithm-agnostic `is_hashed` recognises it.
        let canonical = format!("$2b$12${}", "a".repeat(53));
        assert_eq!(canonical.len(), 60);
        assert!(crate::hashing::is_hashed(&canonical));
    }

    #[test]
    fn as_hashed_rejects_plaintext_that_only_starts_with_dollar_2b() {
        // Plaintext prefix that looks bcrypt-y but isn't the right
        // length — must NOT be treated as already-hashed.
        let fake = "$2b$short-not-a-real-hash".to_string();
        assert!(!crate::hashing::is_hashed(&fake));
    }

    #[test]
    fn as_hashed_rejects_wrong_prefix_even_at_60_chars() {
        // 60 chars but wrong prefix — not a recognised hash, must rehash.
        let mut s = "$2c$".to_string();
        s.push_str(&"x".repeat(56));
        assert_eq!(s.len(), 60);
        assert!(!crate::hashing::is_hashed(&s));
    }

    #[test]
    fn as_hashed_accepts_2a_and_2y_variants() {
        // Older bcrypt variants — must also pass through unchanged.
        let a = format!("$2a$12${}", "x".repeat(53));
        let y = format!("$2y$12${}", "x".repeat(53));
        assert_eq!(a.len(), 60);
        assert_eq!(y.len(), 60);
        assert!(crate::hashing::is_hashed(&a));
        assert!(crate::hashing::is_hashed(&y));
    }

    #[test]
    fn as_hashed_treats_argon_hash_as_already_hashed() {
        // Algorithm-agnostic — once we support argon2id via HASH_DRIVER,
        // the cast must not re-hash a stored argon2id digest into a
        // bcrypt-of-argon-of-argon-of-… chain on every save.
        use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
        let salt = SaltString::generate(&mut OsRng);
        let h = argon2::Argon2::default()
            .hash_password(b"password", &salt)
            .unwrap()
            .to_string();
        assert!(h.starts_with("$argon2id$"));
        assert!(crate::hashing::is_hashed(&h));
        // `to_storage` must pass it through unchanged.
        let pass_through = AsHashed::to_storage(&h).expect("idempotent");
        assert_eq!(pass_through, h);
    }
}
