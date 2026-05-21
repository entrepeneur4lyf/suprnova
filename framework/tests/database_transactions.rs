//! Phase 10C T11 — Transactions integration tests.
//!
//! Exercises the four entry points and their composition with Model
//! / Builder paths:
//!
//! - [`DB::transaction`] (closure form, ambient `CURRENT_TX`)
//! - [`DB::begin_transaction`] (manual handle, `*_with_tx` shims)
//! - [`Transaction::savepoint`] + [`Transaction::rollback_to`]
//! - [`DB::transaction_with_attempts`] (retry-on-deadlock)
//!
//! Plus self-audit pins covering the closure form's CURRENT_TX
//! routing (reads see pending writes) and the nested-transaction
//! rejection contract.
//!
//! `DB::transaction` uses HRTB + `Pin<Box<dyn Future>>` to let the
//! closure body borrow the `&Transaction` across `.await` points.
//! Each test wraps its body in `Box::pin(async move { ... })`.

use chrono::{DateTime, Utc};
use suprnova::testing::TestDatabase;
use suprnova::FrameworkError;
use suprnova::{attrs, model, Model, DB};

#[model(
    table = "t11_accounts",
    fillable = ["owner", "balance"],
)]
pub struct T11Account {
    pub id: i64,
    pub owner: String,
    pub balance: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

async fn fixture() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t11_accounts (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            owner TEXT NOT NULL, \
            balance INTEGER NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
    T11Account::create(attrs! { owner: "alice", balance: 100i64 })
        .await
        .unwrap();
    T11Account::create(attrs! { owner: "bob", balance: 50i64 })
        .await
        .unwrap();
    db
}

#[tokio::test]
async fn transaction_commits_on_ok() {
    let _db = fixture().await;
    let result: Result<(), FrameworkError> = DB::transaction(|_tx| {
        Box::pin(async move {
            let mut alice = T11Account::query()
                .filter("owner", "alice")
                .first()
                .await?
                .unwrap();
            alice.balance -= 30;
            alice.save().await?;

            let mut bob = T11Account::query()
                .filter("owner", "bob")
                .first()
                .await?
                .unwrap();
            bob.balance += 30;
            bob.save().await?;
            Ok(())
        })
    })
    .await;

    assert!(result.is_ok(), "transaction returned {:?}", result);

    let alice = T11Account::query()
        .filter("owner", "alice")
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice.balance, 70);
    let bob = T11Account::query()
        .filter("owner", "bob")
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bob.balance, 80);
}

#[tokio::test]
async fn transaction_rolls_back_on_err() {
    let _db = fixture().await;
    let result: Result<(), FrameworkError> = DB::transaction(|_tx| {
        Box::pin(async move {
            let mut alice = T11Account::query()
                .filter("owner", "alice")
                .first()
                .await?
                .unwrap();
            alice.balance -= 30;
            alice.save().await?;
            Err::<(), _>(FrameworkError::bad_request("simulated failure"))
        })
    })
    .await;
    assert!(result.is_err());

    let alice = T11Account::query()
        .filter("owner", "alice")
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice.balance, 100, "rollback should restore");
}

#[tokio::test]
async fn savepoint_and_rollback_to_preserves_outer_work() {
    let _db = fixture().await;
    DB::transaction(|tx| {
        Box::pin(async move {
            let mut alice = T11Account::query()
                .filter("owner", "alice")
                .first()
                .await?
                .unwrap();
            alice.balance = 200;
            alice.save().await?;

            tx.savepoint("inner").await?;

            let mut bob = T11Account::query()
                .filter("owner", "bob")
                .first()
                .await?
                .unwrap();
            bob.balance = 999;
            bob.save().await?;

            tx.rollback_to("inner").await?;
            Ok::<(), FrameworkError>(())
        })
    })
    .await
    .unwrap();

    let alice = T11Account::query()
        .filter("owner", "alice")
        .first()
        .await
        .unwrap()
        .unwrap();
    let bob = T11Account::query()
        .filter("owner", "bob")
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice.balance, 200, "outer work commits");
    assert_eq!(bob.balance, 50, "inner work rolled back to savepoint");
}

#[tokio::test]
async fn manual_transaction_commit_persists_changes() {
    let _db = fixture().await;

    // Pre-load the row BEFORE begin_transaction. On SQLite the pool
    // has a single connection — once the tx checks it out, any read
    // that doesn't opt into the tx (no `with_tx`, no ambient
    // CURRENT_TX) would block waiting for the pool.
    let mut alice = T11Account::query()
        .filter("owner", "alice")
        .first()
        .await
        .unwrap()
        .unwrap();

    let tx = DB::begin_transaction().await.unwrap();
    alice.balance = 500;
    alice.save_with_tx(&tx).await.unwrap();
    tx.commit().await.unwrap();

    let alice = T11Account::query()
        .filter("owner", "alice")
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice.balance, 500);
}

#[tokio::test]
async fn transaction_with_attempts_succeeds_on_first_try() {
    let _db = fixture().await;
    let result: i64 = DB::transaction_with_attempts(3, |_tx| {
        Box::pin(async move { Ok::<i64, FrameworkError>(42) })
    })
    .await
    .unwrap();
    assert_eq!(result, 42);
}

