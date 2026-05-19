//! Phase 10A T7c — Encrypted + hashed casts.
//!
//! Same model-hoisting convention as T7a / T7b: models live at module
//! scope so the `#[model]` macro's inner module (which only sees the
//! test file's top-level `use` items) resolves the cast type names
//! correctly.
//!
//! Tests:
//! 1. `AsEncrypted` plaintext → ciphertext-on-disk → plaintext round-trip
//!    (with raw `SELECT` proving the column does NOT contain plaintext)
//! 2. `AsEncryptedArray<T>` for `Vec<String>` (plus raw SELECT leak check)
//! 3. `AsEncryptedObject<T>` for a struct (plus raw SELECT leak check)
//! 4. `AsEncryptedCollection<T>` for `Collection<String>` (plus raw SELECT)
//! 5. `AsHashed` creates a bcrypt hash on write; read returns the hash
//! 6. Corrupt ciphertext yields a clear decrypt error (proves the decrypt
//!    path actually runs)
//! 7. `AsHashed` is idempotent across re-saves — loading then saving a
//!    model does NOT re-hash the already-hashed value. Without this
//!    guard, `User::find().save()` would bcrypt the existing hash and
//!    break login (Laravel's `hashed` cast skips rehashing for the same
//!    reason — see `Hash::info()->algoName`).

use serde::{Deserialize, Serialize};
use suprnova::testing::TestDatabase;
use suprnova::{
    attrs, model, AsEncrypted, AsEncryptedArray, AsEncryptedCollection, AsEncryptedObject,
    AsHashed, Collection, Model,
};

// ---- Test fixtures hoisted to module scope ------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Secret {
    pub ssn: String,
    pub dob: String,
}

// ---- Models -------------------------------------------------------------

#[model(
    table = "t7c_enc",
    timestamps = false,
    fillable = ["secret"],
    casts = { secret = AsEncrypted }
)]
pub struct EncModel {
    pub id: i64,
    pub secret: String,
}

#[model(
    table = "t7c_enc_arr",
    timestamps = false,
    fillable = ["tokens"],
    casts = { tokens = AsEncryptedArray<String> }
)]
pub struct EncArrModel {
    pub id: i64,
    pub tokens: Vec<String>,
}

#[model(
    table = "t7c_enc_obj",
    timestamps = false,
    fillable = ["data"],
    casts = { data = AsEncryptedObject<Secret> }
)]
pub struct EncObjModel {
    pub id: i64,
    pub data: Secret,
}

#[model(
    table = "t7c_enc_col",
    timestamps = false,
    fillable = ["items"],
    casts = { items = AsEncryptedCollection<String> }
)]
pub struct EncColModel {
    pub id: i64,
    pub items: Collection<String>,
}

#[model(
    table = "t7c_hash",
    timestamps = false,
    fillable = ["password"],
    casts = { password = AsHashed }
)]
pub struct HashModel {
    pub id: i64,
    pub password: String,
}

#[model(
    table = "t7c_corrupt",
    timestamps = false,
    fillable = ["secret"],
    casts = { secret = AsEncrypted }
)]
pub struct CorruptModel {
    pub id: i64,
    pub secret: String,
}

#[model(
    table = "t7c_hash_idem",
    timestamps = false,
    fillable = ["password"],
    casts = { password = AsHashed }
)]
pub struct HashIdemModel {
    pub id: i64,
    pub password: String,
}

// ---- Helpers ------------------------------------------------------------

/// Install the deterministic test encryption key + return a fresh in-memory
/// SQLite. Idempotent — the underlying `Crypt` facade is `OnceLock`-backed
/// so the second call is a no-op (the helper itself ignores the return).
async fn setup_db_with_key() -> TestDatabase {
    suprnova::testing::install_test_encryption_key();
    TestDatabase::sqlite_memory().await.unwrap()
}

// ---- Tests --------------------------------------------------------------

