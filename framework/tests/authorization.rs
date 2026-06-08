use suprnova::authorization::Response;
use suprnova::{Authorizable, FrameworkError, Gate, policy};

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

// ── #[policy] proc-macro test ─────────────────────────────────────────────────

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

// ── PascalCase → kebab-case action names ──────────────────────────────────────

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

// ── #[policy] methods returning a rich Response ───────────────────────────────

struct Article {
    author_id: i64,
}

struct ArticlePolicy;

#[policy(User, Article)]
impl ArticlePolicy {
    // A `-> bool` method routes through `Gate::define` (unchanged behaviour).
    fn view(_user: &User, _article: &Article) -> bool {
        true
    }

    // A `-> Response` method routes through `Gate::define_with`, so the denial
    // carries a message + HTTP status that `inspect` / `authorize` surface.
    fn update(user: &User, article: &Article) -> Response {
        if article.author_id == user.id {
            Response::allow()
        } else {
            Response::deny_with_status(404, "Article not found.")
        }
    }
}

#[test]
fn policy_macro_routes_bool_and_response_returns() {
    suprnova::authorization::init_policies();

    let owner = User {
        id: 7,
        is_admin: false,
    };
    let stranger = User {
        id: 8,
        is_admin: false,
    };
    let article = Article { author_id: 7 };

    // The `-> bool` method registered a plain allow/deny gate.
    assert!(Gate::allows("view-article", &owner, &article));

    // The `-> Response` method allows the owner.
    assert!(Gate::allows("update-article", &owner, &article));

    // It denies a stranger with the rich message + status it returned — proof
    // the method routed to `define_with`, not `define`.
    let denied = Gate::inspect("update-article", &stranger, &article);
    assert!(denied.denied());
    assert_eq!(denied.message(), Some("Article not found."));
    assert_eq!(denied.status(), Some(404));

    // `authorize` propagates that status as a Domain error, not a bare 403.
    match Gate::authorize("update-article", &stranger, &article) {
        Err(FrameworkError::Domain {
            status_code,
            message,
        }) => {
            assert_eq!(status_code, 404);
            assert_eq!(message, "Article not found.");
        }
        other => panic!("expected Domain 404, got {other:?}"),
    }
}

// ── Async gate support ────────────────────────────────────────────────────────

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

// ── init_policies callable independently of Server::serve ────────────────────

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

/// A PascalCase resource type must be kebab-cased in the generated
/// action name: `UserProfile` → `"view-user-profile"`, not the
/// flat-lowercase `"view-userprofile"` (which would defeat ergonomic
/// `Gate::allows` calls).
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

// ── Introspection: has + abilities ───────────────────────────────────────────

#[test]
fn gate_has_returns_true_for_registered_action() {
    Gate::define::<User, Post>("publish-post", |user, _post| user.is_admin);
    assert!(
        Gate::has::<User, Post>("publish-post"),
        "has must report registered (action, U, R) tuples"
    );
    // Distinct U/R or missing action must report `false` — `has`
    // keys on the full tuple, not the action string alone.
    assert!(!Gate::has::<User, Comment>("publish-post"));
    assert!(!Gate::has::<User, Post>("delete-post-permanently"));
}

#[test]
fn gate_abilities_lists_registered_actions_deduped() {
    Gate::define::<User, Post>("archive-post", |u, _| u.is_admin);
    Gate::define::<User, Post>("unarchive-post", |u, _| u.is_admin);
    // Same action against a DIFFERENT resource type should NOT
    // produce a duplicate in `abilities()` — it dedupes by action
    // string the same way Laravel's `Gate::abilities()` does.
    Gate::define::<User, Comment>("archive-post", |u, _| u.is_admin);

    let abilities = Gate::abilities();
    let count_archive = abilities.iter().filter(|a| *a == "archive-post").count();
    assert_eq!(
        count_archive, 1,
        "abilities must dedupe by action name; got {abilities:?}"
    );
    assert!(abilities.contains(&"unarchive-post".to_string()));
}

// ── Multi-action: any / none / check (sync) ──────────────────────────────────

