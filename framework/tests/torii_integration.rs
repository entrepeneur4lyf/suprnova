//! Integration tests for Torii-backed authentication.
//!
//! These tests exercise the full stack: `ToriiConfig` → `init_torii` →
//! `Auth::password()` → torii → SeaORM (SQLite in-memory).
//!
//! # Design: shared runtime + one-time setup
//!
//! SQLx's in-memory SQLite pool is bound to the tokio `Runtime` it was created
//! on. Each `#[tokio::test]` spawns its own runtime; when that runtime drops,
//! the pool closes. A subsequent test on a new runtime then fails with
//! "no such table" because the global `TORII` `OnceLock` still holds a
//! reference to the stale pool.
//!
//! Fix: one `Runtime` shared across all tests via `once_cell::sync::Lazy`.
//!
//! Additionally, Torii's migrations use `CREATE INDEX IF NOT EXISTS` for some
//! indexes but not all (an upstream quirk). Running `init_torii` twice on the
//! same database therefore panics on the duplicate index. `SETUP` ensures the
//! runtime and Torii are both initialised exactly once before any test body
//! runs, regardless of parallel execution order.

use once_cell::sync::Lazy;
use std::sync::Arc;
use tokio::runtime::Runtime;

use suprnova::torii_integration::{init_torii, middleware::BearerTokenMiddleware, ToriiConfig};
use suprnova::Auth;

/// One tokio runtime shared across every test in this file.
static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// One-time Torii initialisation shared across all tests.
///
/// Accessing `SETUP` (via `Lazy::force`) is idempotent and thread-safe.
static SETUP: Lazy<()> = Lazy::new(|| {
    RT.block_on(async {
        let config = ToriiConfig::sqlite_in_memory()
            .await
            .expect("sqlite in-memory connection");
        init_torii(config).await.expect("init_torii");
    });
});

/// Register a user then authenticate with the correct password.
///
/// Verifies the returned `User` IDs match and no error is raised.
#[test]
fn password_register_and_authenticate_round_trip() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let user = Auth::password()
            .register("test@example.com", "verySecure1!")
            .await
            .unwrap();
        assert_eq!(user.email, "test@example.com");

        let (user2, _session) = Auth::password()
            .authenticate("test@example.com", "verySecure1!", None, None)
            .await
            .unwrap();
        assert_eq!(user.id, user2.id);
    });
}

/// Authenticating with the wrong password must return an error.
#[test]
fn wrong_password_fails_authentication() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        Auth::password()
            .register("wrong@example.com", "correctPassword!")
            .await
            .unwrap();

        let result = Auth::password()
            .authenticate("wrong@example.com", "badPassword", None, None)
            .await;

        assert!(result.is_err());
    });
}

/// Passkey registration returns a non-empty challenge, the echoed email, and an rp_id.
///
/// This test does not complete a full WebAuthn round-trip (that requires a browser).
/// It verifies that `begin_registration` wires correctly all the way from
/// `Auth::passkey()` → `Webauthn` → `PasskeyRegistrationChallenge`.
#[test]
fn passkey_registration_challenge_returns_options() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let challenge = Auth::passkey()
            .begin_registration("alice@example.com")
            .await
            .unwrap();

        assert!(!challenge.challenge.is_empty());
        assert_eq!(challenge.user_email, "alice@example.com");
        assert!(!challenge.rp_id.is_empty());
    });
}

/// Magic-link send returns a non-empty, substantial token string.
///
/// Verifies the full path: `Auth::magic_link()` → torii `MagicLinkService` →
/// `get_or_create_user` → token creation → plaintext token returned.
///
/// No mailer is configured, so the call degrades to pure token generation.
/// The callback URL is accepted but not emailed at this phase.
#[test]
fn magic_link_send_returns_token() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let token = Auth::magic_link()
            .send("magic@example.com", "http://localhost:8000/auth/magic")
            .await
            .unwrap();

        assert!(!token.is_empty());
        assert!(token.len() >= 16, "token should be a substantial random string");
    });
}

/// Magic-link consume returns the expected user and session.
///
/// Calls `send` to obtain a token then `consume` to exchange it for a
/// `(User, Session)`. Asserts the user email matches and the session is
/// linked to the same user. Then verifies the token is single-use: a
/// second `consume` call must fail.
#[test]
fn magic_link_consume_returns_user_and_session() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let email = "magic-consume@example.com";
        let token = Auth::magic_link()
            .send(email, "http://localhost:8000/auth/magic")
            .await
            .unwrap();

        assert!(!token.is_empty());

        let (user, session) = Auth::magic_link().consume(&token).await.unwrap();

        assert_eq!(user.email, email);
        assert_eq!(session.user_id, user.id);

        // Token must be single-use: second consume must fail.
        let second = Auth::magic_link().consume(&token).await;
        assert!(second.is_err(), "magic-link token should be single-use");
    });
}