#[tokio::test]
async fn as_encrypted_round_trips_and_storage_is_ciphertext() {
    let db = setup_db_with_key().await;
    db.execute_unprepared(
        "CREATE TABLE t7c_enc (id INTEGER PRIMARY KEY AUTOINCREMENT, secret TEXT NOT NULL)",
    )
    .await
    .unwrap();

    let made = EncModel::create(attrs! { secret: "social-security-number" })
        .await
        .unwrap();

    // Raw column read should NOT match the plaintext (it's ciphertext).
    let raw = db
        .fetch_one(
            "SELECT secret FROM t7c_enc WHERE id = ?",
            vec![sea_orm::Value::from(made.id)],
        )
        .await
        .unwrap();
    let stored: String = raw.try_get("", "secret").unwrap();
    assert_ne!(
        stored, "social-security-number",
        "DB column should hold ciphertext, not plaintext"
    );
    assert!(stored.len() > 10, "ciphertext should be non-empty");

    // Model read decrypts.
    let read = EncModel::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.secret, "social-security-number");
}

#[tokio::test]
async fn as_encrypted_array_round_trips() {
    let db = setup_db_with_key().await;
    db.execute_unprepared(
        "CREATE TABLE t7c_enc_arr (id INTEGER PRIMARY KEY AUTOINCREMENT, tokens TEXT NOT NULL)",
    )
    .await
    .unwrap();

    let made = EncArrModel::create(attrs! { tokens: ["t1", "t2", "t3"] })
        .await
        .unwrap();
    let read = EncArrModel::find(made.id).await.unwrap().unwrap();
    assert_eq!(
        read.tokens,
        vec!["t1".to_string(), "t2".to_string(), "t3".to_string()]
    );

    // Ciphertext should not leak plaintext substrings.
    let raw = db
        .fetch_one(
            "SELECT tokens FROM t7c_enc_arr WHERE id = ?",
            vec![sea_orm::Value::from(made.id)],
        )
        .await
        .unwrap();
    let stored: String = raw.try_get("", "tokens").unwrap();
    assert!(
        !stored.contains("t1"),
        "ciphertext should not leak plaintext substring 't1'"
    );
}

#[tokio::test]
async fn as_encrypted_object_round_trips() {
    let db = setup_db_with_key().await;
    db.execute_unprepared(
        "CREATE TABLE t7c_enc_obj (id INTEGER PRIMARY KEY AUTOINCREMENT, data TEXT NOT NULL)",
    )
    .await
    .unwrap();

    let made = EncObjModel::create(attrs! {
        data: serde_json::json!({ "ssn": "123-45-6789", "dob": "1990-01-01" })
    })
    .await
    .unwrap();
    let read = EncObjModel::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.data.ssn, "123-45-6789");
    assert_eq!(read.data.dob, "1990-01-01");

    let raw = db
        .fetch_one(
            "SELECT data FROM t7c_enc_obj WHERE id = ?",
            vec![sea_orm::Value::from(made.id)],
        )
        .await
        .unwrap();
    let stored: String = raw.try_get("", "data").unwrap();
    assert!(
        !stored.contains("123-45"),
        "ciphertext should not leak ssn substring"
    );
}

#[tokio::test]
async fn as_encrypted_collection_round_trips() {
    let db = setup_db_with_key().await;
    db.execute_unprepared(
        "CREATE TABLE t7c_enc_col (id INTEGER PRIMARY KEY AUTOINCREMENT, items TEXT NOT NULL)",
    )
    .await
    .unwrap();

    let made = EncColModel::create(attrs! { items: ["alpha", "beta"] })
        .await
        .unwrap();
    let read = EncColModel::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.items.len(), 2);
    assert_eq!(&read.items[0], "alpha");
    assert_eq!(&read.items[1], "beta");

    let raw = db
        .fetch_one(
            "SELECT items FROM t7c_enc_col WHERE id = ?",
            vec![sea_orm::Value::from(made.id)],
        )
        .await
        .unwrap();
    let stored: String = raw.try_get("", "items").unwrap();
    assert!(
        !stored.contains("alpha"),
        "ciphertext should not leak collection element"
    );
}

#[tokio::test]
async fn as_hashed_writes_bcrypt_and_does_not_decrypt() {
    let db = setup_db_with_key().await;
    db.execute_unprepared(
        "CREATE TABLE t7c_hash (id INTEGER PRIMARY KEY AUTOINCREMENT, password TEXT NOT NULL)",
    )
    .await
    .unwrap();

    let made = HashModel::create(attrs! { password: "plain-secret" })
        .await
        .unwrap();

    // Both the in-memory value and the stored value are the bcrypt hash —
    // AsHashed is one-way (Laravel matches this).
    assert!(
        made.password.starts_with("$2b$") || made.password.starts_with("$2a$"),
        "expected bcrypt hash, got {}",
        made.password
    );

    // Verify the hash actually verifies the original plaintext — proves
    // the cast called the real `hashing::hash` and didn't, say, store
    // the literal "plain-secret" with a `$2b$` prefix.
    assert!(
        suprnova::hashing::verify("plain-secret", &made.password).unwrap(),
        "hashing::verify must accept the original plaintext against the stored hash"
    );

    let read = HashModel::find(made.id).await.unwrap().unwrap();
    assert_eq!(
        read.password, made.password,
        "find() should return the same hash that create() returned"
    );
}