#[tokio::test]
async fn gate_any_returns_true_when_at_least_one_allows() {
    Gate::define::<User, Post>("any-view", |_u, p| p.is_public);
    Gate::define::<User, Post>("any-edit", |u, p| p.author_id == u.id);

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let public_post = Post {
        id: 1,
        author_id: 99,
        is_public: true,
    };
    let foreign_private = Post {
        id: 2,
        author_id: 99,
        is_public: false,
    };

    // Public post: view allows, edit denies → any is true.
    assert!(Gate::any(&["any-view", "any-edit"], &alice, &public_post));
    // Foreign private: both deny → any is false.
    assert!(!Gate::any(
        &["any-view", "any-edit"],
        &alice,
        &foreign_private
    ));
}

#[tokio::test]
async fn gate_none_is_inverse_of_any() {
    Gate::define::<User, Post>("none-view", |_u, p| p.is_public);

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let private_post = Post {
        id: 1,
        author_id: 99,
        is_public: false,
    };
    // none-view denies on private → none returns true (no actions allow).
    assert!(Gate::none(&["none-view"], &alice, &private_post));
}

#[tokio::test]
async fn gate_check_requires_all_actions_to_allow() {
    Gate::define::<User, Post>("check-view", |_u, p| p.is_public);
    Gate::define::<User, Post>("check-comment", |_u, _p| true);

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let public_post = Post {
        id: 1,
        author_id: 99,
        is_public: true,
    };
    let private_post = Post {
        id: 2,
        author_id: 99,
        is_public: false,
    };

    // Both allow on public → check is true.
    assert!(Gate::check(
        &["check-view", "check-comment"],
        &alice,
        &public_post
    ));
    // check-view denies on private → check is false.
    assert!(!Gate::check(
        &["check-view", "check-comment"],
        &alice,
        &private_post
    ));
    // Empty slice is vacuously true (matches Iterator::all).
    assert!(Gate::check::<User, Post>(&[], &alice, &public_post));
}

// ── Multi-action: any / none / check (async) ─────────────────────────────────

#[tokio::test]
async fn gate_any_async_works_with_mixed_sync_and_async_registrations() {
    Gate::define::<User, Post>("aa-sync", |u, _| u.is_admin);
    Gate::define_async::<User, Post, _, _>("aa-async", |u, _p| {
        let admin = u.is_admin;
        async move { admin }
    });

    let alice_admin = User {
        id: 1,
        is_admin: true,
    };
    let bob = User {
        id: 2,
        is_admin: false,
    };
    let post = Post {
        id: 1,
        author_id: 1,
        is_public: false,
    };

    // Admin: both allow → any_async is true.
    assert!(Gate::any_async(&["aa-sync", "aa-async"], &alice_admin, &post).await);
    // Non-admin: both deny → any_async is false.
    assert!(!Gate::any_async(&["aa-sync", "aa-async"], &bob, &post).await);
}

#[tokio::test]
async fn gate_check_async_short_circuits_on_first_deny() {
    Gate::define::<User, Post>("ca-cheap-deny", |_u, _p| false);
    // This second gate would never be called if check_async
    // short-circuits — we can't directly assert "wasn't called"
    // without observable side effects, but a passing test confirms
    // the iteration completes (otherwise it would hang forever in
    // a real wait).
    Gate::define_async::<User, Post, _, _>("ca-expensive-allow", |_u, _p| async {
        // Cheap async to avoid an actual hang in misuse.
        true
    });

    let bob = User {
        id: 2,
        is_admin: false,
    };
    let post = Post {
        id: 1,
        author_id: 1,
        is_public: false,
    };

    // First gate denies → check_async returns false without
    // accepting the second gate's allow.
    assert!(!Gate::check_async(&["ca-cheap-deny", "ca-expensive-allow"], &bob, &post).await);
}

// ── Authorizable trait sugar ─────────────────────────────────────────────────

impl Authorizable for User {}

#[tokio::test]
async fn authorizable_can_delegates_to_gate_allows() {
    Gate::define::<User, Post>("can-view", |_u, p| p.is_public);

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let public_post = Post {
        id: 1,
        author_id: 99,
        is_public: true,
    };
    let private_post = Post {
        id: 2,
        author_id: 99,
        is_public: false,
    };

    assert!(alice.can("can-view", &public_post));
    assert!(!alice.can("can-view", &private_post));
    assert!(alice.cannot("can-view", &private_post));
}

