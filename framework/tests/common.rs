//! Shared test helpers for multipart upload tests.
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
