use suprnova::{Gate, FrameworkError, policy};

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
    Gate::define::<User, Post>("edit-post", |user, post| {
        post.author_id == user.id
    });
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

    let alice = User { id: 1, is_admin: false };
    let mine = Comment { author_id: 1 };
    let theirs = Comment { author_id: 99 };

    assert!(Gate::allows("view-comment", &alice, &mine));
    assert!(Gate::allows("update-comment", &alice, &mine));
    assert!(!Gate::allows("update-comment", &alice, &theirs));
}