#[tokio::test]
async fn authorizable_authorize_returns_unauthorized_on_deny() {
    Gate::define::<User, Post>("can-edit", |u, p| p.author_id == u.id);

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let foreign = Post {
        id: 99,
        author_id: 999,
        is_public: true,
    };

    let result = alice.authorize("can-edit", &foreign);
    assert!(matches!(result, Err(FrameworkError::Unauthorized)));
}

#[tokio::test]
async fn authorizable_authorize_maps_rich_denial_to_domain() {
    // The `Authorizable::authorize` doc promises that bare denials become
    // `FrameworkError::Unauthorized` while rich denials (from
    // `Gate::define_with` / `Gate::define_async_with`) become
    // `FrameworkError::Domain` carrying the message + status.
    Gate::define_with::<User, Post>("auth-trait-rich-deny", |_u, _p| {
        Response::deny_as_not_found()
    });
    let alice = User {
        id: 1,
        is_admin: false,
    };
    let post = Post {
        id: 1,
        author_id: 99,
        is_public: false,
    };

    match alice.authorize("auth-trait-rich-deny", &post) {
        Err(FrameworkError::Domain { status_code, .. }) => assert_eq!(status_code, 404),
        other => panic!("expected Domain 404 from rich denial, got {other:?}"),
    }
}

#[tokio::test]
async fn authorizable_authorize_async_maps_rich_denial_to_domain() {
    Gate::define_async_with::<User, Post, _, _>("auth-trait-rich-deny-async", |_u, _p| async {
        Response::deny_as_not_found()
    });
    let alice = User {
        id: 1,
        is_admin: false,
    };
    let post = Post {
        id: 1,
        author_id: 99,
        is_public: false,
    };

    match alice
        .authorize_async("auth-trait-rich-deny-async", &post)
        .await
    {
        Err(FrameworkError::Domain { status_code, .. }) => assert_eq!(status_code, 404),
        other => panic!("expected Domain 404 from rich denial, got {other:?}"),
    }
}

#[tokio::test]
async fn authorizable_can_async_dispatches_async_gates() {
    Gate::define_async::<User, Post, _, _>("can-async-view", |_u, p| {
        let public = p.is_public;
        async move { public }
    });

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let public_post = Post {
        id: 1,
        author_id: 99,
        is_public: true,
    };

    assert!(alice.can_async("can-async-view", &public_post).await);
    assert!(!alice.cannot_async("can-async-view", &public_post).await);
}

// ── Rich Response: define_with + inspect + raw ───────────────────────────────

#[tokio::test]
async fn gate_define_with_returns_rich_response() {
    Gate::define_with::<User, Post>("dw-update", |user, post| {
        if post.author_id == user.id {
            Response::allow()
        } else {
            Response::deny_with("You do not own this post.")
        }
    });

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let owned = Post {
        id: 1,
        author_id: 1,
        is_public: false,
    };
    let foreign = Post {
        id: 2,
        author_id: 99,
        is_public: false,
    };

    let ok = Gate::inspect("dw-update", &alice, &owned);
    assert!(ok.allowed());
    assert_eq!(ok.message(), None);

    let denied = Gate::inspect("dw-update", &alice, &foreign);
    assert!(denied.denied());
    assert_eq!(denied.message(), Some("You do not own this post."));
    // allows() still collapses the rich decision to a bool.
    assert!(!Gate::allows("dw-update", &alice, &foreign));
}

#[tokio::test]
async fn gate_inspect_default_denies_undefined_action() {
    let alice = User {
        id: 1,
        is_admin: false,
    };
    let post = Post {
        id: 1,
        author_id: 1,
        is_public: true,
    };
    let r = Gate::inspect("dw-never-defined", &alice, &post);
    assert!(r.denied());
    assert_eq!(r.status(), None);
    assert_eq!(r.message(), None);
}

#[tokio::test]
async fn gate_raw_distinguishes_undefined_from_deny() {
    Gate::define::<User, Post>("raw-deny", |_u, _p| false);
    Gate::define::<User, Post>("raw-allow", |_u, _p| true);

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let post = Post {
        id: 1,
        author_id: 1,
        is_public: true,
    };

    // Undefined ability → None (no rule), distinct from an explicit deny.
    assert!(Gate::raw("raw-undefined", &alice, &post).is_none());
    // Explicit deny → Some(denied).
    assert!(Gate::raw("raw-deny", &alice, &post).is_some_and(|r| r.denied()));
    // Allow → Some(allowed).
    assert!(Gate::raw("raw-allow", &alice, &post).is_some_and(|r| r.allowed()));
}

