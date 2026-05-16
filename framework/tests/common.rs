//! Shared test helpers for request-body tests (multipart + generic).
//!
//! `hyper::body::Incoming` is privately constructed in hyper 1.x, so
//! we can't build a synthetic `Request` with a `Full<Bytes>` body
//! directly. We instead parse real HTTP/1.1 bytes through an in-memory
//! `tokio::io::duplex` pipe + `hyper::server::conn::http1::serve_connection`.
//! Same pattern as `framework/tests/torii_integration.rs:build_request_async`.

#![allow(dead_code)]

use bytes::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::sync::Mutex;
use suprnova::Request;
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

/// Build a multipart POST `Request` with the given boundary and body
/// by parsing real HTTP/1.1 bytes through a hyper server over a
/// `tokio::io::duplex` pipe. The resulting `Request` carries a genuine
/// `hyper::body::Incoming` body so streaming parsers work end-to-end
/// without a network socket.
pub async fn request_from_multipart(boundary: &str, body: Bytes) -> Request {
    let (req_tx, req_rx) = oneshot::channel::<Request>();
    let req_tx = Mutex::new(Some(req_tx));

    let content_length = body.len();
    let mut http_bytes = Vec::new();
    http_bytes.extend_from_slice(b"POST /upload HTTP/1.1\r\n");
    http_bytes.extend_from_slice(b"Host: localhost\r\n");
    http_bytes.extend_from_slice(
        format!("Content-Type: multipart/form-data; boundary={boundary}\r\n").as_bytes(),
    );
    http_bytes.extend_from_slice(format!("Content-Length: {content_length}\r\n\r\n").as_bytes());
    http_bytes.extend_from_slice(&body);

    // Duplex buffer must hold the entire request — for oversize-rejection
    // tests (6 MiB body in Task 5) the client writes synchronously before
    // the server can read.
    let (client_io, server_io) = tokio::io::duplex(64 * 1024 + content_length);

    tokio::spawn(async move {
        let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
            let wrapped = Request::new(req);
            if let Ok(mut guard) = req_tx.lock()
                && let Some(tx) = guard.take()
            {
                let _ = tx.send(wrapped);
            }
            // Never resolve. Hyper drives body bytes into `Incoming` only
            // while the service future is pending; if we return Ok here,
            // the connection task stops pumping and subsequent body reads
            // (e.g. via `BodyDataStream` + `multer`) fail with
            // "failed to read stream". The tokio test runtime drops this
            // task at end-of-test, so the unreachable Ok branch just gives
            // the closure its `Result<_, Infallible>` return type.
            async {
                std::future::pending::<()>().await;
                Ok::<_, Infallible>(hyper::Response::new(
                    http_body_util::Empty::<Bytes>::new(),
                ))
            }
        });
        let _ = http1::Builder::new()
            .serve_connection(TokioIo::new(server_io), svc)
            .await;
    });

    {
        let mut client = client_io;
        client.write_all(&http_bytes).await.unwrap();
        // Drop the client to signal EOF after the write completes. The
        // hyper server reads the full body before EOF arrives because
        // `write_all` only returns after the bytes are queued in the
        // duplex buffer (sized `content_length + 64 KiB`).
    }

    req_rx.await.expect("server should have received the request")
}

/// Internal: build a `Request` from a hand-assembled HTTP/1.1 request
/// wire payload. The bytes you pass must be a complete HTTP request
/// (request line, headers, blank line, body). This drives the same
/// duplex-pipe pattern as `request_from_multipart` but lets the caller
/// control every header — needed for the body-cap tests, which want
/// honest, lying, and absent `Content-Length`.
async fn request_from_http_bytes(http_bytes: Vec<u8>) -> Request {
    let (req_tx, req_rx) = oneshot::channel::<Request>();
    let req_tx = Mutex::new(Some(req_tx));

    // Duplex buffer must fit the whole request — the client writes
    // synchronously before the server task gets to read.
    let duplex_cap = http_bytes.len() + 64 * 1024;
    let (client_io, server_io) = tokio::io::duplex(duplex_cap);

    tokio::spawn(async move {
        let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
            let wrapped = Request::new(req);
            if let Ok(mut guard) = req_tx.lock()
                && let Some(tx) = guard.take()
            {
                let _ = tx.send(wrapped);
            }
            // Never resolve — see request_from_multipart for the rationale.
            async {
                std::future::pending::<()>().await;
                Ok::<_, Infallible>(hyper::Response::new(
                    http_body_util::Empty::<Bytes>::new(),
                ))
            }
        });
        let _ = http1::Builder::new()
            .serve_connection(TokioIo::new(server_io), svc)
            .await;
    });

    {
        let mut client = client_io;
        client.write_all(&http_bytes).await.unwrap();
        // Drop the client to signal EOF after the write completes.
    }

    req_rx.await.expect("server should have received the request")
}

