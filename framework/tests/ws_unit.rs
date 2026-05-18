//! `WsSocket` send/recv unit tests using an in-memory tungstenite pair.
//!
//! Doesn't touch hyper, the router, or any networking — just exercises
//! the send/recv shape so the Phase 7A integration tests (which need
//! a real upgrade) only have to prove the wiring, not the data path.

use suprnova::ws::WsSocket;
use tokio::io::duplex;
use tokio_tungstenite::{tungstenite::protocol::Role, WebSocketStream};

#[tokio::test]
async fn ws_socket_round_trips_text_messages() {
    let (client_io, server_io) = duplex(64 * 1024);

    let server = tokio::spawn(async move {
        let ws = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let mut socket = WsSocket::from_stream(ws);
        let msg = socket
            .recv_text()
            .await
            .expect("recv ok")
            .expect("not closed");
        socket
            .send_text(format!("echo: {msg}"))
            .await
            .expect("send ok");
        // Hold the socket open long enough for the client to read
        // the reply before the forwarder task exits.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    });

    let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
    use futures_util::{SinkExt, StreamExt};
    client
        .send(tokio_tungstenite::tungstenite::Message::Text("hello".into()))
        .await
        .unwrap();
    let reply = client.next().await.unwrap().unwrap();
    assert_eq!(reply.to_text().unwrap(), "echo: hello");

    server.await.unwrap();
}

#[tokio::test]
async fn ws_socket_close_sends_close_frame() {
    let (client_io, server_io) = duplex(64 * 1024);

    let server = tokio::spawn(async move {
        let ws = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let mut socket = WsSocket::from_stream(ws);
        socket.close(1000, "bye").await.expect("close ok");
    });

    use futures_util::StreamExt;
    let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
    let frame = client.next().await.unwrap().unwrap();
    assert!(matches!(
        frame,
        tokio_tungstenite::tungstenite::Message::Close(_)
    ));

    server.await.unwrap();
}
