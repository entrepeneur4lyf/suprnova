use std::any::Any;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::service_fn;
use serial_test::serial;
use std::convert::Infallible;
use suprnova::auth::request_state::request_state_scope_for_test;
use suprnova::rbac::migrations::CreateRbacTables;
use suprnova::testing::TestDatabase;
use suprnova::{
    Auth, Authenticatable, HasRoles, HttpResponse, Middleware, Next, PermissionMiddleware, Request,
    RoleMiddleware,
};

#[derive(Clone)]
struct User {
    id: i64,
}

impl Authenticatable for User {
    fn get_auth_identifier(&self) -> String {
        self.id.to_string()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl HasRoles for User {}

struct TestMigrator;

impl sea_orm_migration::MigratorTrait for TestMigrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![Box::new(CreateRbacTables)]
    }
}

async fn setup() -> TestDatabase {
    let db = TestDatabase::fresh::<TestMigrator>().await.unwrap();
    suprnova::rbac::create_role("author").await.unwrap();
    suprnova::rbac::create_permission("articles.create")
        .await
        .unwrap();
    suprnova::rbac::create_permission("articles.publish")
        .await
        .unwrap();
    suprnova::rbac::give_permission_to_role("author", "articles.create")
        .await
        .unwrap();
    // Use the model's own discriminator (the fully-qualified type name)
    // so the seeded assignment matches what the trait checks and the
    // Role/Permission middleware query at runtime.
    suprnova::rbac::assign_role_to_model(&User { id: 7 }.rbac_model_type(), "7", "author")
        .await
        .unwrap();
    db
}

async fn request(path: &str) -> Request {
    request_with_inertia(path, false).await
}

async fn inertia_request(path: &str) -> Request {
    request_with_inertia(path, true).await
}

async fn request_with_inertia(path: &str, inertia: bool) -> Request {
    let path = path.to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<Request>();
    let tx = Arc::new(std::sync::Mutex::new(Some(tx)));

    let tx_for_service = tx.clone();
    let server = tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let service = service_fn(move |hyper_req: hyper::Request<Incoming>| {
                let tx = tx_for_service.clone();
                async move {
                    let req = Request::new(hyper_req);
                    if let Some(tx) = tx.lock().unwrap().take() {
                        let _ = tx.send(req);
                    }
                    Ok::<_, Infallible>(
                        hyper::Response::builder()
                            .status(200)
                            .body(Full::new(Bytes::new()))
                            .unwrap(),
                    )
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                .await;
        }
    });

    let client = tokio::spawn(async move {
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let mut req = hyper::Request::builder()
            .method("GET")
            .uri(path)
            .header("Host", "localhost");
        if inertia {
            req = req.header("X-Inertia", "true");
        }
        let req = req.body(Full::new(Bytes::new())).unwrap();
        let _ = sender.send_request(req).await.unwrap();
    });

    let req = rx.await.unwrap();
    client.await.unwrap();
    server.await.unwrap();
    req
}

fn next_ok() -> Next {
    Arc::new(|_req| Box::pin(async { Ok(HttpResponse::text("ok")) }))
}

#[tokio::test]
#[serial]
async fn has_roles_reads_role_inherited_permissions_and_direct_permissions() {
    let _db = setup().await;
    let user = User { id: 7 };

    assert!(user.has_role("author").await.unwrap());
    assert!(user.has_permission_to("articles.create").await.unwrap());
    assert!(!user.has_permission_to("articles.publish").await.unwrap());

    user.give_permission_to("articles.publish").await.unwrap();
    assert!(user.has_permission_to("articles.publish").await.unwrap());
}

#[tokio::test]
#[serial]
async fn missing_roles_and_permissions_deny_by_default() {
    let _db = setup().await;
    let user = User { id: 8 };

    assert!(!user.has_role("author").await.unwrap());
    assert!(!user.has_permission_to("articles.create").await.unwrap());
}

#[tokio::test]
#[serial]
async fn middleware_allows_matching_role_and_permission() {
    let _db = setup().await;

    request_state_scope_for_test(async {
        Auth::set_user(Arc::new(User { id: 7 }));

        let role_response = match RoleMiddleware::<User>::new("author")
            .handle(request("/author").await, next_ok())
            .await
        {
            Ok(response) => response,
            Err(response) => panic!(
                "expected role middleware to pass, got {}",
                response.status_code()
            ),
        };
        assert_eq!(role_response.status_code(), 200);

        let permission_response = match PermissionMiddleware::<User>::new("articles.create")
            .handle(request("/articles").await, next_ok())
            .await
        {
            Ok(response) => response,
            Err(response) => panic!(
                "expected permission middleware to pass, got {}",
                response.status_code()
            ),
        };
        assert_eq!(permission_response.status_code(), 200);
    })
    .await;
}

#[tokio::test]
#[serial]
async fn middleware_returns_forbidden_when_permission_is_missing() {
    let _db = setup().await;

    request_state_scope_for_test(async {
        Auth::set_user(Arc::new(User { id: 7 }));

        let response = match PermissionMiddleware::<User>::new("articles.delete")
            .handle(request("/articles/delete").await, next_ok())
            .await
        {
            Ok(response) => panic!(
                "expected permission middleware to deny, got {}",
                response.status_code()
            ),
            Err(response) => response,
        };
        assert_eq!(response.status_code(), 403);
    })
    .await;
}

#[tokio::test]
#[serial]
async fn middleware_redirects_browser_denials_when_configured() {
    let _db = setup().await;

    request_state_scope_for_test(async {
        let response = match RoleMiddleware::<User>::redirect_to("admin", "/login")
            .handle(request("/admin").await, next_ok())
            .await
        {
            Ok(response) => panic!(
                "expected role middleware to deny, got {}",
                response.status_code()
            ),
            Err(response) => response,
        };
        assert_eq!(response.status_code(), 302);
        assert_eq!(response.header_value("Location"), Some("/login"));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn middleware_redirects_inertia_denials_with_conflict_location() {
    let _db = setup().await;

    request_state_scope_for_test(async {
        let response = match PermissionMiddleware::<User>::redirect_to("articles.delete", "/login")
            .handle(inertia_request("/articles/delete").await, next_ok())
            .await
        {
            Ok(response) => panic!(
                "expected permission middleware to deny, got {}",
                response.status_code()
            ),
            Err(response) => response,
        };
        assert_eq!(response.status_code(), 409);
        assert_eq!(response.header_value("X-Inertia-Location"), Some("/login"));
    })
    .await;
}
