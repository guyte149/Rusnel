use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::Once;
use std::time::Duration;

use rusnel::common::remote::RemoteRequest;
use rusnel::{ClientConfig, ServerConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::timeout;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const STARTUP_DELAY: Duration = Duration::from_millis(500);

static INIT: Once = Once::new();

fn init_crypto() {
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install rustls crypto provider");
    });
}

fn get_available_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn get_available_udp_port() -> u16 {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.local_addr().unwrap().port()
}

fn server_config(port: u16, allow_reverse: bool) -> ServerConfig {
    ServerConfig {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port,
        allow_reverse,
    }
}

fn client_config(server_port: u16, remotes: Vec<RemoteRequest>) -> ClientConfig {
    ClientConfig {
        server: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), server_port),
        remotes,
    }
}

struct TestEnv {
    server_handle: tokio::task::JoinHandle<()>,
    client_handle: tokio::task::JoinHandle<()>,
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        self.server_handle.abort();
        self.client_handle.abort();
    }
}

async fn start_tunnel(
    server_port: u16,
    allow_reverse: bool,
    remotes: Vec<RemoteRequest>,
) -> TestEnv {
    init_crypto();
    let sc = server_config(server_port, allow_reverse);
    let server_handle = tokio::spawn(async move {
        let _ = rusnel::server::run_async(sc).await;
    });

    tokio::time::sleep(STARTUP_DELAY).await;

    let cc = client_config(server_port, remotes);
    let client_handle = tokio::spawn(async move {
        let _ = rusnel::client::run_async(cc).await;
    });

    tokio::time::sleep(STARTUP_DELAY).await;

    TestEnv {
        server_handle,
        client_handle,
    }
}

#[tokio::test]
async fn test_tcp_forward() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let local_port = get_available_port();
        let remote_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{remote_port}"))
            .await
            .unwrap();

        let remote =
            RemoteRequest::from_str(&format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"))
                .unwrap();

        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let mut client_conn = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();

        let (mut target_stream, _) = target_listener.accept().await.unwrap();

        let test_data = b"hello from tcp forward test";
        client_conn.write_all(test_data).await.unwrap();
        client_conn.shutdown().await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = target_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], test_data);
    })
    .await
    .expect("test_tcp_forward timed out");
}

#[tokio::test]
async fn test_tcp_forward_bidirectional() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let local_port = get_available_port();
        let remote_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{remote_port}"))
            .await
            .unwrap();

        let remote =
            RemoteRequest::from_str(&format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"))
                .unwrap();

        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let mut client_conn = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();

        let (mut target_stream, _) = target_listener.accept().await.unwrap();

        // Client -> Target
        let request = b"GET /data HTTP/1.0\r\n\r\n";
        client_conn.write_all(request).await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = target_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], request.as_slice());

        // Target -> Client
        let response = b"HTTP/1.0 200 OK\r\n\r\nresponse body";
        target_stream.write_all(response).await.unwrap();
        target_stream.shutdown().await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = client_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], response.as_slice());
    })
    .await
    .expect("test_tcp_forward_bidirectional timed out");
}

#[tokio::test]
async fn test_tcp_reverse() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let listen_port = get_available_port();
        let target_port = get_available_port();

        // Target service on client side
        let target_listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        let remote = RemoteRequest::from_str(&format!(
            "R:127.0.0.1:{listen_port}:127.0.0.1:{target_port}"
        ))
        .unwrap();

        let _env = start_tunnel(server_port, true, vec![remote]).await;

        // Connect to the reverse-forwarded port
        let mut client_conn = TcpStream::connect(format!("127.0.0.1:{listen_port}"))
            .await
            .unwrap();

        let (mut target_stream, _) = target_listener.accept().await.unwrap();

        let test_data = b"hello from tcp reverse test";
        client_conn.write_all(test_data).await.unwrap();
        client_conn.shutdown().await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = target_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], test_data);
    })
    .await
    .expect("test_tcp_reverse timed out");
}

#[tokio::test]
async fn test_udp_forward() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let local_port = get_available_udp_port();
        let remote_port = get_available_udp_port();

        let target_socket = UdpSocket::bind(format!("127.0.0.1:{remote_port}"))
            .await
            .unwrap();

        let remote = RemoteRequest::from_str(&format!(
            "127.0.0.1:{local_port}:127.0.0.1:{remote_port}/udp"
        ))
        .unwrap();

        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let test_data = b"hello from udp forward test";
        sender
            .send_to(test_data, format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();

        let mut buf = vec![0u8; 1024];
        let n = target_socket.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], test_data);
    })
    .await
    .expect("test_udp_forward timed out");
}

