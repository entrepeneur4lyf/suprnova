use suprnova::{FrameworkError, Gate, policy};

// FIX 6: kebab-case resource names in generated action strings.

#[derive(Debug)]
struct User {
    id: i64,
    is_admin: bool,
}

#[derive(Debug)]
#[allow(dead_code)]
struct Post {
    id: i64,
    author_id: i64,
    is_public: bool,
}

#[tokio::test]
async fn gate_define_and_allows_for_closure() {
    Gate::define::<User, Post>("view-post", |user, post| {
        post.is_public || post.author_id == user.id || user.is_admin
    });

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let public_post = Post {
        id: 10,
        author_id: 99,
        is_public: true,
    };
    let private_post = Post {
        id: 11,
        author_id: 99,
        is_public: false,
    };
    let owned_post = Post {
        id: 12,
        author_id: 1,
        is_public: false,
    };

    assert!(Gate::allows("view-post", &alice, &public_post));
    assert!(!Gate::allows("view-post", &alice, &private_post));
    assert!(Gate::allows("view-post", &alice, &owned_post));
}

#[tokio::test]
async fn gate_authorize_returns_forbidden_when_denied() {
    Gate::define::<User, Post>("edit-post", |user, post| post.author_id == user.id);
    let alice = User {
        id: 1,
        is_admin: false,
    };
    let foreign_post = Post {
        id: 99,
        author_id: 999,
        is_public: true,
    };
    let result = Gate::authorize("edit-post", &alice, &foreign_post);
    assert!(matches!(result, Err(FrameworkError::Unauthorized)));
}

// ── Task 3: #[policy] proc-macro test ────────────────────────────────────────

struct Comment {
    pub author_id: i64,
}

struct CommentPolicy;

#[policy(User, Comment)]
impl CommentPolicy {
    fn view(_user: &User, _comment: &Comment) -> bool {
        true
    }
    fn update(user: &User, comment: &Comment) -> bool {
        comment.author_id == user.id
    }
}

#[test]
fn policy_macro_registers_gates_via_inventory() {
    // The #[policy] attribute should have wired up gates for
    // "view-comment" and "update-comment" via inventory::submit!.
    // Eagerly trigger inventory collection at startup:
    suprnova::authorization::init_policies();

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let mine = Comment { author_id: 1 };
    let theirs = Comment { author_id: 99 };

    assert!(Gate::allows("view-comment", &alice, &mine));
    assert!(Gate::allows("update-comment", &alice, &mine));
    assert!(!Gate::allows("update-comment", &alice, &theirs));
}

// ── FIX 6: PascalCase → kebab-case action names ───────────────────────────────

#[allow(dead_code)]
struct UserProfile {
    pub owner_id: i64,
}

struct UserProfilePolicy;

#[policy(User, UserProfile)]
impl UserProfilePolicy {
    fn view(_user: &User, _profile: &UserProfile) -> bool {
        true
    }
    fn update(user: &User, profile: &UserProfile) -> bool {
        profile.owner_id == user.id
    }
}

// ── FIX 4: Async gate support ─────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Item {
    pub is_public: bool,
    pub owner_id: i64,
}

/// An async gate can be registered and invoked via `allows_async`.
///
/// The future simulates an async DB lookup (yield_now) to prove it runs
/// asynchronously. The same gate must return `false` via sync `allows()`
/// (default-deny for async-registered gates called via the sync path).
#[tokio::test]
async fn gate_async_closure_allows_and_denies() {
    Gate::define_async("view-item-async", |user: &User, item: &Item| {
        let is_public = item.is_public;
        let owner_id = item.owner_id;
        let user_id = user.id;
        async move {
            // Simulate an async operation (e.g. DB lookup).
            tokio::task::yield_now().await;
            is_public || owner_id == user_id
        }
    });

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let public_item = Item {
        is_public: true,
        owner_id: 99,
    };
    let private_foreign = Item {
        is_public: false,
        owner_id: 99,
    };
    let owned_item = Item {
        is_public: false,
        owner_id: 1,
    };

    // Async path works correctly.
    assert!(Gate::allows_async("view-item-async", &alice, &public_item).await);
    assert!(!Gate::allows_async("view-item-async", &alice, &private_foreign).await);
    assert!(Gate::allows_async("view-item-async", &alice, &owned_item).await);

    // Sync path must default-deny for async-registered gates.
    assert!(
        !Gate::allows("view-item-async", &alice, &public_item),
        "sync allows() on an async gate must return false (default deny)"
    );
}

/// `allows_async` also works for sync-registered gates (backwards compatible).
#[tokio::test]
async fn gate_allows_async_dispatches_sync_registered_gates() {
    Gate::define::<User, Item>("read-item-sync", |user, item| {
        item.is_public || item.owner_id == user.id
    });

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let public_item = Item {
        is_public: true,
        owner_id: 99,
    };
    let private_foreign = Item {
        is_public: false,
        owner_id: 99,
    };

    assert!(Gate::allows_async("read-item-sync", &alice, &public_item).await);
    assert!(!Gate::allows_async("read-item-sync", &alice, &private_foreign).await);
}

/// `authorize_async` returns `Err(Unauthorized)` when denied.
#[tokio::test]
async fn gate_authorize_async_returns_unauthorized_when_denied() {
    Gate::define_async("edit-item-async", |user: &User, item: &Item| {
        let owner_id = item.owner_id;
        let user_id = user.id;
        async move { owner_id == user_id }
    });

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let foreign_item = Item {
        is_public: false,
        owner_id: 99,
    };
    let owned_item = Item {
        is_public: false,
        owner_id: 1,
    };

    assert!(
        Gate::authorize_async("edit-item-async", &alice, &owned_item)
            .await
            .is_ok()
    );
    assert!(matches!(
        Gate::authorize_async("edit-item-async", &alice, &foreign_item).await,
        Err(FrameworkError::Unauthorized)
    ));
}

// ── FIX 5: init_policies callable independently of Server::serve ──────────────

/// Verifies that `init_policies()` is idempotent and works without `Server::serve`.
///
/// Background workers and CLI commands call `Application::run()` which now also
/// invokes `init_policies()` before dispatching subcommands. This test proves
/// the inner guard is correct: gates registered via `#[policy]` work regardless
/// of which code path called `init_policies()`.
#[test]
fn init_policies_registers_gates_without_server_serve() {
    // Call init_policies directly — no Server::serve, no Application::run.
    suprnova::authorization::init_policies();
    // Also safe to call again (idempotent).
    suprnova::authorization::init_policies();

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let my_comment = Comment { author_id: 1 };

    // Gate wired up by #[policy(User, Comment)] above — must work.
    assert!(
        Gate::allows("view-comment", &alice, &my_comment),
        "init_policies() must register #[policy] gates without Server::serve"
    );
}

/// Without FIX 6, `UserProfile` becomes `"view-userprofile"` (no hyphen).
/// With FIX 6, it must be `"view-user-profile"` (PascalCase kebab-ified).
#[test]
fn policy_macro_kebab_cases_multi_word_resource() {
    suprnova::authorization::init_policies();

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let mine = UserProfile { owner_id: 1 };
    let theirs = UserProfile { owner_id: 99 };

    // These must exist with proper kebab-case names.
    assert!(Gate::allows("view-user-profile", &alice, &mine));
    assert!(Gate::allows("update-user-profile", &alice, &mine));
    assert!(!Gate::allows("update-user-profile", &alice, &theirs));

    // The buggy flat-lowercase name must NOT exist.
    assert!(
        !Gate::allows("view-userprofile", &alice, &mine),
        "flat-lowercase action 'view-userprofile' must not be registered"
    );
}
