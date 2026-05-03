//! Shared helpers for integration tests.
//!
//! This file lives at `tests/common/mod.rs` (not `tests/common.rs`) so that
//! cargo treats it as a normal module included by `mod common;` in each test
//! binary, rather than compiling it as its own test binary.
//!
//! Some helpers may be unused by individual test binaries — that's expected.

#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Once, OnceLock};
use std::time::Duration;

use rusnel::common::remote::RemoteRequest;
use rusnel::common::tls::{ClientTlsConfig, ServerTlsConfig};
use rusnel::{ClientConfig, ReconnectConfig, ServerConfig, ServerEndpoint};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const TEST_TIMEOUT: Duration = Duration::from_secs(20);
pub const STARTUP_DELAY: Duration = Duration::from_millis(500);

static INIT: Once = Once::new();

pub fn init_crypto() {
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install rustls crypto provider");
    });
}

// Test-port allocator.
//
// The previous `bind to :0, drop, return port` strategy relied on the
// kernel not handing the same just-released port back to a sibling test
// before we re-bound it. Under CI parallelism that assumption is wrong:
// concurrent tests in the same binary observed identical port numbers,
// one bound first, the other's `bind` silently failed, and a SOCKS5
// handshake landed on a plain TCP listener (saw `[5, 1, 0]`).
//
// The fix is a strictly monotonic, *atomic* counter scoped to the test
// process. `fetch_add` guarantees no two callers — concurrent or
// sequential — ever observe the same offset, so the same port number is
// never returned twice in a single `cargo test` invocation. We still
// probe-bind each candidate to skip ports another process happens to
// hold; with the monotonic counter, an in-use port advances the cursor
// past it on every retry instead of looping in place.
//
// Starting offset is seeded from the PID so parallel `cargo test`
// invocations on the same host don't all start at the same place.
const PORT_RANGE_START: u16 = 40_000;
const PORT_RANGE_SPAN: u16 = 20_000; // → 40_000 .. 60_000

static PORT_OFFSET: AtomicU32 = AtomicU32::new(0);
static PORT_BASE: OnceLock<u32> = OnceLock::new();

fn next_port_candidate() -> u16 {
    let base = *PORT_BASE.get_or_init(|| (std::process::id() % PORT_RANGE_SPAN as u32));
    let off = PORT_OFFSET.fetch_add(1, Ordering::Relaxed);
    PORT_RANGE_START + ((base + off) % PORT_RANGE_SPAN as u32) as u16
}