#[tokio::test]
async fn test_udp_reverse() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let listen_port = get_available_udp_port();
        let target_port = get_available_udp_port();

        let target_socket = UdpSocket::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        let remote = RemoteRequest::from_str(&format!(
            "R:127.0.0.1:{listen_port}:127.0.0.1:{target_port}/udp"
        ))
        .unwrap();

        let _env = start_tunnel(server_port, true, vec![remote]).await;

        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let test_data = b"hello from udp reverse test";
        sender
            .send_to(test_data, format!("127.0.0.1:{listen_port}"))
            .await
            .unwrap();

        let mut buf = vec![0u8; 1024];
        let n = target_socket.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], test_data);
    })
    .await
    .expect("test_udp_reverse timed out");
}

#[tokio::test]
async fn test_socks5_forward() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let socks_port = get_available_port();
        let target_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        let remote = RemoteRequest::from_str(&format!("127.0.0.1:{socks_port}:socks")).unwrap();

        let _env = start_tunnel(server_port, false, vec![remote]).await;

        // Perform SOCKS5 handshake
        let mut socks_conn = TcpStream::connect(format!("127.0.0.1:{socks_port}"))
            .await
            .unwrap();

        // Greeting: version 5, 1 method (no auth)
        socks_conn.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut resp = [0u8; 2];
        socks_conn.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp, [0x05, 0x00]); // no auth selected

        // CONNECT request to 127.0.0.1:target_port
        let mut connect_req = vec![
            0x05, // version
            0x01, // CONNECT
            0x00, // reserved
            0x01, // IPv4
            127, 0, 0, 1,
        ];
        connect_req.extend_from_slice(&target_port.to_be_bytes());
        socks_conn.write_all(&connect_req).await.unwrap();

        let mut resp = [0u8; 10];
        socks_conn.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp[0], 0x05); // version
        assert_eq!(resp[1], 0x00); // success

        // Send data through SOCKS tunnel
        let test_data = b"hello through socks5 proxy";
        socks_conn.write_all(test_data).await.unwrap();
        socks_conn.shutdown().await.unwrap();

        let (mut target_stream, _) = target_listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = target_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], test_data);
    })
    .await
    .expect("test_socks5_forward timed out");
}

#[tokio::test]
async fn test_multiple_tcp_remotes() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let local_port_1 = get_available_port();
        let remote_port_1 = get_available_port();
        let local_port_2 = get_available_port();
        let remote_port_2 = get_available_port();

        let target_1 = TcpListener::bind(format!("127.0.0.1:{remote_port_1}"))
            .await
            .unwrap();
        let target_2 = TcpListener::bind(format!("127.0.0.1:{remote_port_2}"))
            .await
            .unwrap();

        let remotes = vec![
            RemoteRequest::from_str(&format!(
                "127.0.0.1:{local_port_1}:127.0.0.1:{remote_port_1}"
            ))
            .unwrap(),
            RemoteRequest::from_str(&format!(
                "127.0.0.1:{local_port_2}:127.0.0.1:{remote_port_2}"
            ))
            .unwrap(),
        ];

        let _env = start_tunnel(server_port, false, remotes).await;

        // Test first tunnel
        let mut conn1 = TcpStream::connect(format!("127.0.0.1:{local_port_1}"))
            .await
            .unwrap();
        let (mut target_stream_1, _) = target_1.accept().await.unwrap();

        let data1 = b"data for tunnel 1";
        conn1.write_all(data1).await.unwrap();
        conn1.shutdown().await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = target_stream_1.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], data1);

        // Test second tunnel
        let mut conn2 = TcpStream::connect(format!("127.0.0.1:{local_port_2}"))
            .await
            .unwrap();
        let (mut target_stream_2, _) = target_2.accept().await.unwrap();

        let data2 = b"data for tunnel 2";
        conn2.write_all(data2).await.unwrap();
        conn2.shutdown().await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = target_stream_2.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], data2);
    })
    .await
    .expect("test_multiple_tcp_remotes timed out");
}
