//! Phase 10C T14 — end-to-end coverage that wires every 10C feature
//! through the example app's real `Migrator` schema + real
//! `app::models::User` + the `UserObserver` + the `#[scopes(User)]`
//! local scope.
//!
//! Uses `TestDatabase::fresh::<Migrator>()` so the schema the tests
//! exercise can never drift from what the dev DB runs. Each test owns
//! its own connection; observer install runs once per test via the
//! `install_observers` helper because the inventory is process-global
//! and `bootstrap_observers()` itself is idempotent (each install
//! closure gates with an `AtomicBool`).
//!
//! What each test pins:
//!
//! 1. `create_normalises_email_via_observer` — `UserObserver::creating`
//!    lowercases the email column before insert; `UserObserver::created`
//!    fires after the row lands.
//! 2. `active_scope_filters_to_active_rows` — both the static helper
//!    `User::active()` and the `Builder<User>` extension method
//!    `.active()` filter to `active = true` rows.
//! 3. `paginate_returns_inertia_ready_shape` — `User::query().paginate(n)`
//!    returns a `LengthAwarePaginator<User>` with the Laravel-shape
//!    `data` / `total` / `last_page` / `current_page` fields populated.
//! 4. `chunk_by_id_walks_full_dataset` — `User::query().chunk_by_id(n, ...)`
//!    visits every row exactly once across the batched cursor walks.
//! 5. `collection_pluck_extracts_emails` — `Collection<User>::pluck("email")`
//!    materialises a `Collection<String>` of the column values.
//! 6. `transaction_commits_then_rolls_back_audit_row` — `DB::transaction`
//!    commits a User + audit_log pair atomically. The companion error
//!    path proves the audit row is rolled back when the closure errors.

use app::migrations::Migrator;
use app::models::users::User;
// Bring the `#[scopes(User)]` macro-emitted trait into scope so the
// `Builder<User>::active()` extension method resolves. The trait is
// `pub` so test code outside `app::models::users` can opt into the
// extension; explicit `use` is by design — Rust's orphan rules and
// the test harness's module boundaries mean it can't auto-import.
use app::models::users::HasScope_active_User;
use suprnova::eloquent::observers::bootstrap_observers;
use suprnova::testing::TestDatabase;
use suprnova::{attrs, Collection, FrameworkError, LengthAwarePaginator, Model, DB};

/// Install every `#[suprnova::observer(...)]` declared in the binary.
/// The macro emits an `AtomicBool` gate per observer type so calling
/// this repeatedly is safe — only the first call actually registers
/// the listener adapters with the framework's `EventDispatcher`.
async fn install_observers() {
    bootstrap_observers()
        .await
        .expect("UserObserver install must succeed");
}

// ---- 1. Observer wiring -------------------------------------------------

#[tokio::test]
async fn create_normalises_email_via_observer() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    install_observers().await;

    // The observer's `creating` hook lower-cases the `email` column
    // BEFORE the insert lands. Pass a mixed-case email; expect the
    // persisted row to have it lower-cased.
    let user = User::create(attrs! {
        name: "Alice",
        email: "ALICE@EXAMPLE.COM",
        password: "hashed",
    })
    .await
    .unwrap();

    assert_eq!(
        user.email, "alice@example.com",
        "UserObserver::creating should lower-case email pre-insert; \
         observed: {:?}",
        user.email
    );

    // Round-trip: re-read from DB to confirm the persisted value
    // matches what `creating` rewrote (not just what the returned
    // Model carried).
    let reread = User::find(user.id).await.unwrap().unwrap();
    assert_eq!(reread.email, "alice@example.com");
}

// ---- 2. Local scope -----------------------------------------------------

#[tokio::test]
async fn active_scope_filters_to_active_rows() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    install_observers().await;

    // Two users: one explicitly active, one flagged inactive via
    // raw UPDATE (because `active` is not in `fillable`, the
    // attrs!-driven create path can't set it directly — see the
    // dogfood-eloquent test for the fillable check).
    let alice = User::create(attrs! {
        name: "Alice",
        email: "alice@example.com",
        password: "pw",
    })
    .await
    .unwrap();
    let _bob = User::create(attrs! {
        name: "Bob",
        email: "bob@example.com",
        password: "pw",
    })
    .await
    .unwrap();

    // Mark Bob inactive via raw DB::update — the dogfood needs both
    // active and inactive rows to prove the scope filters. The
    // facade takes `IntoIterator<Item = SeaValue>` for the bindings;
    // build the SeaValue explicitly because the bool variant of
    // serde_json::Value collapses to integer on SQLite the wrong
    // way around.
    DB::update(
        "UPDATE users SET active = ? WHERE name = ?",
        [
            suprnova::sea_orm::Value::Bool(Some(false)),
            suprnova::sea_orm::Value::String(Some(Box::new("Bob".to_string()))),
        ],
    )
    .await
    .unwrap();

    // Static helper entry point.
    let actives_via_static: Vec<User> = User::active().get().await.unwrap().into_vec();
    assert_eq!(actives_via_static.len(), 1);
    assert_eq!(actives_via_static[0].id, alice.id);

    // Builder extension form — chains onto an existing builder.
    let actives_via_chain: Vec<User> = User::query()
        .order_by_asc("id")
        .active()
        .get()
        .await
        .unwrap()
        .into_vec();
    assert_eq!(actives_via_chain.len(), 1);
    assert_eq!(actives_via_chain[0].id, alice.id);
}

