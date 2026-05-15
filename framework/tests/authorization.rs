use suprnova::{Gate, FrameworkError};

#[derive(Debug)]
struct User {
    id: i64,
    is_admin: bool,
}

#[derive(Debug)]
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