/// FIX 2: Passkey in-flight state is stored in the session, not a process-local DashMap.
///
/// Calls `begin_registration` inside a session scope and asserts the session
/// contains the `passkey_reg` key with non-trivial JSON bytes. This proves
/// state is written to the session and would survive a process restart or
/// work across multiple replicas (each using the same session store).
#[test]
fn passkey_registration_state_stored_in_session() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let slot = suprnova::session::new_session_slot_for_test();

        let session_has_reg_key = suprnova::session::session_scope_for_test(slot, async {
            let _challenge = Auth::passkey()
                .begin_registration("session-state@example.com")
                .await
                .expect("begin_registration should succeed");

            // The session must now contain the passkey_reg key.
            suprnova::session::session()
                .and_then(|s| s.get::<String>("passkey_reg"))
                .map(|v| !v.is_empty())
                .unwrap_or(false)
        })
        .await;

        assert!(
            session_has_reg_key,
            "begin_registration must store in-flight state in the session under 'passkey_reg'"
        );
    });
}

/// FIX 3: `begin_registration` does not create a password row for the user.
///
/// Before the fix, `get_or_create_user_by_email` called `password().register(email, random_uuid)`,
/// which set `password_hash` to a bcrypt-hashed random UUID in the users table.
/// After the fix, user creation goes through `find_or_create_by_email` (the repository
/// layer directly), which creates the users row but leaves `password_hash = NULL`.
///
/// # Discriminator
///
/// We read the raw `password_hash` column after `begin_registration` and assert it is
/// `None`.  Pre-fix code sets a non-null hash (random UUID, bcrypt-hashed); post-fix
/// code leaves the hash null.  Using `password().authenticate()` does NOT discriminate
/// between the two paths (both return `Err` — for different reasons), but a direct hash
/// read does.
#[test]
fn passkey_registration_does_not_create_password_row() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let email = "no-password-row@example.com";

        let slot = suprnova::session::new_session_slot_for_test();
        suprnova::session::session_scope_for_test(slot, async {
            // Creates the user via find_or_create_by_email — no password hash set.
            Auth::passkey()
                .begin_registration(email)
                .await
                .expect("begin_registration should succeed");
        })
        .await;

        // Read the raw password hash from the database.
        // Post-fix: hash is None (password_hash column is NULL).
        // Pre-fix: hash is Some(<bcrypt of random uuid>) — password().register() was called.
        let hash = suprnova::torii_integration::password_hash_for_email_test_only(email)
            .await
            .expect("password_hash_for_email_test_only should not error");

        assert!(
            hash.is_none(),
            "passkey registration must not create a password hash; \
             found hash={hash:?} — indicates the old password().register() path is still running"
        );
    });
}

/// OAuth kickoff returns a valid GitHub authorization URL and a non-empty state token.
#[test]
fn oauth_kickoff_returns_authorization_url() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        Auth::oauth("github").configure(suprnova::torii_integration::oauth::OAuthProviderConfig {
            client_id: "test-client".into(),
            client_secret: "test-secret".into(),
            redirect_url: "http://localhost:8000/auth/oauth/github/callback".into(),
            scopes: vec!["user:email".into()],
        });

        let slot = suprnova::session::new_session_slot_for_test();
        let (url, state) = suprnova::session::session_scope_for_test(slot, async {
            let kickoff = Auth::oauth("github").begin().await.unwrap();
            (kickoff.authorization_url, kickoff.state)
        })
        .await;

        assert!(
            url.starts_with("https://github.com/login/oauth"),
            "expected GitHub OAuth URL, got: {url}",
        );
        assert!(!state.is_empty());
    });
}