#[tokio::test]
async fn transaction_with_attempts_retries_on_deadlock_error() {
    let _db = fixture().await;
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let c = counter.clone();

    let result: Result<(), FrameworkError> = DB::transaction_with_attempts(3, move |_tx| {
        let c = c.clone();
        Box::pin(async move {
            let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            if n < 3 {
                Err(FrameworkError::database("simulated deadlock detected"))
            } else {
                Ok(())
            }
        })
    })
    .await;

    assert!(result.is_ok(), "retries should converge: {:?}", result);
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[tokio::test]
async fn reads_inside_transaction_see_pending_writes_in_same_tx() {
    // Self-audit pin: a read inside `DB::transaction` MUST see the
    // pending insert / update from the same transaction. If the
    // Builder terminals were still routing through the pool, the
    // read would skip the un-committed row and this test would fail.
    let _db = fixture().await;

    let saw_pending = DB::transaction(|_tx| {
        Box::pin(async move {
            T11Account::create(attrs! { owner: "carol", balance: 25i64 }).await?;
            // Inside the same tx — the new row MUST be visible.
            let count: i64 = T11Account::query().count().await?;
            Ok::<i64, FrameworkError>(count)
        })
    })
    .await
    .unwrap();

    assert_eq!(
        saw_pending, 3,
        "the pending insert is visible to reads inside the same tx"
    );

    // After commit, the row is visible to readers outside the tx too.
    let count: i64 = T11Account::query().count().await.unwrap();
    assert_eq!(count, 3);
}

#[tokio::test]
async fn nested_db_transaction_is_rejected_at_runtime() {
    // Spec §"Nested DB::transaction is rejected at runtime": calling
    // DB::transaction inside an already-active transaction must error
    // instead of starting a sibling top-level transaction that would
    // commit / rollback independently of the outer scope. Users wanting
    // nested-rollback semantics use `tx.savepoint(name)`.
    let _db = fixture().await;

    let outer: Result<(), FrameworkError> = DB::transaction(|_tx| {
        Box::pin(async move {
            let inner = DB::transaction(|_t| {
                Box::pin(async move { Ok::<(), FrameworkError>(()) })
            })
            .await;
            assert!(inner.is_err(), "nested DB::transaction must error");
            let msg = format!("{}", inner.unwrap_err());
            assert!(
                msg.contains("nested DB::transaction is not supported"),
                "error message points users at savepoint: {msg}"
            );
            Ok(())
        })
    })
    .await;
    assert!(outer.is_ok());
}

#[tokio::test]
async fn builder_with_tx_routes_through_explicit_transaction() {
    // Builder::with_tx pins a read to the supplied tx without
    // installing CURRENT_TX. Pair it with Model::*_with_tx for
    // writes; this test confirms the read sees the pending write
    // through the same tx handle.
    let _db = fixture().await;

    let tx = DB::begin_transaction().await.unwrap();
    let mut alice = T11Account::query()
        .filter("owner", "alice")
        .with_tx(&tx)
        .first()
        .await
        .unwrap()
        .unwrap();
    alice.balance = 777;
    alice.save_with_tx(&tx).await.unwrap();

    // Read through the same tx — sees the pending update.
    let through_tx = T11Account::query()
        .filter("owner", "alice")
        .with_tx(&tx)
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        through_tx.balance, 777,
        "Builder::with_tx routes the read through the supplied tx"
    );

    tx.commit().await.unwrap();
    let after = T11Account::query()
        .filter("owner", "alice")
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.balance, 777);
}

// ---- AF5 — Model::create_with_tx ----------------------------------------
//
// Manual `DB::begin_transaction` does not install `CURRENT_TX`, so
// `Model::create()` inside a manual-tx scope silently routed to the
// pool — defeating the whole point of holding the handle. AF5 adds
// the missing `create_with_tx(&tx, attrs)` shim that mirrors
// `create()` event-for-event but pins the INSERT to `tx`. These two
// tests pin the contract end-to-end: commit lands the row, dropping
// the handle without committing rolls back.

#[tokio::test]
async fn create_with_tx_commits_atomically() {
    let _db = fixture().await;

    let tx = DB::begin_transaction().await.unwrap();
    let carol = T11Account::create_with_tx(&tx, attrs! { owner: "carol", balance: 999i64 })
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let row = T11Account::query()
        .filter("id", carol.id)
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.owner, "carol");
    assert_eq!(row.balance, 999);
}

#[tokio::test]
async fn create_with_tx_rolls_back_when_handle_dropped_without_commit() {
    let _db = fixture().await;

    let baseline = T11Account::query().get().await.unwrap().len();

    {
        let tx = DB::begin_transaction().await.unwrap();
        let _ =
            T11Account::create_with_tx(&tx, attrs! { owner: "dave", balance: 1i64 })
                .await
                .unwrap();
        // Drop `tx` without calling commit — the SeaORM
        // DatabaseTransaction's Drop rolls back any uncommitted work.
        drop(tx);
    }

    let after = T11Account::query().get().await.unwrap().len();
    assert_eq!(
        after, baseline,
        "uncommitted manual-tx create must not survive the dropped handle"
    );
    let lookup = T11Account::query()
        .filter("owner", "dave")
        .first()
        .await
        .unwrap();
    assert!(lookup.is_none(), "rollback must un-insert the in-tx row");
}