#[tokio::test]
async fn gate_authorize_carries_status_from_rich_response() {
    Gate::define_with::<User, Post>("dw-secret", |_u, _p| Response::deny_as_not_found());
    let alice = User {
        id: 1,
        is_admin: false,
    };
    let post = Post {
        id: 1,
        author_id: 99,
        is_public: false,
    };

    match Gate::authorize("dw-secret", &alice, &post) {
        Err(FrameworkError::Domain { status_code, .. }) => assert_eq!(status_code, 404),
        other => panic!("expected Domain 404 from deny_as_not_found, got {other:?}"),
    }
}

#[tokio::test]
async fn gate_inspect_async_with_define_async_with() {
    Gate::define_async_with::<User, Post, _, _>("dw-async-update", |user, post| {
        let owns = post.author_id == user.id;
        async move {
            tokio::task::yield_now().await;
            if owns {
                Response::allow()
            } else {
                Response::deny_with("not yours (async)")
            }
        }
    });

    let alice = User {
        id: 1,
        is_admin: false,
    };
    let foreign = Post {
        id: 9,
        author_id: 99,
        is_public: false,
    };

    let r = Gate::inspect_async("dw-async-update", &alice, &foreign).await;
    assert!(r.denied());
    assert_eq!(r.message(), Some("not yours (async)"));
    assert!(!Gate::allows_async("dw-async-update", &alice, &foreign).await);
}

// ── before / after hooks ─────────────────────────────────────────────────────
//
// Each hook test uses a DEDICATED user type. before/after hooks are keyed by
// the user's TypeId in the process-global registry, so registering one against
// the shared `User` type would leak into every other parallel test. A unique
// per-test user type isolates the hook completely.

#[derive(Debug)]
struct AdminUser {
    is_admin: bool,
}

#[tokio::test]
async fn gate_before_hook_short_circuits_and_continues() {
    // The gate always denies; the before hook grants admins everything.
    Gate::define::<AdminUser, Post>("bh-edit", |_u, _p| false);
    Gate::before::<AdminUser>(|u, _action| u.is_admin.then_some(true));

    let admin = AdminUser { is_admin: true };
    let regular = AdminUser { is_admin: false };
    let post = Post {
        id: 1,
        author_id: 1,
        is_public: false,
    };

    // Admin: before short-circuits the denying gate → allowed.
    assert!(Gate::allows("bh-edit", &admin, &post));
    // Non-admin: before returns None → the gate decides → denied.
    assert!(!Gate::allows("bh-edit", &regular, &post));
}

#[derive(Debug)]
struct AfterUser {
    #[allow(dead_code)]
    id: i64,
}

#[tokio::test]
async fn gate_after_hook_fills_only_undecided() {
    // A denying gate for one action; the after hook ALWAYS returns allow.
    Gate::define::<AfterUser, Post>("ah-defined", |_u, _p| false);
    Gate::after::<AfterUser>(|_u, _action, _decided| Some(true));

    let user = AfterUser { id: 1 };
    let post = Post {
        id: 1,
        author_id: 1,
        is_public: false,
    };

    // Undefined ability: result is None → the after hook FILLS it → allowed.
    assert!(Gate::allows("ah-undefined", &user, &post));
    // Defined-deny: result is Some(false) → after CANNOT override → denied.
    assert!(!Gate::allows("ah-defined", &user, &post));
}

#[derive(Debug)]
struct AsyncHookUser {
    is_admin: bool,
}

#[tokio::test]
async fn gate_before_hook_applies_to_async_path() {
    Gate::define::<AsyncHookUser, Post>("ah-async-edit", |_u, _p| false);
    Gate::before::<AsyncHookUser>(|u, _action| u.is_admin.then_some(true));

    let admin = AsyncHookUser { is_admin: true };
    let regular = AsyncHookUser { is_admin: false };
    let post = Post {
        id: 1,
        author_id: 1,
        is_public: false,
    };

    // The before hook fires on the async evaluation path too.
    assert!(Gate::allows_async("ah-async-edit", &admin, &post).await);
    assert!(!Gate::allows_async("ah-async-edit", &regular, &post).await);
}
