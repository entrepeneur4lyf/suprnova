//! Regression: the webhook ingress route must resolve `WebhookContext.remote_addr`
//! through the trusted-proxy allowlist, NOT from a raw `X-Forwarded-For` header.
//!
//! With the default (empty) trusted-proxy allowlist, a client that sends a
//! forged `X-Forwarded-For: 9.9.9.9` must NOT have that value surface as the
//! webhook's `remote_addr` — an adapter that IP-allowlists via that field would
//! otherwise be trivially spoofable. The resolved value must be the actual TCP
//! peer (`127.0.0.1` for a loopback connect).

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use suprnova::payments::{
    Checkout, CreateCustomerRequest, CustomerRef, CustomerStore, PaymentError, PaymentProvider,
    PaymentProviderRegistry, PaymentResult, SessionPayload, StartSessionRequest, SubscribeRequest,
    Subscription, SubscriptionResult, UpdateCustomerRequest, UpdateSubscriptionRequest,
    WebhookContext, WebhookEvent, WebhookHandler, webhook_routes,
};
use suprnova::testing::TestDatabase;
use suprnova::{MiddlewareRegistry, Router, handle_request_with_peer};

/// Provider whose `verify` records the `remote_addr` the route passed in, then
/// fails closed so the route short-circuits at 401 — no DB query runs, so this
/// test needs no migrations.
struct RemoteAddrProbe {
    seen: Arc<Mutex<Option<Option<IpAddr>>>>,
}

impl PaymentProvider for RemoteAddrProbe {
    fn name(&self) -> &'static str {
        "remote-addr-probe"
    }
}

#[async_trait]
impl WebhookHandler for RemoteAddrProbe {
    fn verify(&self, ctx: &WebhookContext<'_>) -> PaymentResult<()> {
        *self.seen.lock().unwrap() = Some(ctx.remote_addr);
        // Reject so the route stops before any DB access.
        Err(PaymentError::WebhookSignature("probe rejects all".into()))
    }

    fn parse_event(&self, _body: &[u8]) -> PaymentResult<WebhookEvent> {
        Err(PaymentError::NotSupported("probe".into()))
    }
}

#[async_trait]
impl Checkout for RemoteAddrProbe {
    async fn start_session(&self, _req: StartSessionRequest) -> PaymentResult<SessionPayload> {
        Err(PaymentError::NotSupported("probe".into()))
    }
}

#[async_trait]
impl Subscription for RemoteAddrProbe {
    async fn subscribe(&self, _req: SubscribeRequest) -> PaymentResult<SubscriptionResult> {
        Err(PaymentError::NotSupported("probe".into()))
    }
    async fn update(&self, _req: UpdateSubscriptionRequest) -> PaymentResult<SubscriptionResult> {
        Err(PaymentError::NotSupported("probe".into()))
    }
    async fn cancel(&self, _id: &str, _at_period_end: bool) -> PaymentResult<SubscriptionResult> {
        Err(PaymentError::NotSupported("probe".into()))
    }
    async fn get(&self, _id: &str) -> PaymentResult<SubscriptionResult> {
        Err(PaymentError::NotFound("probe".into()))
    }
}

#[async_trait]
impl CustomerStore for RemoteAddrProbe {
    async fn create_customer(&self, _req: CreateCustomerRequest) -> PaymentResult<CustomerRef> {
        Err(PaymentError::NotSupported("probe".into()))
    }
    async fn update_customer(&self, _req: UpdateCustomerRequest) -> PaymentResult<CustomerRef> {
        Err(PaymentError::NotSupported("probe".into()))
    }
    async fn get_customer(&self, _id: &str) -> PaymentResult<CustomerRef> {
        Err(PaymentError::NotFound("probe".into()))
    }
    async fn delete_customer(&self, _id: &str) -> PaymentResult<()> {
        Err(PaymentError::NotFound("probe".into()))
    }
}

/// Spawn the webhook router behind `handle_request_with_peer`, threading the
/// accepted TCP peer IP exactly as the production accept loop does.
async fn spawn_server(router: Router) -> SocketAddr {
    let router = Arc::new(router);
    let middleware = Arc::new(MiddlewareRegistry::new());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral listener");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        let Ok((stream, peer)) = listener.accept().await else {
            return;
        };
        let peer_ip: Option<IpAddr> = Some(peer.ip());
        let io = TokioIo::new(stream);
        let svc = service_fn(move |req: hyper::Request<Incoming>| {
            let router = router.clone();
            let middleware = middleware.clone();
            async move {
                Ok::<_, Infallible>(
                    handle_request_with_peer(router, middleware, req, peer_ip).await,
                )
            }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(io, svc)
            .await;
    });

    addr
}

#[tokio::test]
async fn forged_x_forwarded_for_does_not_reach_remote_addr() {
    // A DB connection is required to mount the route, but `verify` rejects
    // before any query so it is never touched.
    let db = TestDatabase::sqlite_memory().await.expect("sqlite_memory");
    let conn = Arc::new(db.conn().clone());

    let seen = Arc::new(Mutex::new(None));
    let probe: Arc<dyn PaymentProvider> = Arc::new(RemoteAddrProbe { seen: seen.clone() });
    PaymentProviderRegistry::bind("remote-addr-probe", probe);

    let router: Router = webhook_routes(conn);
    let addr = spawn_server(router).await;

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn_task) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn_task.await;
    });

    let body = Bytes::from_static(b"{}");
    let req = hyper::Request::builder()
        .method("POST")
        .uri("/webhooks/payments/remote-addr-probe")
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .header("Content-Length", body.len().to_string())
        // Forged client-controlled header — must NOT be trusted.
        .header("X-Forwarded-For", "9.9.9.9")
        .body(Full::new(body))
        .unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send timeout")
        .expect("send_request");
    // verify() rejected → 401.
    assert_eq!(resp.status(), 401);
    let _ = resp.into_body().collect().await;

    let captured = seen
        .lock()
        .unwrap()
        .expect("verify should have been called");

    // The forged header must NOT win — the resolved address is the loopback
    // peer, because the default trusted-proxy allowlist is empty.
    assert_eq!(
        captured,
        Some(IpAddr::from([127, 0, 0, 1])),
        "remote_addr must be the TCP peer, not the spoofed X-Forwarded-For"
    );
    assert_ne!(
        captured,
        Some("9.9.9.9".parse::<IpAddr>().unwrap()),
        "spoofed X-Forwarded-For must never reach remote_addr"
    );
}
