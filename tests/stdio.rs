//! End-to-end smoke test for `stdio:` forward remotes.
//!
//! The stdio data path can't be exercised in-process — `tunnel_stdio_client`
//! reads from `tokio::io::stdin()` / writes to `tokio::io::stdout()`, both of
//! which are process-global handles. So this test spawns the actual `rusnel`
//! binary as a subprocess (cargo exposes its path via `CARGO_BIN_EXE_rusnel`
//! at integration-test compile time), points it at an in-process server +
//! echo target, and pipes payload through the child's stdin/stdout.
//!
//! Verifies three things in one shot:
//!   1. `stdio:host:port` parses + dispatches end-to-end (CLI → session
//!      hello → server-side TCP connect to the echo target).
//!   2. Bytes flow bidirectionally over the QUIC stream piped to/from the
//!      child's stdio.
//!   3. Closing the child's stdin (EOF) drives a clean shutdown of the
//!      whole client — the process exits 0 instead of hanging.

mod common;

use std::process::Stdio;
use std::time::Duration;

use common::{get_available_port, init_crypto, server_config};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::time::timeout;

/// Bound for *any* await in this test. Generous (10s) because the child
/// has to: build nothing (cargo prebuilds the bin), exec, do a QUIC
/// handshake against `--insecure`, send the session hello, and only then
/// open the stdio bi-stream. On a cold CI runner the handshake alone can
/// take a few hundred ms.
const STEP_TIMEOUT: Duration = Duration::from_secs(10);

/// Tiny TCP echo server: accept once, echo bytes until the peer half-closes,
/// then drop. Returns the listener's address. The accept loop runs in a
/// background task that owns the listener for the lifetime of the test.
async fn spawn_echo_server() -> std::net::SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // One accept is enough — stdio is single-conn by design.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (mut r, mut w) = sock.split();
        let _ = tokio::io::copy(&mut r, &mut w).await;
        let _ = w.shutdown().await;
    });
    addr
}

#[tokio::test]
async fn stdio_forward_pipes_stdin_stdout_through_tunnel() {
    init_crypto();

    let server_port = get_available_port();
    let echo_addr = spawn_echo_server().await;

    // In-process server. We only need the server half — the client is
    // the subprocess we spawn below — so we can't reuse `start_tunnel`
    // (which spawns both).
    let sc = server_config(server_port, false);
    let server_handle = tokio::spawn(async move {
        let _ = rusnel::server::run_async(sc).await;
    });
    // Same delay `start_tunnel` uses for the QUIC listener to come up.
    tokio::time::sleep(common::STARTUP_DELAY).await;

    let bin = env!("CARGO_BIN_EXE_rusnel");
    let server_url = format!("127.0.0.1:{server_port}");
    let stdio_remote = format!("stdio:127.0.0.1:{}", echo_addr.port());

    let mut child = Command::new(bin)
        .args([
            "client",
            "--insecure",
            // Cap reconnects: a stdio client is single-shot. `0` means
            // "no retries" — the process exits once the session ends.
            "--max-retry-count",
            "0",
            &server_url,
            &stdio_remote,
        ])
        // Inherit stderr so test logs are visible on failure; capture
        // stdin/stdout — those are the tunnel.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to spawn rusnel client subprocess");

    let mut stdin = child.stdin.take().expect("captured stdin");
    let stdout = child.stdout.take().expect("captured stdout");
    let mut reader = BufReader::new(stdout);

    // Round-trip a few framed messages through the echo. Use newline
    // framing so we can read line-by-line without guessing chunk
    // boundaries — quinn may coalesce or split writes.
    let messages = ["hello\n", "stdio tunnel\n", "third frame\n"];
    for msg in messages {
        timeout(STEP_TIMEOUT, stdin.write_all(msg.as_bytes()))
            .await
            .expect("write to child stdin timed out")
            .expect("write to child stdin failed");
        timeout(STEP_TIMEOUT, stdin.flush())
            .await
            .expect("flush child stdin timed out")
            .expect("flush child stdin failed");

        let mut got = String::new();
        timeout(STEP_TIMEOUT, reader.read_line(&mut got))
            .await
            .expect("read from child stdout timed out")
            .expect("read from child stdout failed");
        assert_eq!(got, msg, "echo mismatch (sent vs got)");
    }

    // Drop stdin: this should propagate EOF through the tunnel, the echo
    // server shuts its write half, the child's stdout reader sees EOF,
    // the stdio task fires the shutdown signal, and the client exits 0.
    drop(stdin);

    // Drain anything still buffered in stdout so the child's quic→stdout
    // copy can complete cleanly (without this read, the child can block
    // on `stdout.write_all` if the echo server flushed bytes after our
    // last read).
    let mut tail = Vec::new();
    let _ = timeout(STEP_TIMEOUT, reader.read_to_end(&mut tail)).await;

    let status = timeout(STEP_TIMEOUT, child.wait())
        .await
        .expect("child did not exit after stdin EOF")
        .expect("child wait failed");
    assert!(
        status.success(),
        "rusnel client exited with non-zero status: {status:?}"
    );

    server_handle.abort();
}
