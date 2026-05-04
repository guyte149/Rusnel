//! End-to-end test for the read-only admin API.
//!
//! Spins up a real server + client pair on localhost (matching the
//! existing `tests/tunnels.rs` style), shovels bytes through a forward
//! TCP tunnel, and then exercises every `GET /api/v1/...` endpoint over
//! the unix socket to verify the JSON shape, byte counters, and history
//! recording.

mod common;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use rusnel::common::remote::RemoteRequest;
use rusnel::common::tls::{ClientTlsConfig, ServerTlsConfig};
use rusnel::ctl;
use rusnel::{ClientConfig, ReconnectConfig, ServerConfig, ServerEndpoint};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use common::{get_available_port, init_crypto, STARTUP_DELAY};

/// Build a short unix-socket path under `/tmp`. macOS's `sun_path` is
/// only ~104 bytes and `$TMPDIR` is too long, so we route around it.
fn admin_sock_path(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    PathBuf::from(format!(
        "/tmp/rusnel-it-{}-{}-{}.sock",
        label,
        std::process::id(),
        nanos
    ))
}

#[tokio::test]
async fn admin_api_lifecycle() {
    init_crypto();

    let server_port = get_available_port();
    let upstream_port = get_available_port();
    let local_port = get_available_port();
    let socket_path = admin_sock_path("lifecycle");

    // Upstream echo TCP server. Reflects whatever the client writes so we
    // can assert bytes_in/bytes_out independently.
    let upstream_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), upstream_port);
    let upstream_listener = TcpListener::bind(upstream_addr).await.unwrap();
    let upstream_handle = tokio::spawn(async move {
        loop {
            let (mut sock, _) = match upstream_listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = sock.read(&mut buf).await {
                    if n == 0 {
                        return;
                    }
                    if sock.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
            });
        }
    });

    // Bring up the rusnel server with admin enabled.
    let server_config = ServerConfig {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: server_port,
        allow_reverse: true,
        allow_socks: true,
        tls: ServerTlsConfig::Insecure,
        congestion: Default::default(),
        max_connections: None,
        admin_socket: Some(socket_path.clone()),
    };
    let server_handle = tokio::spawn(async move {
        let _ = rusnel::server::run_async(server_config).await;
    });
    tokio::time::sleep(STARTUP_DELAY).await;

    // The socket file should now exist with mode 0600.
    let mode = std::fs::metadata(&socket_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");

    // Bring up the client with one forward TCP tunnel pointing at the
    // upstream echo server.
    let remote_str = format!("127.0.0.1:{local_port}:127.0.0.1:{upstream_port}");
    let remote: RemoteRequest = remote_str.parse().expect("parse remote spec");
    let remote_display = remote.to_string();
    let client_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), server_port);
    let client_config = ClientConfig {
        server: ServerEndpoint {
            addrs: vec![client_addr],
            host: client_addr.ip().to_string(),
        },
        remotes: vec![remote.clone()],
        tls: ClientTlsConfig::Insecure,
        congestion: Default::default(),
        reconnect: ReconnectConfig::default(),
        proxy: None,
    };
    let client_handle = tokio::spawn(async move {
        let _ = rusnel::client::run_async(client_config).await;
    });
    tokio::time::sleep(STARTUP_DELAY).await;

    // /api/v1/server: version + listen addr present.
    let server_info = ctl::get(&socket_path, "/api/v1/server").await.unwrap();
    assert!(server_info["version"].as_str().is_some());
    assert!(server_info["listen_addr"]
        .as_str()
        .unwrap()
        .ends_with(&server_port.to_string()));

    // /api/v1/clients: exactly one entry.
    let clients = await_clients(&socket_path, 1).await;
    let client_id = clients[0]["id"].as_u64().expect("client id is u64");
    assert!(clients[0]["remote"].as_str().unwrap().contains("127.0.0.1"));

    // Forward TCP tunnels register on the server only when the client
    // opens a bi-stream — i.e. when a local TCP connection arrives. So
    // connect first, then exercise the tunnels endpoint.
    let mut conn = TcpStream::connect(("127.0.0.1", local_port)).await.unwrap();
    let payload = b"hello rusnel admin api\n";
    conn.write_all(payload).await.unwrap();
    let mut buf = vec![0u8; payload.len()];
    conn.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, payload);

    // /api/v1/tunnels: one tunnel, spec matches.
    let tunnels = await_tunnels(&socket_path, 1).await;
    assert_eq!(tunnels[0]["client_id"].as_u64().unwrap(), client_id);
    assert_eq!(tunnels[0]["spec"].as_str().unwrap(), remote_display);
    let tunnel_id = tunnels[0]["id"].as_u64().unwrap();

    // Tunnel-vs-conn distinction: a *single* tunnel can carry many
    // conns. Open a second concurrent connection through the same
    // local port and verify the tunnel count stays at 1 while the
    // conn count grows to 2.
    let mut conn2 = TcpStream::connect(("127.0.0.1", local_port)).await.unwrap();
    conn2.write_all(b"second\n").await.unwrap();
    let mut buf2 = vec![0u8; 7];
    conn2.read_exact(&mut buf2).await.unwrap();

    let mut active_conns = 0u64;
    for _ in 0..50 {
        let detail = ctl::get(&socket_path, &format!("/api/v1/tunnels/{tunnel_id}"))
            .await
            .unwrap();
        active_conns = detail["active_conn_count"].as_u64().unwrap_or(0);
        if active_conns >= 2 {
            assert_eq!(
                detail["conns"].as_array().map(|a| a.len()).unwrap_or(0),
                active_conns as usize,
                "embedded conns array should match active_conn_count"
            );
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(
        active_conns >= 2,
        "expected at least 2 active conns on the tunnel, got {active_conns}"
    );
    let tunnel_list_again = ctl::get(&socket_path, "/api/v1/tunnels")
        .await
        .unwrap()
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        tunnel_list_again.len(),
        1,
        "tunnels are deduped by spec — second connection must not add a tunnel"
    );

    // /api/v1/conns: global conn list reflects the tunnel's
    // active conns.
    let conns_global = ctl::get(&socket_path, "/api/v1/conns")
        .await
        .unwrap()
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        conns_global.len() >= 2,
        "global /conns should see >=2 active conns"
    );
    for s in &conns_global {
        assert_eq!(s["tunnel_id"].as_u64().unwrap(), tunnel_id);
        assert_eq!(s["client_id"].as_u64().unwrap(), client_id);
    }

    // /api/v1/clients/:id/conns: scoped query returns the same.
    let client_conns = ctl::get(&socket_path, &format!("/api/v1/clients/{client_id}/conns"))
        .await
        .unwrap()
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(client_conns.len() >= 2);

    // Drop the second connection — its conn should clear, but the
    // tunnel sticks around with cumulative counters and total_conns
    // recording it ever existed.
    drop(conn2);
    let mut total_conns = 0u64;
    for _ in 0..50 {
        let detail = ctl::get(&socket_path, &format!("/api/v1/tunnels/{tunnel_id}"))
            .await
            .unwrap();
        total_conns = detail["total_conns"].as_u64().unwrap_or(0);
        let active = detail["active_conn_count"].as_u64().unwrap_or(0);
        if total_conns >= 2 && active < active_conns {
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(
        total_conns >= 2,
        "tunnel.total_conns should record closed conns; got {total_conns}"
    );

    // Counters update lazily as data flows; poll a few times.
    let mut bin = 0u64;
    let mut bout = 0u64;
    for _ in 0..50 {
        let tunnels = ctl::get(&socket_path, "/api/v1/tunnels")
            .await
            .unwrap()
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(t) = tunnels.first() {
            bin = t["bytes_in"].as_u64().unwrap_or(0);
            bout = t["bytes_out"].as_u64().unwrap_or(0);
            if bin > 0 && bout > 0 {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(bin > 0, "expected bytes_in > 0, got {bin}");
    assert!(bout > 0, "expected bytes_out > 0, got {bout}");

    // /api/v1/clients/:id detail returns the same tunnel embedded.
    let detail = ctl::get(&socket_path, &format!("/api/v1/clients/{client_id}"))
        .await
        .unwrap();
    let detail_tunnels = detail["tunnels"].as_array().unwrap();
    assert_eq!(detail_tunnels.len(), 1);
    assert_eq!(detail_tunnels[0]["spec"].as_str().unwrap(), remote_display);

    // Unknown client → 404.
    let err = ctl::get(&socket_path, "/api/v1/clients/999999")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("404"), "expected 404 in error, got: {err}");

    // /api/v1/history initially returns an empty array (we haven't
    // disconnected anyone yet — the disconnect → history-push path is
    // covered by a unit test in `src/server/state.rs` because the QUIC
    // idle-timeout-driven disconnect detection is too slow to exercise
    // reliably in an end-to-end test).
    let history = ctl::get(&socket_path, "/api/v1/history").await.unwrap();
    assert!(history.is_array(), "history should be a JSON array");

    drop(conn);
    client_handle.abort();
    server_handle.abort();
    upstream_handle.abort();
    let _ = std::fs::remove_file(&socket_path);
}

/// Poll `/api/v1/clients` until at least `n` entries appear or we time out.
async fn await_clients(socket_path: &std::path::Path, n: usize) -> Vec<Value> {
    for _ in 0..50 {
        let v = ctl::get(socket_path, "/api/v1/clients").await.unwrap();
        if let Some(arr) = v.as_array() {
            if arr.len() >= n {
                return arr.clone();
            }
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    panic!("timed out waiting for {n} client(s)");
}

async fn await_tunnels(socket_path: &std::path::Path, n: usize) -> Vec<Value> {
    for _ in 0..50 {
        let v = ctl::get(socket_path, "/api/v1/tunnels").await.unwrap();
        if let Some(arr) = v.as_array() {
            if arr.len() >= n {
                return arr.clone();
            }
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    panic!("timed out waiting for {n} tunnel(s)");
}
