//! Server-side resource cleanup on client disconnect.
//!
//! Regression test for: when a client disconnects, the server must abort any
//! per-tunnel tasks that own local sockets. The most visible case is a
//! *reverse* TCP tunnel (`R:<port>:host:port`) — the server-side runs a
//! `TcpListener` bound to `<port>`, and prior to the fix the listener kept
//! accepting forever against a dead QUIC connection, leaking the port until
//! the server process exited.

mod common;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::time::Duration;

use common::{get_available_port, init_crypto, server_config, STARTUP_DELAY};
use quinn::VarInt;
use rusnel::common::quic::{create_client_endpoint, Congestion};
use rusnel::common::remote::{RemoteRequest, SessionHello};
use rusnel::common::tls::ClientTlsConfig;
use rusnel::common::tunnel::client_send_session_hello;
use tokio::net::TcpListener;
use tokio::time::timeout;

/// Try to bind `127.0.0.1:port`. Returns `true` if the port is currently
/// free (rebindable) and `false` if something else still holds it.
async fn port_is_free(port: u16) -> bool {
    TcpListener::bind(format!("127.0.0.1:{port}")).await.is_ok()
}

/// Poll until the predicate is satisfied or the deadline expires.
async fn wait_until<F, Fut>(deadline: Duration, mut probe: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = std::time::Instant::now();
    while start.elapsed() < deadline {
        if probe().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Reverse TCP tunnel cleanup: the server binds the listener on
/// `reverse_listen_port`. After the client gracefully closes its QUIC
/// connection, the server's `handle_client_connection` must abort the
/// listener task (via the JoinSet shutdown) so the port becomes immediately
/// rebindable. Without the fix, the listener task would loop forever and the
/// port would stay bound until the server process exited.
///
/// We drive the client side by hand instead of going through
/// `client::run_async`, because:
///   * `run_async`'s only graceful shutdown lever is `signal::ctrl_c`, which
///     is not safe to fire from a test (it would affect every other test in
///     the same process).
///   * `JoinHandle::abort()` on a `run_async` task doesn't actually stop
///     quinn's endpoint driver: it just drops the user-facing handle, while
///     the driver task keeps the socket and the connection alive. The server
///     would then sit through a 30 s idle timeout instead of seeing the
///     close, and the test would have to wait that out for no reason.
#[tokio::test]
async fn test_server_releases_reverse_listener_on_client_disconnect() {
    timeout(Duration::from_secs(30), async {
        init_crypto();
        let server_port = get_available_port();
        let reverse_listen_port = get_available_port();
        let reverse_target_port = get_available_port();

        // Real server with reverse tunnels enabled — this is the code path
        // we're regression-testing.
        let sc = server_config(server_port, true);
        let server_handle = tokio::spawn(async move {
            let _ = rusnel::server::run_async(sc).await;
        });
        tokio::time::sleep(STARTUP_DELAY).await;

        let server_addr: SocketAddr = (IpAddr::V4(Ipv4Addr::LOCALHOST), server_port).into();
        let endpoint =
            create_client_endpoint(&ClientTlsConfig::Insecure, Congestion::Cubic, server_addr)
                .unwrap();
        let connection = endpoint
            .connect(server_addr, "127.0.0.1")
            .unwrap()
            .await
            .unwrap();

        // Reverse spec — the server binds reverse_listen_port and forwards
        // accepted sockets back to the client over a dynamic stream. We
        // never push traffic; we only care that the listener exists, and
        // then goes away.
        let remote = RemoteRequest::from_str(&format!(
            "R:127.0.0.1:{reverse_listen_port}:127.0.0.1:{reverse_target_port}"
        ))
        .unwrap();
        // Send a session hello carrying just this one reverse remote.
        // The post-0.8 wire protocol no longer accepts a per-stream
        // `RemoteRequest`; tunnels are declared up front and the
        // server spawns the reverse listener on receipt of the
        // hello reply.
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let hello = SessionHello {
            remotes: vec![remote],
        };
        let _ = client_send_session_hello(&hello, &mut send, &mut recv)
            .await
            .unwrap();

        let bound = wait_until(Duration::from_secs(10), || async {
            !port_is_free(reverse_listen_port).await
        })
        .await;
        assert!(
            bound,
            "server never bound the reverse listener on port {reverse_listen_port}"
        );

        // Graceful close: the server sees ApplicationClosed and drops out
        // of its accept loop immediately, then the JoinSet shutdown aborts
        // the reverse listener task.
        connection.close(VarInt::from_u32(0), b"test done");
        endpoint.wait_idle().await;

        let freed = wait_until(Duration::from_secs(5), || async {
            port_is_free(reverse_listen_port).await
        })
        .await;
        assert!(
            freed,
            "server never released the reverse listener on port {reverse_listen_port} \
             after client disconnect — JoinSet abort regression"
        );

        server_handle.abort();
    })
    .await
    .expect("test_server_releases_reverse_listener_on_client_disconnect timed out");
}
