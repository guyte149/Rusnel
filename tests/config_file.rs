//! End-to-end smoke tests for `--config <PATH>` (TOML) on the
//! `rusnel server` and `rusnel client` subcommands.
//!
//! These tests spawn the actual `rusnel` binary instead of going
//! through the library API, so we exercise the full clap → file →
//! merge path that real users hit.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Path to the freshly-built `rusnel` binary that cargo always builds
/// before running integration tests.
fn rusnel_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rusnel")
}

#[test]
fn server_help_advertises_config_flag() {
    let out = Command::new(rusnel_bin())
        .args(["server", "--help"])
        .output()
        .expect("spawn rusnel");
    assert!(out.status.success(), "rusnel server --help failed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--config"),
        "missing --config in help: {stdout}"
    );
}

#[test]
fn client_help_advertises_config_flag() {
    let out = Command::new(rusnel_bin())
        .args(["client", "--help"])
        .output()
        .expect("spawn rusnel");
    assert!(out.status.success(), "rusnel client --help failed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--config"),
        "missing --config in help: {stdout}"
    );
}

#[test]
fn missing_config_file_errors_clearly() {
    let out = Command::new(rusnel_bin())
        .args(["server", "--config", "/definitely/not/a/real/path.toml"])
        .output()
        .expect("spawn rusnel");
    assert!(!out.status.success(), "should have failed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("failed to read config file"),
        "expected wrapped read error in stderr, got: {stderr}"
    );
}

#[test]
fn unknown_field_in_config_file_rejected() {
    let mut tmp = tempfile_in_target("rusnel-bad.toml");
    writeln!(tmp.file, "[server]\nport = 9090\nbogus_field = true\n").unwrap();
    let out = Command::new(rusnel_bin())
        .args(["server", "--config"])
        .arg(&tmp.path)
        .output()
        .expect("spawn rusnel");
    assert!(!out.status.success(), "should have failed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("bogus_field") || stderr.contains("unknown field"),
        "expected unknown-field error in stderr, got: {stderr}"
    );
}

#[test]
fn server_loads_settings_from_config_file() {
    // Spawn the server with values from the file, wait for the
    // "Listening on …" log line, then kill. We check the address in
    // the log matches the values we put in the file.
    let port = pick_free_port();
    let mut tmp = tempfile_in_target("rusnel-server.toml");
    writeln!(
        tmp.file,
        "[server]\nhost = \"127.0.0.1\"\nport = {port}\ninsecure = true\n"
    )
    .unwrap();
    let stderr =
        run_server_briefly(&["server", "--config", tmp.path.to_str().expect("utf-8 path")]);
    let needle = format!("127.0.0.1:{port}");
    assert!(
        stderr.contains(&needle),
        "expected log to mention `{needle}` from config file; stderr was:\n{stderr}"
    );
}

#[test]
fn cli_flag_overrides_config_file() {
    // File says port = X, CLI passes --port Y. The log line should
    // mention Y, not X.
    let file_port = pick_free_port();
    let cli_port = pick_free_port();
    assert_ne!(file_port, cli_port);
    let mut tmp = tempfile_in_target("rusnel-override.toml");
    writeln!(
        tmp.file,
        "[server]\nhost = \"127.0.0.1\"\nport = {file_port}\ninsecure = true\n"
    )
    .unwrap();
    let stderr = run_server_briefly(&[
        "server",
        "--config",
        tmp.path.to_str().expect("utf-8 path"),
        "--port",
        &cli_port.to_string(),
    ]);
    assert!(
        stderr.contains(&format!(":{cli_port}")),
        "expected CLI --port {cli_port} to win; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains(&format!(":{file_port}")),
        "file's port {file_port} should have been overridden; stderr:\n{stderr}"
    );
}

/// Spawn `rusnel <args>`, wait up to ~1.5 s for the "Listening on …"
/// log line on stderr, then kill the process. Returns the captured
/// stderr.
fn run_server_briefly(args: &[&str]) -> String {
    let mut child = Command::new(rusnel_bin())
        .args(args)
        .env("RUST_LOG", "rusnel=info,warn")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rusnel");
    let mut stderr = child.stderr.take().expect("stderr pipe");

    // Read non-blockingly by polling a background reader thread.
    let (tx, rx) = std::sync::mpsc::channel::<u8>();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match stderr.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    for &b in &buf[..n] {
                        if tx.send(b).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    });

    let deadline = Instant::now() + Duration::from_millis(2500);
    let mut collected = String::new();
    // Read until we see the "server listening" line in full (i.e. up
    // to and including its trailing newline). Don't early-break the
    // moment we spot the *substring* "listening" — that drops the
    // `addr=…` field that follows it on the same line in the
    // compact log format.
    let mut saw_listening_line = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(b) => {
                collected.push(b as char);
                if !saw_listening_line && collected.contains("server listening") {
                    saw_listening_line = true;
                }
                if saw_listening_line && b == b'\n' {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    collected
}

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind to ephemeral port");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

// ----------------------------------------------------------------------
// Tiny tempfile helper — the `tempfile` crate isn't a project dep, and
// pulling it in just for two tests would be silly. We write into the
// cargo target dir so cleanup is obvious and CI doesn't accumulate
// state in $TMPDIR across runs.
// ----------------------------------------------------------------------

struct TempPath {
    path: std::path::PathBuf,
    file: std::fs::File,
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn tempfile_in_target(name: &str) -> TempPath {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).expect("create CARGO_TARGET_TMPDIR");
    let path = dir.join(format!(
        "{}-{}-{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let file = std::fs::File::create(&path).expect("create tempfile");
    TempPath { path, file }
}