/// Reserve a TCP port that is *both* unique to this test run and currently
/// bindable. The atomic counter rules out in-process collisions; the bind
/// probe rules out a number some other process on the host happens to be
/// using. Loops until a free port is found — in practice one iteration.
pub fn get_available_port() -> u16 {
    loop {
        let port = next_port_candidate();
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

pub fn get_available_udp_port() -> u16 {
    loop {
        let port = next_port_candidate();
        if std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

pub fn server_config(port: u16, allow_reverse: bool) -> ServerConfig {
    server_config_with_tls(port, allow_reverse, ServerTlsConfig::Insecure)
}

pub fn server_config_with_tls(
    port: u16,
    allow_reverse: bool,
    tls: ServerTlsConfig,
) -> ServerConfig {
    ServerConfig {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port,
        allow_reverse,
        tls,
        congestion: Default::default(),
        max_connections: None,
    }
}

pub fn client_config(server_port: u16, remotes: Vec<RemoteRequest>) -> ClientConfig {
    client_config_with_tls(server_port, remotes, ClientTlsConfig::Insecure)
}

pub fn client_config_with_tls(
    server_port: u16,
    remotes: Vec<RemoteRequest>,
    tls: ClientTlsConfig,
) -> ClientConfig {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), server_port);
    ClientConfig {
        server: ServerEndpoint {
            addrs: vec![addr],
            host: addr.ip().to_string(),
        },
        remotes,
        tls,
        congestion: Default::default(),
        reconnect: ReconnectConfig::default(),
    }
}

pub struct TestEnv {
    pub server_handle: tokio::task::JoinHandle<()>,
    pub client_handle: tokio::task::JoinHandle<()>,
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        self.server_handle.abort();
        self.client_handle.abort();
    }
}

/// Spawn a server + client pair on localhost and wait long enough for both
/// the QUIC handshake and the per-tunnel listeners to come up.
pub async fn start_tunnel(
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

/// Perform a SOCKS5 no-auth handshake and CONNECT to an IPv4 target.
/// Returns the open stream (already past the SOCKS reply) ready for app data.
pub async fn socks5_connect_ipv4(
    socks_addr: &str,
    target_ip: [u8; 4],
    target_port: u16,
) -> TcpStream {
    let mut conn = TcpStream::connect(socks_addr).await.unwrap();

    // Greeting: version 5, 1 method, no-auth.
    conn.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    let mut greet_resp = [0u8; 2];
    conn.read_exact(&mut greet_resp).await.unwrap();
    assert_eq!(greet_resp, [0x05, 0x00]);

    let mut req = vec![
        0x05, // version
        0x01, // CONNECT
        0x00, // reserved
        0x01, // IPv4 address type
        target_ip[0],
        target_ip[1],
        target_ip[2],
        target_ip[3],
    ];
    req.extend_from_slice(&target_port.to_be_bytes());
    conn.write_all(&req).await.unwrap();

    // BND.ADDR + BND.PORT we don't care about the exact values — just that
    // the reply is success and the right length for an IPv4 BND.
    let mut reply = [0u8; 10];
    conn.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[0], 0x05, "SOCKS reply version");
    assert_eq!(reply[1], 0x00, "SOCKS reply status (0x00 = success)");

    conn
}

/// Perform a SOCKS5 no-auth handshake + UDP ASSOCIATE. Returns the still-open
/// TCP control connection (must be kept alive for the lifetime of the
/// association — closing it tears the relay down) and the UDP relay address
/// the SOCKS server bound for our datagrams.
pub async fn socks5_udp_associate(socks_addr: &str) -> (TcpStream, SocketAddr) {
    let mut conn = TcpStream::connect(socks_addr).await.unwrap();

    conn.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    let mut greet_resp = [0u8; 2];
    conn.read_exact(&mut greet_resp).await.unwrap();
    assert_eq!(greet_resp, [0x05, 0x00]);

    // VER=5 CMD=3(UDP ASSOCIATE) RSV=0 ATYP=1(IPv4) DST.ADDR=0.0.0.0 DST.PORT=0
    conn.write_all(&[0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await
        .unwrap();

    // Read fixed header VER REP RSV ATYP, then ATYP-dependent BND.ADDR + BND.PORT.
    let mut head = [0u8; 4];
    conn.read_exact(&mut head).await.unwrap();
    assert_eq!(head[0], 0x05, "SOCKS reply version");
    assert_eq!(head[1], 0x00, "UDP ASSOCIATE reply status");

    let bnd_ip: IpAddr = match head[3] {
        0x01 => {
            let mut o = [0u8; 4];
            conn.read_exact(&mut o).await.unwrap();
            IpAddr::V4(Ipv4Addr::from(o))
        }
        0x04 => {
            let mut o = [0u8; 16];
            conn.read_exact(&mut o).await.unwrap();
            IpAddr::V6(std::net::Ipv6Addr::from(o))
        }
        other => panic!("unexpected ATYP {other} in UDP ASSOCIATE reply"),
    };
    let mut port = [0u8; 2];
    conn.read_exact(&mut port).await.unwrap();
    let bnd_port = u16::from_be_bytes(port);

    // Some SOCKS servers reply with an unspecified BND.ADDR, expecting the
    // client to use the same host it connected to (RFC 1928 hint). Rusnel
    // returns the actual bound IP, but normalize defensively.
    let bnd_ip = if bnd_ip.is_unspecified() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        bnd_ip
    };

    (conn, SocketAddr::new(bnd_ip, bnd_port))
}

/// Build a SOCKS5 UDP datagram targeting the given IPv4 address with the
/// supplied payload.
pub fn socks5_udp_wrap_ipv4(target: SocketAddrV4, payload: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8, 0u8, 0u8, 0x01];
    buf.extend_from_slice(&target.ip().octets());
    buf.extend_from_slice(&target.port().to_be_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Decode a SOCKS5-wrapped UDP datagram, asserting it points at an IPv4
/// target, and return `(target_ip, target_port, payload)`.
pub fn socks5_udp_unwrap_ipv4(buf: &[u8]) -> ([u8; 4], u16, &[u8]) {
    assert!(buf.len() >= 10);
    assert_eq!(&buf[..3], &[0, 0, 0], "RSV/FRAG must be zero");
    assert_eq!(buf[3], 0x01, "expected IPv4 ATYP");
    let ip = [buf[4], buf[5], buf[6], buf[7]];
    let port = u16::from_be_bytes([buf[8], buf[9]]);
    (ip, port, &buf[10..])
}

/// Perform a SOCKS5 no-auth handshake and CONNECT to a domain target.
pub async fn socks5_connect_domain(
    socks_addr: &str,
    target_domain: &str,
    target_port: u16,
) -> TcpStream {
    let mut conn = TcpStream::connect(socks_addr).await.unwrap();

    conn.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    let mut greet_resp = [0u8; 2];
    conn.read_exact(&mut greet_resp).await.unwrap();
    assert_eq!(greet_resp, [0x05, 0x00]);

    let domain_bytes = target_domain.as_bytes();
    assert!(domain_bytes.len() <= u8::MAX as usize);

    let mut req = vec![
        0x05,
        0x01,
        0x00,
        0x03, // domain
        domain_bytes.len() as u8,
    ];
    req.extend_from_slice(domain_bytes);
    req.extend_from_slice(&target_port.to_be_bytes());
    conn.write_all(&req).await.unwrap();

    let mut reply = [0u8; 10];
    conn.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[0], 0x05);
    assert_eq!(reply[1], 0x00);

    conn
}
