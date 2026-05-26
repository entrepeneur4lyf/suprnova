//! Regression: HIGH audit finding `container` #291 — `TestContainer`
//! is thread-local, not async-task scoped. `TEST_CONTAINER` is a
//! `thread_local!`, and `App::get` / `App::make` consult it first. Tests
//! that exercise async work on a `flavor = "multi_thread"` runtime can
//! see the future migrate to a different worker thread, at which point
//! the override is no longer visible and `App::*` falls through to the
//! global container.
//!
//! The fix adds a `tokio::task_local!` `TASK_CONTAINER` and a
//! `TestContainer::scope(future)` helper that binds the override to the
//! future itself, not the calling thread. `App::*` lookups consult the
//! task-local first, then the existing thread-local, then the global
//! container — so `TestContainer::fake()` keeps working for sync /
//! current_thread callers while the new scope helper closes the
//! multi-thread / future-migration hole.
//!
//! These tests prove:
//! 1. `App::make` inside a scope sees the bound fake.
//! 2. The override survives `tokio::task::yield_now().await` on a
//!    multi-thread runtime (the case where the future may be picked up
//!    by a different worker after the yield).
//! 3. Two concurrent scopes don't bleed into each other.
//! 4. Task-local takes precedence over thread-local when both are
//!    active in the same call site (defensive — users shouldn't do
//!    this, but the precedence must be defined).

use std::sync::Arc;
use suprnova::App;
use suprnova::testing::TestContainer;

trait Tagger: Send + Sync + 'static {
    fn tag(&self) -> &'static str;
}

struct TagA;
impl Tagger for TagA {
    fn tag(&self) -> &'static str {
        "A"
    }
}

struct TagB;
impl Tagger for TagB {
    fn tag(&self) -> &'static str {
        "B"
    }
}

#[tokio::test]
async fn scope_binds_override_visible_to_app_make() {
    TestContainer::scope(async {
        TestContainer::bind::<dyn Tagger>(Arc::new(TagA));

        let resolved = App::make::<dyn Tagger>().expect("must resolve from task-local scope");
        assert_eq!(resolved.tag(), "A");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scope_survives_yield_across_worker_threads() {
    // This is the regression test for the audit finding: on a
    // multi-thread runtime the future can be picked up by any worker
    // after `yield_now`. Thread-local would be invisible from the new
    // worker; task-local is bound to the future itself, so it follows.
    TestContainer::scope(async {
        TestContainer::bind::<dyn Tagger>(Arc::new(TagA));

        // Force the runtime to consider re-scheduling. On a multi-thread
        // runtime with multiple workers this is the canonical "future
        // hops" trigger.
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }

        let resolved = App::make::<dyn Tagger>()
            .expect("override must still be visible after multi-yield across workers");
        assert_eq!(
            resolved.tag(),
            "A",
            "task-local must survive the multi-thread runtime hopping the future"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_scopes_do_not_bleed() {
    // Two scopes running concurrently must each see only their own
    // override. This is the load-bearing isolation guarantee for any
    // test framework — if it fails, parallel test runs would be
    // hopelessly flaky.
    let scope_a = TestContainer::scope(async {
        TestContainer::bind::<dyn Tagger>(Arc::new(TagA));
        // Yield once to let the other scope run.
        tokio::task::yield_now().await;
        let resolved = App::make::<dyn Tagger>().expect("scope A sees something");
        resolved.tag()
    });

    let scope_b = TestContainer::scope(async {
        TestContainer::bind::<dyn Tagger>(Arc::new(TagB));
        tokio::task::yield_now().await;
        let resolved = App::make::<dyn Tagger>().expect("scope B sees something");
        resolved.tag()
    });

    let (a, b) = tokio::join!(scope_a, scope_b);
    assert_eq!(a, "A", "scope A must see only its own override");
    assert_eq!(b, "B", "scope B must see only its own override");
}

#[tokio::test]
async fn task_local_takes_precedence_over_thread_local() {
    // Defensive precedence test: if both a thread-local fake guard and
    // a task-local scope are active, the task-local override wins.
    // (Real tests shouldn't do this, but the precedence must be
    // well-defined.)
    let _guard = TestContainer::fake();
    TestContainer::bind::<dyn Tagger>(Arc::new(TagA)); // thread-local

    TestContainer::scope(async {
        TestContainer::bind::<dyn Tagger>(Arc::new(TagB)); // task-local

        let resolved = App::make::<dyn Tagger>().unwrap();
        assert_eq!(
            resolved.tag(),
            "B",
            "task-local override must win over an outer thread-local fake"
        );
    })
    .await;

    // Outside the scope, the thread-local fake is what App sees again.
    let resolved = App::make::<dyn Tagger>().unwrap();
    assert_eq!(
        resolved.tag(),
        "A",
        "after scope exit, the outer thread-local override is visible again"
    );
}