/// FIX 1a: OAuth CSRF — complete() rejects when the calling session never
/// stored any state (attacker tricks a victim with no in-progress OAuth flow).
///
/// Pre-fix: state was stored in torii's global pkce_verifier store, so any
/// valid state from ANY session in the world would pass validation. Post-fix:
/// state is bound to the session that called begin(); a session with no
/// `oauth_state_<provider>` key rejects any presented state.
#[test]
fn oauth_complete_rejects_when_session_has_no_stored_state() {
    Lazy::force(&SETUP);

    Auth::oauth("github").configure(suprnova::torii_integration::oauth::OAuthProviderConfig {
        client_id: "test-client".into(),
        client_secret: "test-secret".into(),
        redirect_url: "http://localhost:8000/auth/oauth/github/callback".into(),
        scopes: vec!["user:email".into()],
    });

    RT.block_on(async {
        // Session B: initiate a flow so a state token exists in B's session.
        // (Pre-fix this also wrote to torii's global store, which is what the
        // attacker would have replayed.)
        let slot_b = suprnova::session::new_session_slot_for_test();
        let state_b = suprnova::session::session_scope_for_test(slot_b, async {
            Auth::oauth("github").begin().await.unwrap().state
        })
        .await;

        // Victim's session: never called begin(). Attempting complete() with
        // any state (including state_b stolen from session B) must fail —
        // there's no `oauth_state_github` key in this session.
        let victim_slot = suprnova::session::new_session_slot_for_test();
        let result = suprnova::session::session_scope_for_test(victim_slot, async {
            Auth::oauth("github").complete("fake-code", &state_b).await
        })
        .await;

        assert!(result.is_err(), "complete() in a session with no stored state must fail");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("missing"),
            "expected 'missing' error (no oauth_state_github in session), got: {err_msg}",
        );
    });
}

/// FIX 1b: OAuth CSRF — complete() rejects when the session DOES have a stored
/// state but the presented state value doesn't match it.
///
/// This exercises the `Some(expected) if expected != state` branch — the
/// classic state-mismatch CSRF defence. Combined with the "no stored state"
/// test above, both arms of the session check are covered.
#[test]
fn oauth_complete_rejects_when_state_doesnt_match_session_stored() {
    Lazy::force(&SETUP);

    Auth::oauth("github").configure(suprnova::torii_integration::oauth::OAuthProviderConfig {
        client_id: "test-client".into(),
        client_secret: "test-secret".into(),
        redirect_url: "http://localhost:8000/auth/oauth/github/callback".into(),
        scopes: vec!["user:email".into()],
    });

    RT.block_on(async {
        // Both begin() and complete() in the SAME session — but complete()
        // receives a state value that does NOT match what begin() stored.
        let slot = suprnova::session::new_session_slot_for_test();
        let result = suprnova::session::session_scope_for_test(slot, async {
            let state_a = Auth::oauth("github").begin().await.unwrap().state;
            assert!(!state_a.is_empty(), "begin() must produce a non-empty state");

            // Forge a different state — must not match the stored state_a.
            Auth::oauth("github")
                .complete("fake-code", "attacker-controlled-state-value")
                .await
        })
        .await;

        assert!(result.is_err(), "complete() must reject state that doesn't match the stored value");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("mismatch"),
            "expected 'mismatch' error (state doesn't match session-stored value), got: {err_msg}",
        );
    });
}

// ── BearerTokenMiddleware tests ───────────────────────────────────────────────

/// Creates a real `suprnova::Request` with an optional `Authorization` header
/// by spinning up a minimal in-memory HTTP/1.1 connection.
///
/// `suprnova::Request` wraps `hyper::Request<hyper::body::Incoming>`, and
/// `Incoming` can only be produced by hyper's connection machinery. We use a
/// `tokio::io::duplex` pipe + `hyper::server::conn::http1` to parse a raw
/// HTTP request, giving us a genuine `Incoming` body without a network socket.
async fn build_request_async(auth_header: Option<&str>) -> suprnova::Request {
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use std::convert::Infallible;
    use tokio::sync::oneshot;

    let (req_tx, req_rx) = oneshot::channel::<suprnova::Request>();

    // Build the raw HTTP bytes to send over the wire.
    let auth_line = auth_header
        .map(|h| format!("Authorization: {}\r\n", h))
        .unwrap_or_default();
    let http_bytes = format!(
        "GET /api/test HTTP/1.1\r\nHost: localhost\r\n{}Content-Length: 0\r\n\r\n",
        auth_line
    );

    let (client_io, server_io) = tokio::io::duplex(4096);

    // Server side: accept one request, send it through the oneshot channel.
    // `service_fn` requires `Fn`, so we use a `Mutex<Option<_>>` to move
    // the sender out on the first (and only) call.
    let req_tx = std::sync::Mutex::new(Some(req_tx));
    tokio::spawn(async move {
        let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
            let wrapped = suprnova::Request::new(req);
            if let Ok(mut guard) = req_tx.lock() {
                if let Some(tx) = guard.take() {
                    let _ = tx.send(wrapped);
                }
            }
            async {
                Ok::<_, Infallible>(hyper::Response::new(
                    http_body_util::Empty::<bytes::Bytes>::new(),
                ))
            }
        });

        let _ = http1::Builder::new()
            .serve_connection(hyper_util::rt::TokioIo::new(server_io), svc)
            .await;
    });

    // Client side: write the raw request bytes, then drop (signals EOF).
    {
        use tokio::io::AsyncWriteExt;
        let mut client = client_io;
        client.write_all(http_bytes.as_bytes()).await.unwrap();
    }

    req_rx.await.expect("server should have received the request")
}

