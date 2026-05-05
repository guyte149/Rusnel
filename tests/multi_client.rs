//! Multiple clients sharing a single server.
//!
//! All other tests spawn exactly one rusnel client, so the server's
//! per-session handler is only ever exercised against a single live
//! connection. This file pins down the multi-client invariants:
//!
//!   * Two clients with disjoint forward tunnels both work concurrently.
//!   * One client disconnecting (graceful or abrupt) does not perturb
//!     the other client's in-flight or future traffic.

mod common;

use std::str::FromStr;
use std::time::Duration;

use common::{
    client_config, get_available_port, init_crypto, server_config, STARTUP_DELAY, TEST_TIMEOUT,
};
use rusnel::common::remote::RemoteRequest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

/// Spawn a server task and return a handle that aborts it on drop.
struct ServerGuard(tokio::task::JoinHandle<()>);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct ClientGuard(tokio::task::JoinHandle<()>);
impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn spawn_server(server_port: u16) -> ServerGuard {
    init_crypto();
    let sc = server_config(server_port, false);
    let h = tokio::spawn(async move {
        let _ = rusnel::server::run_async(sc).await;
    });
    tokio::time::sleep(STARTUP_DELAY).await;
    ServerGuard(h)
}

async fn spawn_client(server_port: u16, remotes: Vec<RemoteRequest>) -> ClientGuard {
    let cc = client_config(server_port, remotes);
    let h = tokio::spawn(async move {
        let _ = rusnel::client::run_async(cc).await;
    });
    tokio::time::sleep(STARTUP_DELAY).await;
    ClientGuard(h)
}

/// Two clients, each owning its own forward TCP tunnel through the same
/// server, both work in parallel — disjoint sessions, no port collisions,
/// both round-trips succeed.
#[tokio::test]
async fn test_two_clients_concurrent_traffic() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let local_a = get_available_port();
        let target_a = get_available_port();
        let local_b = get_available_port();
        let target_b = get_available_port();

        let listener_a = TcpListener::bind(format!("127.0.0.1:{target_a}"))
            .await
            .unwrap();
        let listener_b = TcpListener::bind(format!("127.0.0.1:{target_b}"))
            .await
            .unwrap();

        let _server = spawn_server(server_port).await;
        let _ca = spawn_client(
            server_port,
            vec![
                RemoteRequest::from_str(&format!("127.0.0.1:{local_a}:127.0.0.1:{target_a}"))
                    .unwrap(),
            ],
        )
        .await;
        let _cb = spawn_client(
            server_port,
            vec![
                RemoteRequest::from_str(&format!("127.0.0.1:{local_b}:127.0.0.1:{target_b}"))
                    .unwrap(),
            ],
        )
        .await;

        let (mut app_a, mut app_b) = tokio::join!(
            async {
                TcpStream::connect(format!("127.0.0.1:{local_a}"))
                    .await
                    .unwrap()
            },
            async {
                TcpStream::connect(format!("127.0.0.1:{local_b}"))
                    .await
                    .unwrap()
            },
        );
        app_a.write_all(b"AAA").await.unwrap();
        app_b.write_all(b"BBB").await.unwrap();
        app_a.shutdown().await.unwrap();
        app_b.shutdown().await.unwrap();

        let (mut srv_a, _) = listener_a.accept().await.unwrap();
        let (mut srv_b, _) = listener_b.accept().await.unwrap();
        let mut got_a = Vec::new();
        let mut got_b = Vec::new();
        srv_a.read_to_end(&mut got_a).await.unwrap();
        srv_b.read_to_end(&mut got_b).await.unwrap();

        assert_eq!(got_a, b"AAA");
        assert_eq!(got_b, b"BBB");
    })
    .await
    .expect("test_two_clients_concurrent_traffic timed out");
}

/// Aborting client A's task must not knock client B's tunnels offline.
/// This is the regression test for any state shared across sessions on
/// the server (a single accept loop bug, a global JoinSet, a port
/// registry, etc.) that would tear down B when A's handler unwinds.
#[tokio::test]
async fn test_one_client_disconnect_doesnt_affect_other() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let local_a = get_available_port();
        let target_a = get_available_port();
        let local_b = get_available_port();
        let target_b = get_available_port();

        let _listener_a = TcpListener::bind(format!("127.0.0.1:{target_a}"))
            .await
            .unwrap();
        let listener_b = TcpListener::bind(format!("127.0.0.1:{target_b}"))
            .await
            .unwrap();

        let _server = spawn_server(server_port).await;
        let client_a = spawn_client(
            server_port,
            vec![
                RemoteRequest::from_str(&format!("127.0.0.1:{local_a}:127.0.0.1:{target_a}"))
                    .unwrap(),
            ],
        )
        .await;
        let _client_b = spawn_client(
            server_port,
            vec![
                RemoteRequest::from_str(&format!("127.0.0.1:{local_b}:127.0.0.1:{target_b}"))
                    .unwrap(),
            ],
        )
        .await;

        // Verify B works before tearing down A.
        {
            let mut app = TcpStream::connect(format!("127.0.0.1:{local_b}"))
                .await
                .unwrap();
            app.write_all(b"pre").await.unwrap();
            app.shutdown().await.unwrap();
            let (mut s, _) = listener_b.accept().await.unwrap();
            let mut got = Vec::new();
            s.read_to_end(&mut got).await.unwrap();
            assert_eq!(got, b"pre");
        }

        // Abort client A. The server-side handler for A's session should
        // unwind, tear down only A's tunnels, and leave B's session
        // running.
        client_a.0.abort();
        tokio::time::sleep(Duration::from_millis(500)).await;

        // B must still be live. Use a fresh connection through B's
        // tunnel — if the server collapsed B's session, this connect
        // will succeed locally (the client-side listener is still up)
        // but no bytes will reach the target.
        let mut app = TcpStream::connect(format!("127.0.0.1:{local_b}"))
            .await
            .unwrap();
        app.write_all(b"post").await.unwrap();
        app.shutdown().await.unwrap();
        let (mut s, _) = timeout(Duration::from_secs(5), listener_b.accept())
            .await
            .expect("client B's tunnel went dark after client A disconnected")
            .unwrap();
        let mut got = Vec::new();
        s.read_to_end(&mut got).await.unwrap();
        assert_eq!(
            got, b"post",
            "client B traffic must survive client A disconnect"
        );
    })
    .await
    .expect("test_one_client_disconnect_doesnt_affect_other timed out");
}