#[tokio::test]
async fn corrupt_ciphertext_yields_clear_error() {
    // Proves the decrypt path actually runs and surfaces errors —
    // without depending on a "remove the key after install" API that
    // doesn't exist on Crypt (its global state is OnceLock-backed).
    // Insert a value that isn't valid AES-GCM ciphertext directly via
    // raw SQL, then try to read it through the AsEncrypted cast.
    let db = setup_db_with_key().await;
    db.execute_unprepared(
        "CREATE TABLE t7c_corrupt (id INTEGER PRIMARY KEY AUTOINCREMENT, secret TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "INSERT INTO t7c_corrupt (id, secret) VALUES (1, 'not-real-ciphertext')",
    )
    .await
    .unwrap();

    // `find` calls `From<inner::Model>` which `.expect()`s the cast —
    // a panic propagates out of the awaited future as a join error.
    // The macro-generated From impl uses `.expect("cast from_storage failed
    // — corrupt data in database column")` on the from_storage call.
    // The panic surfaces here as an `Err` from `catch_unwind` semantics
    // — tokio's `spawn` reflects the panic into the JoinHandle. For
    // `Model::find`, which executes inline (no spawn), the panic
    // unwinds normally — we catch it with `std::panic::catch_unwind`
    // via `AssertUnwindSafe` and `FutureExt::catch_unwind`.
    use futures::future::FutureExt;
    let result = std::panic::AssertUnwindSafe(CorruptModel::find(1))
        .catch_unwind()
        .await;
    assert!(
        result.is_err(),
        "decrypt of corrupt ciphertext should panic via the macro's .expect()"
    );
    // Extract the panic message and assert it mentions the cast failure.
    let panic_payload = result.unwrap_err();
    let msg = panic_payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            panic_payload
                .downcast_ref::<&'static str>()
                .map(|s| (*s).to_string())
        })
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        msg.contains("cast") || msg.contains("corrupt") || msg.contains("from_storage"),
        "expected cast/corrupt mention in panic, got: {msg}"
    );
}

#[tokio::test]
async fn as_hashed_is_idempotent_across_re_saves() {
    // Regression guard against the Laravel `hashed` re-hash bug:
    // 1. Create row → password column holds bcrypt hash H of "plain-secret".
    // 2. Load it back via find() → in-memory model carries H.
    // 3. Save it again (unchanged) → `Cast::to_storage(&H)` MUST NOT
    //    re-hash H into hash-of-hash. If it did, `verify("plain-secret",
    //    new_stored)` would fail and the user could no longer log in.
    // 4. Re-load and assert verify still passes against original plaintext.
    let db = setup_db_with_key().await;
    db.execute_unprepared(
        "CREATE TABLE t7c_hash_idem (id INTEGER PRIMARY KEY AUTOINCREMENT, password TEXT NOT NULL)",
    )
    .await
    .unwrap();

    let made = HashIdemModel::create(attrs! { password: "plain-secret" })
        .await
        .unwrap();
    let hash_after_create = made.password.clone();
    assert!(suprnova::hashing::verify("plain-secret", &hash_after_create).unwrap());

    let mut reloaded = HashIdemModel::find(made.id).await.unwrap().unwrap();
    assert_eq!(reloaded.password, hash_after_create);

    // Save again — same hash in memory, the update path walks every
    // cast field and re-runs `to_storage`. With the idempotence guard
    // in AsHashed, the stored value stays equal.
    reloaded.save().await.unwrap();

    let re_read = HashIdemModel::find(made.id).await.unwrap().unwrap();
    assert_eq!(
        re_read.password, hash_after_create,
        "AsHashed::to_storage must pass through already-hashed bcrypt values, \
         not re-hash them into hash-of-hash"
    );
    assert!(
        suprnova::hashing::verify("plain-secret", &re_read.password).unwrap(),
        "the original plaintext must still verify after a no-op save"
    );
}