/// `BearerTokenMiddleware` binds the session when a valid token is presented.
///
/// Registers a user, authenticates (obtaining a real session token), then
/// drives the middleware with that token in the `Authorization` header.
/// Asserts that `Auth::check()` returns `true` inside the session scope.
#[test]
fn bearer_token_middleware_binds_session_when_token_valid() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        // Register + authenticate to get a real session token.
        Auth::password()
            .register("bearer-valid@example.com", "Bearer1!")
            .await
            .unwrap();

        let (_user, torii_session) = Auth::password()
            .authenticate("bearer-valid@example.com", "Bearer1!", None, None)
            .await
            .unwrap();

        let token_str = torii_session.token.to_string();
        assert!(!token_str.is_empty());

        // Build a fake request with the bearer token.
        let request = build_request_async(Some(&format!("Bearer {}", token_str))).await;

        // Set up a per-request session scope (as SessionMiddleware would do at runtime).
        let slot = suprnova::session::new_session_slot_for_test();

        let authenticated = suprnova::session::session_scope_for_test(slot, async {
            // Stub `next` that just returns OK without touching the session.
            let next: suprnova::Next = Arc::new(|_req| {
                Box::pin(async { Ok(suprnova::HttpResponse::text("ok")) })
            });

            let mw = BearerTokenMiddleware;
            use suprnova::Middleware;
            let _response = mw.handle(request, next).await;

            // After middleware runs, `Auth::check()` must return true because
            // `set_auth_user` was called with the raw torii UserId string.
            Auth::check()
        })
        .await;

        assert!(
            authenticated,
            "BearerTokenMiddleware should bind the session for a valid token"
        );
    });
}

/// `BearerTokenMiddleware` stores the raw torii `UserId` string, not a hash.
///
/// Registers a user, authenticates, drives the middleware, then asserts that
/// `Auth::id()` returns the raw `"usr_…"` prefixed string — proving that the
/// old FNV-1a hashing punt has been removed.
#[test]
fn bearer_middleware_stores_raw_user_id_not_hash() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        Auth::password()
            .register("raw-uid@example.com", "RawUid1!")
            .await
            .unwrap();

        let (_user, torii_session) = Auth::password()
            .authenticate("raw-uid@example.com", "RawUid1!", None, None)
            .await
            .unwrap();

        let token_str = torii_session.token.to_string();

        let request = build_request_async(Some(&format!("Bearer {}", token_str))).await;
        let slot = suprnova::session::new_session_slot_for_test();

        let session_uid = suprnova::session::session_scope_for_test(slot, async {
            let next: suprnova::Next = Arc::new(|_req| {
                Box::pin(async { Ok(suprnova::HttpResponse::text("ok")) })
            });

            let mw = BearerTokenMiddleware;
            use suprnova::Middleware;
            let _response = mw.handle(request, next).await;

            Auth::id()
        })
        .await;

        let session_uid = session_uid.expect("Auth::id() should be Some after bearer middleware");
        assert!(
            session_uid.starts_with("usr_"),
            "expected raw torii UserId (starts with 'usr_'), got: {session_uid}"
        );
    });
}

/// `BearerTokenMiddleware` passes through without binding when the token is invalid.
///
/// Drives the middleware with a garbage token; asserts that `Auth::check()`
/// returns `false` (no session was bound) and the request reached the handler
/// (response is `Ok`, not `401`).
#[test]
fn bearer_token_middleware_passes_through_when_token_invalid() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let request = build_request_async(Some("Bearer garbage_invalid_token_xyz")).await;

        let slot = suprnova::session::new_session_slot_for_test();

        let (authenticated, response_ok) =
            suprnova::session::session_scope_for_test(slot, async {
                let next: suprnova::Next = Arc::new(|_req| {
                    Box::pin(async { Ok(suprnova::HttpResponse::text("ok")) })
                });

                let mw = BearerTokenMiddleware;
                use suprnova::Middleware;
                let response = mw.handle(request, next).await;

                (Auth::check(), response.is_ok())
            })
            .await;

        assert!(
            !authenticated,
            "BearerTokenMiddleware should NOT bind the session for an invalid token"
        );
        assert!(
            response_ok,
            "BearerTokenMiddleware should pass through (no 401) for an invalid token"
        );
    });
}
