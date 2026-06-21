//! Shared helpers for HTTP-based mail providers (Postmark, SES, SendGrid, …).

use crate::error::FrameworkError;
use reqwest::Client;
use std::sync::OnceLock;
use std::time::Duration;

/// Per-request total timeout for HTTP mail providers. Matches the
/// `suprnova-web-push` `DEFAULT_REQUEST_TIMEOUT` so the entire framework
/// uses one upper bound on outbound provider calls.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Connect-only timeout for HTTP mail providers. A separate, shorter
/// budget so a black-holed TLS handshake fails fast rather than burning
/// the entire request budget.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum number of error-response body bytes any HTTP mail provider
/// will buffer before dropping the rest. The endpoint is operator-
/// overridable (`MAIL_<PROVIDER>_ENDPOINT`), so the peer is not strictly
/// trusted; capping the diagnostic snippet stops a hostile or
/// misconfigured server from forcing an unbounded read into RAM. Matches
/// the `suprnova-web-push` client's 8 KiB cap.
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;

/// One shared `reqwest::Client` across all HTTP-mail transports.
/// Connection-pooled, rustls, no PII headers. Carries an explicit
/// request + connect timeout so a slow or unresponsive provider cannot
/// hold a `MailTransport::send` await indefinitely.
pub(crate) fn shared_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(concat!("suprnova-mail/", env!("CARGO_PKG_VERSION")))
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .build()
            .expect("reqwest client builder")
    })
}

pub(crate) fn err(provider: &'static str, status: u16, body: String) -> FrameworkError {
    FrameworkError::internal(format!("{provider} HTTP {status}: {body}"))
}

/// Stream and accumulate up to [`MAX_ERROR_BODY_BYTES`] of an error
/// response body, then drop the response so the remainder is not
/// buffered. The returned string is UTF-8-lossy — a provider may emit
/// arbitrary bytes, but the snippet is for diagnostic surfacing only.
///
/// Dropping the response once the cap is reached closes the connection
/// (or returns it to the pool) so a hostile peer can't hold the socket
/// open by dribbling more bytes after we've stopped reading.
pub(crate) async fn read_error_body(resp: reqwest::Response) -> String {
    read_capped_body(resp, MAX_ERROR_BODY_BYTES).await
}

async fn read_capped_body(mut resp: reqwest::Response, cap: usize) -> String {
    let mut buf: Vec<u8> = Vec::new();
    while buf.len() < cap {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let remaining = cap - buf.len();
                let take = remaining.min(chunk.len());
                buf.extend_from_slice(&chunk[..take]);
                if buf.len() >= cap {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    drop(resp);
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An oversized error body is truncated to the cap rather than read in
    /// full. We drive `read_capped_body` against a local server that
    /// streams far more than the cap and assert the buffered snippet is
    /// exactly `cap` bytes long.
    #[tokio::test]
    async fn oversized_error_body_is_truncated_to_cap() {
        use std::io::Write;
        use std::net::TcpListener;

        const CAP: usize = 64;
        // Body twice the cap so a correct reader stops well before EOF.
        let body_len = CAP * 2;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            // Drain the request line/headers so the client's write completes.
            {
                use std::io::Read;
                let mut probe = [0u8; 1024];
                let _ = stream.read(&mut probe);
            }
            let response = format!(
                "HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\nContent-Length: {body_len}\r\nConnection: close\r\n\r\n{}",
                "x".repeat(body_len)
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status().as_u16(), 500);

        let snippet = read_capped_body(resp, CAP).await;
        assert_eq!(
            snippet.len(),
            CAP,
            "body must be truncated to the cap, not read in full"
        );
        assert!(snippet.bytes().all(|b| b == b'x'));

        handle.join().expect("server thread");
    }

    /// A body smaller than the cap is returned whole.
    #[tokio::test]
    async fn undersized_error_body_is_returned_whole() {
        use std::io::Write;
        use std::net::TcpListener;

        const CAP: usize = 8 * 1024;
        let body = "boom";

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            {
                use std::io::Read;
                let mut probe = [0u8; 1024];
                let _ = stream.read(&mut probe);
            }
            let response = format!(
                "HTTP/1.1 422 Unprocessable Entity\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("send");

        let snippet = read_capped_body(resp, CAP).await;
        assert_eq!(snippet, "boom");

        handle.join().expect("server thread");
    }
}
