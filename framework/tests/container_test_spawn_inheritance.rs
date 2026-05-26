//! Regression: HIGH audit finding `container` #291 — completeness pass.
//!
//! `TestContainer::scope` binds the override to a future. `tokio::spawn`'d
//! sub-tasks do NOT inherit task-locals, which left a documented gap
//! around tests that fan out to sub-tasks via bare `tokio::spawn`.
//!
//! `TestContainer::spawn` closes that gap: it captures the current
//! task-local container and re-installs it inside the spawned future,
//! so the fakes registered in the parent scope remain visible.
//!
//! These tests prove:
//! 1. `TestContainer::spawn` from inside a scope inherits the override.
//! 2. Bare `tokio::spawn` from inside a scope does NOT inherit — this
//!    documents *why* `TestContainer::spawn` exists, and guards against
//!    a regression where someone "helpfully" makes `tokio::spawn`
//!    inherit task-locals (it can't; that would require runtime
//!    cooperation we don't have).
//! 3. `TestContainer::spawn` outside a scope falls through to
//!    `tokio::spawn` unchanged.
//! 4. Bindings added inside the spawned task become visible to the
//!    parent scope — the shared `Arc<RwLock<Container>>` is the same.

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_inherits_task_local_container() {
    // Register the fake inside the scope, then spawn a sub-task via
    // TestContainer::spawn. The sub-task must resolve the fake — not
    // fall through to the global container.
    TestContainer::scope(async {
        TestContainer::bind::<dyn Tagger>(Arc::new(TagA));

        let handle = TestContainer::spawn(async {
            let resolved = App::make::<dyn Tagger>()
                .expect("spawned sub-task must inherit task-local override");
            resolved.tag()
        });

        let tag = handle.await.expect("spawned task must succeed");
        assert_eq!(
            tag, "A",
            "TestContainer::spawn must carry the task-local container into the sub-task"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bare_tokio_spawn_does_not_inherit_task_local_container() {
    // This is a guard test: it documents the precise reason
    // TestContainer::spawn exists. If a future runtime change made
    // bare tokio::spawn inherit task-locals, this test would fail and
    // prompt re-evaluation of whether TestContainer::spawn is still
    // needed.
    TestContainer::scope(async {
        TestContainer::bind::<dyn Tagger>(Arc::new(TagA));

        let handle = tokio::spawn(async {
            // App::make falls through to the global container here —
            // we don't expect dyn Tagger to be bound globally, so this
            // is None.
            App::make::<dyn Tagger>().is_none()
        });

        let was_none = handle.await.expect("spawned task must succeed");
        assert!(
            was_none,
            "bare tokio::spawn must NOT see the task-local fake — \
             if this assertion ever flips, re-evaluate whether \
             TestContainer::spawn is still necessary"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_outside_scope_falls_through_to_tokio_spawn() {
    // Outside a TestContainer::scope block, TestContainer::spawn must
    // behave exactly like tokio::spawn — no task-local to capture.
    let handle = TestContainer::spawn(async {
        // No scope active; App::make should miss (returning None for
        // an unregistered trait).
        App::make::<dyn Tagger>().is_none()
    });

    let was_none = handle.await.expect("spawned task must succeed");
    assert!(
        was_none,
        "TestContainer::spawn outside a scope must fall through to tokio::spawn"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bindings_added_in_spawned_task_visible_to_parent() {
    // The captured Arc<RwLock<Container>> is shared between parent and
    // spawned task. A binding added inside the sub-task is visible to
    // the parent after the sub-task commits — this matches the
    // semantics inside the parent scope itself (both write to the same
    // task-local container).
    TestContainer::scope(async {
        let handle = TestContainer::spawn(async {
            TestContainer::bind::<dyn Tagger>(Arc::new(TagB));
        });
        handle.await.expect("spawned task must succeed");

        let resolved = App::make::<dyn Tagger>()
            .expect("binding added in spawned sub-task must be visible to parent scope");
        assert_eq!(
            resolved.tag(),
            "B",
            "the shared task-local container is mutated by the sub-task"
        );
    })
    .await;
}
