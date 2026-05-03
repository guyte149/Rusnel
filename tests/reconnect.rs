//! Client reconnect-with-exponential-backoff tests.
//!
//! Asserts that the client survives both:
//!   * a server restart mid-session (connection dropped after handshake), and
//!   * an initial-connect failure (server not yet up when the client starts).
//!
//! The test approach mirrors how chisel's reconnect is exercised: spawn the
//! server, observe data flowing, abort the server, restart it on the same
//! port, and verify the client transparently re-establishes the tunnel
//! without the caller restarting the local listener.

mod common;

use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::{client_config, get_available_port, init_crypto, STARTUP_DELAY, TEST_TIMEOUT};
use quinn::{Connection, VarInt};
use rusnel::common::quic::{create_server_endpoint, Congestion};
use rusnel::common::remote::RemoteRequest;
use rusnel::common::tls::ServerTlsConfig;
use rusnel::ReconnectConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::timeout;
use tracing::{error, info};

/// Test-local server runner that supports a *graceful* shutdown via the
/// returned [`oneshot::Sender`]. We cannot use `JoinHandle::abort()` for the
/// "crash and restart" scenario because aborting leaves quinn's endpoint
/// driver task holding the UDP socket — preventing the next server instance
/// from binding the same port.
struct ServerHandle {
    join: tokio::task::JoinHandle<()>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl ServerHandle {
    /// Trigger graceful shutdown and wait for the endpoint to fully close
    /// (releasing the UDP socket).
    async fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

/// Spawn a server that closes its endpoint cleanly when signalled, so the
/// loopback UDP port becomes immediately rebindable for the next instance.
async fn spawn_server(server_port: u16) -> ServerHandle {
    init_crypto();
    let (tx, mut rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        let endpoint = match create_server_endpoint(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            server_port,
            &ServerTlsConfig::Insecure,
            Congestion::Cubic,
        ) {
            Ok(e) => e,
            Err(e) => {
                error!("test server failed to bind: {e}");
                return;
            }
        };

        let session_counter = AtomicUsize::new(0);
        loop {
            tokio::select! {
                _ = &mut rx => break,
                maybe_conn = endpoint.accept() => {
                    let Some(conn) = maybe_conn else { break };
                    let _id = session_counter.fetch_add(1, Ordering::Relaxed);
                    tokio::spawn(async move {
                        let conn: Connection = match conn.await {
                            Ok(c) => c,
                            Err(e) => { info!("incoming failed: {e}"); return; }
                        };
                        // Run the same per-connection accept loop the real
                        // server uses by handing each bi-stream off to the
                        // public tunnel handler. We only need the data plane
                        // to work end-to-end; using the library's own server
                        // mod would require exposing more internals.
                        loop {
                            let stream = conn.accept_bi().await;
                            let (send, recv) = match stream {
                                Ok(s) => s,
                                Err(_) => return,
                            };
                            tokio::spawn(handle_test_stream(conn.clone(), send, recv));
                        }
                    });
                }
            }
        }
        // Graceful close: ask peers to terminate, then wait for quinn's
        // driver to flush and release the UDP socket.
        endpoint.close(VarInt::from_u32(0), b"test shutdown");
        endpoint.wait_idle().await;
    });
    tokio::time::sleep(STARTUP_DELAY).await;
    ServerHandle {
        join,
        shutdown: Some(tx),
    }
}

/// Minimal server-side stream handler that re-implements the forward-TCP path
/// from `server::handle_remote_stream`. Using only public APIs keeps this test
/// independent of internal layout changes.
async fn handle_test_stream(
    _conn: Connection,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
) {
    use rusnel::common::tcp::tunnel_tcp_server;
    use rusnel::common::tunnel::server_receive_remote_request;
    let request = match server_receive_remote_request(&mut send, &mut recv, false, false).await {
        Ok(r) => r,
        Err(e) => {
            info!("control handshake failed: {e}");
            return;
        }
    };
    if let Err(e) = tunnel_tcp_server(recv, send, request).await {
        info!("tunnel ended: {e}");
    }
}

/// Reconnect config tuned for tests: tight backoff so the test finishes fast
/// while still exercising the same code paths the production default uses.
fn fast_reconnect() -> ReconnectConfig {
    ReconnectConfig {
        max_retries: None,
        initial_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_millis(500),
    }
}

/// End-to-end: open a TCP connection through a forward tunnel, send a payload,
/// receive it on the other side. Verifies the data plane on a freshly
/// established (or re-established) session.
async fn assert_tunnel_works(local_port: u16, target_listener: &TcpListener) {
    let mut client_conn = TcpStream::connect(format!("127.0.0.1:{local_port}"))
        .await
        .unwrap();

    let (mut target_stream, _) = target_listener.accept().await.unwrap();

    let payload = b"hello after reconnect";
    client_conn.write_all(payload).await.unwrap();
    client_conn.shutdown().await.unwrap();

    let mut buf = vec![0u8; 1024];
    let mut total = 0;
    while let Ok(n) = target_stream.read(&mut buf[total..]).await {
        if n == 0 {
            break;
        }
        total += n;
        if total >= payload.len() {
            break;
        }
    }
    assert_eq!(&buf[..total], payload);
}

/// Server crash → client reconnects → tunnel works again on the same local
/// port. The client is *never* restarted: its forward listener must come back
/// after the new server-side tunnel is established.
#[tokio::test]
async fn test_client_reconnect_after_server_restart() {
    // Detecting the server crash can take up to the QUIC idle timeout
    // (~30 s by default) after the server-side UDP socket goes away, so the
    // overall test budget needs to comfortably cover that plus reconnect.
    timeout(Duration::from_secs(90), async {
        init_crypto();
        let server_port = get_available_port();
        let local_port = get_available_port();
        let remote_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{remote_port}"))
            .await
            .unwrap();

        let server_handle = spawn_server(server_port).await;

        let mut cc = client_config(
            server_port,
            vec![RemoteRequest::from_str(&format!(
                "127.0.0.1:{local_port}:127.0.0.1:{remote_port}"
            ))
            .unwrap()],
        );
        cc.reconnect = fast_reconnect();
        let client_handle = tokio::spawn(async move {
            let _ = rusnel::client::run_async(cc).await;
        });

        // Wait for the initial tunnel to come up and exercise it.
        tokio::time::sleep(STARTUP_DELAY).await;
        assert_tunnel_works(local_port, &target_listener).await;

        // Gracefully stop the server: closes the QUIC endpoint with a CONNECTION_CLOSE
        // frame so the client immediately sees the disconnect (no waiting for
        // the idle timeout) and releases the UDP port for the next instance.
        server_handle.stop().await;

        // Restart the server on the same UDP port. The graceful close above
        // means the bind should succeed on the first try.
        let _restarted = spawn_server(server_port).await;

        // Give the client time to notice the drop, back off, and reconnect.
        // The session needs to be fully re-established before the local
        // listener will hand a fresh accept off to the new QUIC connection.
        let mut last_err = None;
        let mut succeeded = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            // Try to push data through the re-established tunnel. We retry
            // because the client's reconnect is asynchronous from the test.
            let attempt = timeout(Duration::from_secs(2), async {
                let mut conn = TcpStream::connect(format!("127.0.0.1:{local_port}")).await?;
                let (mut target, _) = target_listener.accept().await?;
                let payload = b"hello after reconnect";
                conn.write_all(payload).await?;
                conn.shutdown().await?;
                let mut buf = vec![0u8; 64];
                let mut total = 0;
                while let Ok(n) = target.read(&mut buf[total..]).await {
                    if n == 0 {
                        break;
                    }
                    total += n;
                    if total >= payload.len() {
                        break;
                    }
                }
                if &buf[..total] == payload {
                    Ok::<(), anyhow::Error>(())
                } else {
                    Err(anyhow::anyhow!("payload mismatch: got {:?}", &buf[..total]))
                }
            })
            .await;
            match attempt {
                Ok(Ok(())) => {
                    succeeded = true;
                    break;
                }
                Ok(Err(e)) => last_err = Some(format!("{e}")),
                Err(_) => last_err = Some("inner timeout".to_string()),
            }
        }
        assert!(
            succeeded,
            "tunnel never recovered after server restart (last error: {last_err:?})"
        );

        client_handle.abort();
    })
    .await
    .expect("test_client_reconnect_after_server_restart timed out");
}