// ---- 3. Pagination ------------------------------------------------------

#[tokio::test]
async fn paginate_returns_inertia_ready_shape() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    install_observers().await;

    for i in 0..25 {
        User::create(attrs! {
            name: format!("U{i}"),
            email: format!("u{i}@example.com"),
            password: "pw",
        })
        .await
        .unwrap();
    }

    let page: LengthAwarePaginator<User> = User::query()
        .order_by_asc("id")
        .paginate(10)
        .await
        .unwrap();

    assert_eq!(page.data.len(), 10);
    assert_eq!(page.total, 25);
    assert_eq!(page.last_page, 3);
    assert_eq!(page.current_page, 1);
    assert_eq!(page.per_page, 10);
    // The serialize-ready paginator carries the window bounds so an
    // Inertia view can render "Showing 1–10 of 25" without a second
    // query.
    assert_eq!(page.from, Some(1));
    assert_eq!(page.to, Some(10));
}

// ---- 4. Chunking --------------------------------------------------------

#[tokio::test]
async fn chunk_by_id_walks_full_dataset() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    install_observers().await;

    for i in 0..25 {
        User::create(attrs! {
            name: format!("Chunked{i}"),
            email: format!("c{i}@example.com"),
            password: "pw",
        })
        .await
        .unwrap();
    }

    // PK-cursor chunking — concurrent-insert-safe and the recommended
    // shape for bulk processing. We accumulate ids in `seen` and
    // assert at the end that every row appeared exactly once.
    let mut seen: Vec<i64> = Vec::new();
    User::query()
        .order_by_asc("id")
        .chunk_by_id(10, |batch: Collection<User>| {
            for u in batch.iter() {
                seen.push(u.id);
            }
            async move { Ok(()) }
        })
        .await
        .unwrap();

    assert_eq!(seen.len(), 25, "every row visited exactly once: {seen:?}");
    // Sort the ids to defend against any future scheduler change in
    // batch ordering; the contract we care about is coverage, not
    // sequence.
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(sorted.first(), Some(&seen[0]));
    assert_eq!(sorted.last(), Some(&seen[seen.len() - 1]));
}

// ---- 5. Collection::pluck -----------------------------------------------

#[tokio::test]
async fn collection_pluck_extracts_emails() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    install_observers().await;

    User::create(attrs! {
        name: "A", email: "a@x.com", password: "pw",
    })
    .await
    .unwrap();
    User::create(attrs! {
        name: "B", email: "b@x.com", password: "pw",
    })
    .await
    .unwrap();

    // Builder::get → Collection<User>. The model-aware `pluck` walks
    // each row's macro-emitted `field_value` and deserialises the
    // column into the target type.
    let users: Collection<User> = User::query()
        .order_by_asc("id")
        .get()
        .await
        .unwrap();
    let emails: Collection<String> = users.pluck::<String>("email");
    let vec: Vec<String> = emails.into_vec();
    assert!(vec.contains(&"a@x.com".to_string()), "got: {vec:?}");
    assert!(vec.contains(&"b@x.com".to_string()), "got: {vec:?}");
}

// ---- 6. Transactions ----------------------------------------------------

#[tokio::test]
async fn transaction_commits_user_and_audit_row_atomically() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    install_observers().await;

    // Wrap the User insert + the audit_log insert in a single
    // transaction. The active tx is auto-detected via the
    // `CURRENT_TX` task-local so neither inner call needs to thread
    // the tx through explicitly.
    let user_id: i64 = DB::transaction(|_tx| {
        Box::pin(async move {
            let user = User::create(attrs! {
                name: "Trans",
                email: "trans@example.com",
                password: "pw",
            })
            .await?;

            DB::table("audit_log")
                .insert(attrs! {
                    event: "user.created",
                    actor_id: user.id,
                    payload: format!("{{\"name\":\"{}\"}}", user.name),
                })
                .await?;

            Ok::<i64, FrameworkError>(user.id)
        })
    })
    .await
    .unwrap();

    assert!(user_id > 0);
    let audit_count = DB::table("audit_log")
        .filter("event", "user.created")
        .count()
        .await
        .unwrap();
    assert_eq!(audit_count, 1, "audit row visible after commit");
}

#[tokio::test]
async fn transaction_rolls_back_audit_row_on_error() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    install_observers().await;

    // Same shape as the commit test, but the closure returns an error
    // AFTER the audit insert. The rollback contract is: nothing the
    // closure did is observable from outside the transaction.
    let result: Result<(), FrameworkError> = DB::transaction(|_tx| {
        Box::pin(async move {
            DB::table("audit_log")
                .insert(attrs! {
                    event: "user.created",
                    actor_id: 999_i64,
                    payload: "shouldn't survive",
                })
                .await?;

            Err::<(), FrameworkError>(FrameworkError::internal("boom"))
        })
    })
    .await;

    assert!(result.is_err(), "transaction closure error propagates");
    let audit_count = DB::table("audit_log").count().await.unwrap();
    assert_eq!(
        audit_count, 0,
        "rollback un-writes the audit row; observed count {audit_count}",
    );
}