/// Build a POST `Request` for the given path with `Content-Type` and a
/// body, declaring an honest `Content-Length`. Use this for the default
/// "client sent a normal request" path.
pub async fn request_with_body(
    path: &str,
    content_type: &str,
    body: &[u8],
) -> Request {
    let mut http_bytes = Vec::new();
    http_bytes.extend_from_slice(format!("POST {path} HTTP/1.1\r\n").as_bytes());
    http_bytes.extend_from_slice(b"Host: localhost\r\n");
    http_bytes.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
    http_bytes.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    http_bytes.extend_from_slice(body);
    request_from_http_bytes(http_bytes).await
}

/// Build a POST `Request` that declares a specific `Content-Length` but
/// carries a smaller body. The body buffer is zero-filled to
/// `actual_body_len` and the header advertises `declared_len`. Used to
/// drive the pre-rejection path: the server should reject on the header
/// alone, without ever reading the body.
///
/// Caller responsibility: `declared_len` must be larger than
/// `actual_body_len` for the pre-check to win the race against the read.
/// In practice the pre-check happens synchronously before any frame is
/// awaited, so even a perfectly-sized body would still be rejected when
/// the header crosses the cap.
pub async fn request_with_declared_length(
    path: &str,
    content_type: &str,
    declared_len: u64,
    actual_body: &[u8],
) -> Request {
    let mut http_bytes = Vec::new();
    http_bytes.extend_from_slice(format!("POST {path} HTTP/1.1\r\n").as_bytes());
    http_bytes.extend_from_slice(b"Host: localhost\r\n");
    http_bytes.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
    http_bytes.extend_from_slice(format!("Content-Length: {declared_len}\r\n\r\n").as_bytes());
    http_bytes.extend_from_slice(actual_body);
    request_from_http_bytes(http_bytes).await
}

/// Build a POST `Request` with `Transfer-Encoding: chunked` and no
/// `Content-Length` header. Each `&[u8]` in `chunks` becomes one HTTP
/// chunk; a terminating empty chunk is appended automatically. Use this
/// to exercise the progressive-cap path when the body has no declared
/// length up front.
pub async fn request_with_chunked_body(
    path: &str,
    content_type: &str,
    chunks: &[&[u8]],
) -> Request {
    let mut http_bytes = Vec::new();
    http_bytes.extend_from_slice(format!("POST {path} HTTP/1.1\r\n").as_bytes());
    http_bytes.extend_from_slice(b"Host: localhost\r\n");
    http_bytes.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
    http_bytes.extend_from_slice(b"Transfer-Encoding: chunked\r\n\r\n");
    for chunk in chunks {
        // Chunk header: hex size + CRLF, then bytes, then CRLF.
        http_bytes.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        http_bytes.extend_from_slice(chunk);
        http_bytes.extend_from_slice(b"\r\n");
    }
    // Final zero-length chunk.
    http_bytes.extend_from_slice(b"0\r\n\r\n");
    request_from_http_bytes(http_bytes).await
}

/// Build a multipart body from `(name, optional_filename, bytes)` tuples.
///
/// Parts with `Some(filename)` are emitted as file parts (with a
/// `Content-Type: application/octet-stream` header by default). Parts
/// with `None` are emitted as text fields.
pub fn build_multipart_body(boundary: &str, fields: &[(&str, Option<&str>, &[u8])]) -> Bytes {
    let mut body = Vec::new();
    for (name, file_name, bytes) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match file_name {
            Some(fname) => body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{fname}\"\r\n\
                     Content-Type: application/octet-stream\r\n\r\n"
                )
                .as_bytes(),
            ),
            None => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            ),
        }
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Bytes::from(body)
}