/// Client started before the server: the first `endpoint.connect` will fail.
/// The client must back off and retry until the server appears, then run
/// normally. This is the chisel `--server-not-yet-up-on-startup` scenario.
#[tokio::test]
async fn test_client_reconnect_before_server_up() {
    timeout(TEST_TIMEOUT, async {
        init_crypto();
        let server_port = get_available_port();
        let local_port = get_available_port();
        let remote_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{remote_port}"))
            .await
            .unwrap();

        let mut cc = client_config(
            server_port,
            vec![RemoteRequest::from_str(&format!(
                "127.0.0.1:{local_port}:127.0.0.1:{remote_port}"
            ))
            .unwrap()],
        );
        cc.reconnect = fast_reconnect();
        let client_handle = tokio::spawn(async move {
            let _ = rusnel::client::run_async(cc).await;
        });

        // Let the client cycle through a couple of failed connect attempts.
        tokio::time::sleep(Duration::from_millis(300)).await;

        let _server_handle = spawn_server(server_port).await;

        // Wait for the tunnel to finally come up. Retry the dial because the
        // client's listener only binds *after* the first successful session
        // hand-off.
        let mut succeeded = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let attempt = timeout(Duration::from_secs(2), async {
                let mut conn = TcpStream::connect(format!("127.0.0.1:{local_port}")).await?;
                let (mut target, _) = target_listener.accept().await?;
                let payload = b"hello after late server start";
                conn.write_all(payload).await?;
                conn.shutdown().await?;
                let mut buf = vec![0u8; 64];
                let mut total = 0;
                while let Ok(n) = target.read(&mut buf[total..]).await {
                    if n == 0 {
                        break;
                    }
                    total += n;
                    if total >= payload.len() {
                        break;
                    }
                }
                anyhow::ensure!(&buf[..total] == payload, "payload mismatch");
                Ok::<(), anyhow::Error>(())
            })
            .await;
            if matches!(attempt, Ok(Ok(()))) {
                succeeded = true;
                break;
            }
        }
        assert!(
            succeeded,
            "tunnel never came up after server eventually started"
        );
        client_handle.abort();
    })
    .await
    .expect("test_client_reconnect_before_server_up timed out");
}
