use clap::crate_version;
use clap::error::ErrorKind;
use clap::{Args as ClapArgs, CommandFactory, Parser, Subcommand, ValueEnum};
use rusnel::cert;
use rusnel::common::proxy::ProxyConfig;
use rusnel::common::quic::Congestion;
use rusnel::common::remote::RemoteRequest;
use rusnel::common::tls::{parse_fingerprint, ClientTlsConfig, ServerTlsConfig};
use rusnel::embedded::{self, Materialized};
use rusnel::{run_client, run_server, ClientConfig, ReconnectConfig, ServerConfig, ServerEndpoint};

/// CLI mirror of `rusnel::common::quic::Congestion`. Kept separate so that
/// clap's `ValueEnum` derive lives in the binary crate and doesn't pull
/// `clap` into the library.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum CongestionArg {
    /// CUBIC — loss-based, the same algorithm Linux TCP uses by default.
    /// Fast and predictable on near-zero-RTT loopback and well-tuned LANs.
    /// Ramps up over many RTTs on high-latency links.
    #[default]
    Cubic,
    /// BBR — model-based, paces sending to the link's bottleneck bandwidth.
    /// Wins on high-BDP / lossy links (real WAN, satellite, cellular)
    /// where CUBIC's slow-start is the bottleneck. On near-zero-RTT
    /// loopback the bandwidth estimator settles low and *under*paces, so
    /// single-stream local throughput drops noticeably — pick it only
    /// when latency × bandwidth is non-trivial.
    Bbr,
}

impl From<CongestionArg> for Congestion {
    fn from(c: CongestionArg) -> Self {
        match c {
            CongestionArg::Cubic => Congestion::Cubic,
            CongestionArg::Bbr => Congestion::Bbr,
        }
    }
}
use std::net::{IpAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use tracing::{debug, info};

/// Parse a non-negative integer number of seconds into a [`Duration`].
fn parse_duration_secs(s: &str) -> Result<Duration, String> {
    let secs: u64 = s
        .parse()
        .map_err(|e| format!("invalid duration `{s}` (expected whole seconds): {e}"))?;
    Ok(Duration::from_secs(secs))
}

/// Convert the raw `--max-retry-count` value (chisel-style: any negative
/// number means "retry forever") into the [`Option<u32>`] our library API
/// uses. Kept as a free function rather than a clap `value_parser` because
/// clap's derive treats `Option<T>`-typed fields as plain optional arguments,
/// so the parser must return `T`, not `Option<T>`.
fn max_retries_from_cli(raw: i64) -> Result<Option<u32>, String> {
    if raw < 0 {
        Ok(None)
    } else {
        u32::try_from(raw)
            .map(Some)
            .map_err(|_| format!("retry count `{raw}` is too large"))
    }
}

/// Resolve `host:port` to a `ServerEndpoint`, preserving the original host
/// string so it can be reused as the TLS SNI value (see `client_server_name`).
/// Used as a clap `value_parser` so resolution failures surface as a
/// `clap::Error` (consistent formatting + exit code 2) instead of a panic
/// (#20 §4).
///
/// We collect *all* resolved addresses (not just the first), and reorder them
/// per RFC 8305: alternate address families starting with the resolver's
/// preferred family. The client races them with Happy Eyeballs so a host like
/// `localhost` that resolves to both `[::1]` and `127.0.0.1` connects quickly
/// even when only one family has a listener — matching what curl, `ssh`, and
/// chisel's Go-based client do out of the box.
fn parse_server_addr(s: &str) -> Result<ServerEndpoint, String> {
    // Split host:port without losing the host. We do this manually instead of
    // relying on `SocketAddr::from_str` because we want to accept DNS names
    // too, not just IP literals.
    let host = if let Some(rest) = s.strip_prefix('[') {
        // IPv6 literal form: `[addr]:port`.
        let close = rest
            .find(']')
            .ok_or_else(|| format!("malformed IPv6 address in `{s}` (missing `]`)"))?;
        rest[..close].to_string()
    } else {
        s.rsplit_once(':')
            .ok_or_else(|| format!("expected host:port in `{s}`"))?
            .0
            .to_string()
    };

    let resolved: Vec<_> = s
        .to_socket_addrs()
        .map_err(|e| format!("failed to resolve server address `{s}`: {e}"))?
        .collect();
    if resolved.is_empty() {
        return Err(format!("no addresses found for server `{s}`"));
    }

    let addrs = interleave_address_families(resolved);
    Ok(ServerEndpoint { addrs, host })
}

/// Reorder resolved addresses per RFC 8305 §4: the first address keeps its
/// resolver-preferred family, and subsequent addresses alternate families so
/// the alternate family is reached after a single "Connection Attempt Delay"
/// regardless of how many same-family addresses come first. With both v4 and
/// v6 in play this means we always race the *other* family second instead of
/// burning attempts on every same-family candidate first.
fn interleave_address_families(resolved: Vec<std::net::SocketAddr>) -> Vec<std::net::SocketAddr> {
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    for a in resolved {
        if a.is_ipv6() {
            v6.push(a);
        } else {
            v4.push(a);
        }
    }
    // Whichever family the resolver returned first goes first. Falling back
    // to v4 is irrelevant — empty primary means we just iterate the other
    // bucket — but we still want a deterministic preference for tests.
    let (mut primary, mut alternate) = if !v6.is_empty() && !v4.is_empty() {
        // Both families present: preserve resolver preference (v6 first on
        // most modern Unix per the default `gai.conf`/RFC 6724 ordering).
        (v6.into_iter(), v4.into_iter())
    } else {
        (v4.into_iter(), v6.into_iter())
    };
    let mut out = Vec::new();
    loop {
        match (primary.next(), alternate.next()) {
            (Some(a), Some(b)) => {
                out.push(a);
                out.push(b);
            }
            (Some(a), None) => out.push(a),
            (None, Some(b)) => out.push(b),
            (None, None) => break,
        }
    }
    out
}

/// Parse a remote spec via `RemoteRequest::from_str`, surfacing parse errors
/// as `clap` errors instead of `eprintln! + process::exit` (#20 §4 + §5).
fn parse_remote(s: &str) -> Result<RemoteRequest, String> {
    RemoteRequest::from_str(s).map_err(|e| format!("invalid remote `{s}`: {e}"))
}

fn parse_proxy(s: &str) -> Result<ProxyConfig, String> {
    ProxyConfig::from_str(s)
}

/// Rusnel is a fast tcp/udp multiplexed tunnel.
#[derive(Parser)]
#[command(name = "Rusnel", version = crate_version!())]
#[command(about = "A fast tcp/udp tunnel", long_about = None)]
struct Args {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Debug, Subcommand)]
enum Mode {
    /// run Rusnel in server mode
    ///
    /// Exactly one of --insecure, --tls-self-signed, or --tls-cert/--tls-key
    /// must be set, unless the binary was built with embedded server
    /// credentials (see RUSNEL_EMBED_* in build.rs), in which case those are
    /// used as the default.
    #[allow(clippy::too_many_arguments)]
    Server {
        /// defines Rusnel listening host (the network interface)
        #[arg(long, default_value_t = IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0)))]
        host: IpAddr,

        /// defines Rusnel listening port
        #[arg(long, short, default_value_t = 8080)]
        port: u16,

        /// Allow clients to specify reverse port forwarding remotes.
        #[arg(long, default_value_t = false)]
        allow_reverse: bool,

        /// Allow clients to specify SOCKS5 remotes. `R:socks` additionally requires `--allow-reverse`.
        #[arg(long, default_value_t = false)]
        allow_socks: bool,

        /// Disable all TLS authentication. MITM-vulnerable; for testing only.
        ///
        /// Uses an ephemeral self-signed certificate and accepts any client.
        #[arg(long, default_value_t = false)]
        insecure: bool,

        /// Use a self-signed cert persisted under --tls-state-dir (default: ~/.rusnel).
        ///
        /// Generated on first run; reused on subsequent runs so the fingerprint
        /// stays stable.
        #[arg(long, default_value_t = false, conflicts_with_all = ["insecure", "tls_cert", "tls_key"])]
        tls_self_signed: bool,

        /// Directory used to persist the self-signed cert/key. Implies
        /// --tls-self-signed.
        #[arg(long, value_name = "DIR", requires = "tls_self_signed")]
        tls_state_dir: Option<PathBuf>,

        /// Path to the server's PEM-encoded certificate. Must be paired with
        /// --tls-key.
        #[arg(long, value_name = "PATH", requires = "tls_key", conflicts_with_all = ["insecure", "tls_self_signed"])]
        tls_cert: Option<PathBuf>,

        /// Path to the server's PEM-encoded private key. Must be paired with
        /// --tls-cert.
        #[arg(long, value_name = "PATH", requires = "tls_cert")]
        tls_key: Option<PathBuf>,

        /// Enable mTLS: require connecting clients to present a certificate
        /// chained to this CA bundle. Must be paired with --tls-cert/--tls-key.
        #[arg(long, value_name = "PATH", requires = "tls_cert", conflicts_with_all = ["insecure", "tls_self_signed"])]
        tls_ca: Option<PathBuf>,

        /// QUIC congestion controller.
        ///
        /// `cubic` (default) is the same algorithm Linux TCP uses — predictable
        /// and fastest on loopback / clean LANs. `bbr` is model-based and wins
        /// on high-BDP / lossy WAN links where CUBIC slow-start is the
        /// bottleneck, but underpaces on near-zero-RTT loopback. Pick `bbr`
        /// when latency × bandwidth is non-trivial (≳25ms RTT or any loss).
        #[arg(long, value_enum, default_value_t = CongestionArg::Cubic)]
        congestion: CongestionArg,

        /// Cap on concurrent client connections; `0` (default) = uncapped.
        ///
        /// Once the cap is reached, additional connections are refused at the
        /// QUIC layer until an existing one closes. quinn's per-connection
        /// stream limit still applies on top of this.
        #[arg(long, value_name = "N", default_value_t = 0)]
        max_connections: usize,

        /// Path to the admin HTTP API unix socket.
        ///
        /// Defaults to `~/.rusnel/admin.sock` (auto-created with mode
        /// 0600). Pass `--no-admin-socket` to disable the admin API
        /// entirely; pass an explicit path here to override the default
        /// (e.g. when running multiple rusnel servers as the same uid).
        /// Query the API with `rusnel ctl ...` or
        /// `curl --unix-socket <path> http://x/api/v1/clients`.
        #[arg(long, value_name = "PATH", conflicts_with = "no_admin_socket")]
        admin_socket: Option<PathBuf>,

        /// Disable the admin HTTP API. Mutually exclusive with
        /// `--admin-socket`.
        #[arg(long, default_value_t = false)]
        no_admin_socket: bool,

        /// enable verbose logging
        #[arg(short('v'), long("verbose"), default_value_t = false)]
        is_verbose: bool,

        /// enable debug logging
        #[arg(long("debug"), default_value_t = false)]
        is_debug: bool,
    },
    /// run Rusnel in client mode
    ///
    /// Exactly one of --insecure, --tls-fingerprint, or --tls-ca must be set,
    /// unless the binary was built with embedded client credentials, in which
    /// case those are used as the default.
    #[allow(clippy::too_many_arguments)]
    Client {
        /// defines the Rusnel server address (in form of host:port)
        #[arg(value_parser = parse_server_addr)]
        server: ServerEndpoint,

        #[arg(name = "remote", required = true, value_parser = parse_remote, value_delimiter = ' ', num_args = 1.., help=r#"
<remote>s are remote connections tunneled through the server, each which come in the form:

    <local-host>:<local-port>:<remote-host>:<remote-port>/<protocol>

    ■ local-host defaults to 0.0.0.0 (all interfaces).
    ■ local-port defaults to remote-port.
    ■ remote-port is required*.
    ■ remote-host defaults to 0.0.0.0 (server localhost).
    ■ protocol defaults to tcp.

which shares <remote-host>:<remote-port> from the server to the client as <local-host>:<local-port>, or:

    R:<local-host>:<local-port>:<remote-host>:<remote-port>/<protocol>

which does reverse port forwarding,
sharing <remote-host>:<remote-port> from the client to the server\'s <local-host>:<local-port>.

    example remotes

        1337
        example.com:1337
        1337:google.com:80
        192.168.1.14:5000:google.com:80
        socks
        5000:socks
        R:2222:localhost:22
        R:socks
        R:5000:socks
        1.1.1.1:53/udp
        [::1]:80
        [::1]:5000:[2001:db8::1]:80

    IPv6 literals must be wrapped in [brackets] (same convention as URLs and ssh -L).

    When the Rusnel server has --allow-reverse enabled, remotes can be prefixed with R to denote that they are reversed.

    Remotes can specify "socks" in place of remote-host and remote-port.
    The default local host and port for a "socks" remote is 127.0.0.1:1080.
        "#)]
        remotes: Vec<RemoteRequest>,

        /// Disable server certificate verification. MITM-vulnerable; for testing only.
        #[arg(long, default_value_t = false)]
        insecure: bool,

        /// Pin the server's leaf certificate by SHA-256 fingerprint.
        ///
        /// Accepts `sha256:<hex>`, bare hex, or colon-separated hex. The
        /// expected value is logged by the server at startup as
        /// `server cert fingerprint: sha256:<hex>`.
        #[arg(long, value_name = "SHA256", conflicts_with_all = ["insecure", "tls_ca"])]
        tls_fingerprint: Option<String>,

        /// Verify the server certificate against this CA bundle.
        ///
        /// Use alone for server-auth-only TLS, or pair with --tls-cert/--tls-key
        /// for full mTLS.
        #[arg(long, value_name = "PATH", conflicts_with = "insecure")]
        tls_ca: Option<PathBuf>,

        /// Path to the client's PEM-encoded certificate. Must be paired with
        /// --tls-key and --tls-ca.
        #[arg(long, value_name = "PATH", requires_all = ["tls_key", "tls_ca"])]
        tls_cert: Option<PathBuf>,

        /// Path to the client's PEM-encoded private key. Must be paired with
        /// --tls-cert and --tls-ca.
        #[arg(long, value_name = "PATH", requires_all = ["tls_cert", "tls_ca"])]
        tls_key: Option<PathBuf>,

        /// Override the SNI / server name sent during the TLS handshake.
        ///
        /// With --tls-ca, this name must match a SAN in the server certificate.
        /// With --tls-fingerprint, the value is sent as SNI but ignored during
        /// verification.
        #[arg(long, value_name = "NAME")]
        tls_server_name: Option<String>,

        /// QUIC congestion controller.
        ///
        /// `cubic` (default) is the same algorithm Linux TCP uses — predictable
        /// and fastest on loopback / clean LANs. `bbr` is model-based and wins
        /// on high-BDP / lossy WAN links where CUBIC slow-start is the
        /// bottleneck, but underpaces on near-zero-RTT loopback. Pick `bbr`
        /// when latency × bandwidth is non-trivial (≳25ms RTT or any loss).
        ///
        /// The client and server can run with different controllers; QUIC
        /// negotiates each direction independently.
        #[arg(long, value_enum, default_value_t = CongestionArg::Cubic)]
        congestion: CongestionArg,

        /// Maximum reconnect attempts after a failure; `-1` (default) = retry forever.
        ///
        /// The counter resets after every successful connection.
        #[arg(long, value_name = "N", default_value_t = -1, allow_hyphen_values = true)]
        max_retry_count: i64,

        /// Cap on the exponential reconnect backoff, in seconds (default 300).
        ///
        /// The client starts at 200 ms and doubles on each successive failure
        /// up to this value (matching chisel).
        #[arg(long, value_name = "SECONDS", default_value = "300", value_parser = parse_duration_secs)]
        max_retry_interval: Duration,

        /// Route the QUIC connection through a SOCKS5 proxy via UDP ASSOCIATE. Form: `socks5://[user:pass@]host:port`.
        ///
        /// `socks://` is accepted as an alias. HTTP CONNECT is not supported
        /// because it cannot carry UDP/QUIC. The proxy must permit UDP
        /// ASSOCIATE; many corporate / hotel HTTP proxies do not. A fresh
        /// UDP association is opened on every (re)connect.
        #[arg(long, value_name = "URL", value_parser = parse_proxy)]
        proxy: Option<ProxyConfig>,

        /// enable verbose logging
        #[arg(short('v'), long("verbose"), default_value_t = false)]
        is_verbose: bool,

        /// enable debug logging
        #[arg(long("debug"), default_value_t = false)]
        is_debug: bool,
    },
    /// generate certificates for use with --tls-* flags
    Cert {
        #[command(subcommand)]
        action: CertAction,
    },
    /// Query a running server's admin API over a unix socket.
    ///
    /// The server must be started with --admin-socket <path>. By default
    /// `ctl` looks at $XDG_RUNTIME_DIR/rusnel-admin.sock (Linux) or
    /// /tmp/rusnel-admin-<uid>.sock (macOS / no XDG); pass --socket to
    /// override. Output defaults to a tab-aligned table; pass --json to
    /// pipe the raw API response.
    Ctl {
        /// Path to the admin unix socket.
        #[arg(long, value_name = "PATH")]
        socket: Option<PathBuf>,
        /// Print the raw JSON API response instead of a formatted table.
        #[arg(long, default_value_t = false)]
        json: bool,
        #[command(subcommand)]
        action: CtlAction,
    },
}

#[derive(Debug, Subcommand)]
enum CtlAction {
    /// Print server info: version, listen address, uptime, client count.
    Server,
    /// List currently-connected clients.
    Clients,
    /// Show full detail (including tunnels) for one client.
    Client {
        /// Client id from `ctl clients`.
        id: u64,
    },
    /// List active conns on one client (across all of its tunnels).
    ClientConns {
        /// Client id from `ctl clients`.
        id: u64,
    },
    /// List every tunnel (remote declaration) across every client.
    Tunnels,
    /// Show full detail for one tunnel, including its active conns.
    Tunnel {
        /// Tunnel id from `ctl tunnels`.
        id: u64,
    },
    /// List active conns going through one tunnel.
    TunnelConns {
        /// Tunnel id from `ctl tunnels`.
        id: u64,
    },
    /// List every active conn across every tunnel.
    Conns,
    /// Recent client disconnects (most recent first).
    History {
        /// Cap on the number of rows to fetch.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },
}

#[derive(Debug, Subcommand)]
enum CertAction {
    /// Create a self-signed certificate authority that can sign server and
    /// client certs.
    Ca(CaArgs),
    /// Issue a server certificate signed by an existing CA. Requires at least
    /// one --name (DNS) or --ip SAN matching how clients will reach the
    /// server.
    Server(ServerCertArgs),
    /// Issue a client certificate signed by an existing CA.
    Client(ClientCertArgs),
    /// Print the SHA-256 fingerprint of the leaf certificate in a PEM file
    /// (the value `--tls-fingerprint` expects).
    Fingerprint {
        /// Path to a PEM-encoded certificate (e.g. server.pem).
        cert: PathBuf,
    },
}

#[derive(Debug, ClapArgs)]
struct CaArgs {
    /// Directory to write ca.pem and ca.key into. Created if missing.
    #[arg(long, value_name = "DIR", default_value = "./pki")]
    out_dir: PathBuf,
    /// Common name embedded in the CA certificate.
    #[arg(long, default_value = "rusnel-ca")]
    common_name: String,
}

#[derive(Debug, ClapArgs)]
struct ServerCertArgs {
    /// Directory to write the resulting cert + key into.
    #[arg(long, value_name = "DIR", default_value = "./pki")]
    out_dir: PathBuf,
    /// Path to the CA certificate (PEM).
    #[arg(long, value_name = "PATH")]
    ca: PathBuf,
    /// Path to the CA private key (PEM).
    #[arg(long, value_name = "PATH")]
    ca_key: PathBuf,
    /// Common name. Defaults to the first --name SAN if any.
    #[arg(long)]
    common_name: Option<String>,
    /// DNS Subject Alternative Name. May be repeated.
    #[arg(long = "name", value_name = "DNS")]
    names: Vec<String>,
    /// IP Subject Alternative Name. May be repeated.
    #[arg(long = "ip", value_name = "IP")]
    ips: Vec<IpAddr>,
    /// Output filename stem (default `server`).
    #[arg(long, default_value = "server")]
    file_stem: String,
}

#[derive(Debug, ClapArgs)]
struct ClientCertArgs {
    /// Directory to write the resulting cert + key into.
    #[arg(long, value_name = "DIR", default_value = "./pki")]
    out_dir: PathBuf,
    /// Path to the CA certificate (PEM).
    #[arg(long, value_name = "PATH")]
    ca: PathBuf,
    /// Path to the CA private key (PEM).
    #[arg(long, value_name = "PATH")]
    ca_key: PathBuf,
    /// Common name embedded in the client certificate.
    #[arg(long, default_value = "rusnel-client")]
    common_name: String,
    /// Output filename stem (default: matches --common-name).
    #[arg(long)]
    file_stem: Option<String>,
}

/// Resolve the server CLI flags into a [`ServerTlsConfig`]. CLI flags take
/// precedence; if none are set, we try to use any embedded credentials baked
/// in by build.rs. If neither path applies, error with a clear message —
/// honouring the "require explicit" decision: either the operator explicitly
/// chose a mode at runtime, or the build was explicitly configured with
/// embedded creds.
fn resolve_server_tls(
    insecure: bool,
    tls_self_signed: bool,
    tls_state_dir: Option<PathBuf>,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
    tls_ca: Option<PathBuf>,
    embedded: &Materialized,
) -> Result<ServerTlsConfig, String> {
    if insecure {
        return Ok(ServerTlsConfig::Insecure);
    }
    if tls_self_signed {
        let state_dir = match tls_state_dir {
            Some(p) => p,
            None => default_state_dir()?,
        };
        return Ok(ServerTlsConfig::SelfSigned { state_dir });
    }

    // Explicit --tls-cert/--tls-key (clap enforces both-or-neither). If only
    // one of cert/key is present here the user mis-configured something we
    // can't recover from — clap should have caught it.
    if let (Some(cert), Some(key)) = (tls_cert.clone(), tls_key.clone()) {
        return Ok(match tls_ca {
            Some(ca) => ServerTlsConfig::Mtls { cert, key, ca },
            None => ServerTlsConfig::Provided { cert, key },
        });
    }

    // No CLI flags. Fall back to embedded creds.
    if let (Some(cert), Some(key)) = (embedded.server_cert.clone(), embedded.server_key.clone()) {
        info!("using embedded server credentials baked in at build time");
        return Ok(match embedded.ca.clone() {
            Some(ca) => ServerTlsConfig::Mtls { cert, key, ca },
            None => ServerTlsConfig::Provided { cert, key },
        });
    }

    Err(
        "no TLS mode specified. Pass one of --insecure, --tls-self-signed, \
         --tls-cert + --tls-key (with optional --tls-ca for mTLS), or build \
         with RUSNEL_EMBED_SERVER_CERT / RUSNEL_EMBED_SERVER_KEY."
            .into(),
    )
}

/// Default state dir for persisted self-signed certs: `~/.rusnel`.
fn default_state_dir() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|h| h.join(".rusnel"))
        .ok_or_else(|| "could not determine home directory; pass --tls-state-dir explicitly".into())
}

fn resolve_client_tls(
    insecure: bool,
    tls_fingerprint: Option<String>,
    tls_ca: Option<PathBuf>,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
    tls_server_name: Option<String>,
    embedded: &Materialized,
) -> Result<ClientTlsConfig, String> {
    if insecure {
        return Ok(ClientTlsConfig::Insecure);
    }

    let embedded_server_name = || embedded::EMBED_SERVER_NAME.map(|s| s.to_string());

    if let Some(raw) = tls_fingerprint {
        let sha256 = parse_fingerprint(&raw)
            .map_err(|e| format!("invalid --tls-fingerprint value `{raw}`: {e}"))?;
        return Ok(ClientTlsConfig::Fingerprint {
            sha256,
            server_name: tls_server_name.or_else(embedded_server_name),
        });
    }
    if let Some(ca) = tls_ca {
        return Ok(match (tls_cert, tls_key) {
            (Some(cert), Some(key)) => ClientTlsConfig::Mtls {
                ca,
                cert,
                key,
                server_name: tls_server_name.or_else(embedded_server_name),
            },
            _ => ClientTlsConfig::Ca {
                ca,
                server_name: tls_server_name.or_else(embedded_server_name),
            },
        });
    }

    // No CLI flags. Fall back to embedded creds in this priority order:
    //   1. embedded CA + client cert/key  → mTLS
    //   2. embedded CA only               → CA-only verification
    //   3. embedded fingerprint           → fingerprint pinning
    if let Some(ca) = embedded.ca.clone() {
        info!("using embedded client credentials baked in at build time");
        return Ok(
            match (embedded.client_cert.clone(), embedded.client_key.clone()) {
                (Some(cert), Some(key)) => ClientTlsConfig::Mtls {
                    ca,
                    cert,
                    key,
                    server_name: tls_server_name.or_else(embedded_server_name),
                },
                _ => ClientTlsConfig::Ca {
                    ca,
                    server_name: tls_server_name.or_else(embedded_server_name),
                },
            },
        );
    }
    if let Some(fp) = embedded::EMBED_FINGERPRINT {
        info!("using embedded server fingerprint baked in at build time");
        let sha256 = parse_fingerprint(fp).map_err(|e| {
            format!("invalid embedded fingerprint (RUSNEL_EMBED_FINGERPRINT) `{fp}`: {e}")
        })?;
        return Ok(ClientTlsConfig::Fingerprint {
            sha256,
            server_name: tls_server_name.or_else(embedded_server_name),
        });
    }

    Err(
        "no TLS mode specified. Pass one of --insecure, --tls-fingerprint, \
         --tls-ca (with optional --tls-cert + --tls-key for mTLS), or build \
         with RUSNEL_EMBED_CA / RUSNEL_EMBED_FINGERPRINT."
            .into(),
    )
}

fn main() {
    if let Err(e) = rustls::crypto::ring::default_provider().install_default() {
        Args::command()
            .error(
                ErrorKind::Io,
                format!("failed to install rustls crypto provider: {e:?}"),
            )
            .exit();
    }

    let args = Args::parse();

    match args.mode {
        Mode::Server {
            host,
            port,
            allow_reverse,
            allow_socks,
            insecure,
            tls_self_signed,
            tls_state_dir,
            tls_cert,
            tls_key,
            tls_ca,
            congestion,
            max_connections,
            admin_socket,
            no_admin_socket,
            is_verbose,
            is_debug,
        } => {
            set_log_level(is_verbose, is_debug);

            let embedded = match embedded::materialize() {
                Ok(m) => m,
                Err(e) => Args::command()
                    .error(
                        ErrorKind::Io,
                        format!("failed to materialize embedded credentials: {e:#}"),
                    )
                    .exit(),
            };

            let tls = match resolve_server_tls(
                insecure,
                tls_self_signed,
                tls_state_dir,
                tls_cert,
                tls_key,
                tls_ca,
                embedded,
            ) {
                Ok(t) => t,
                Err(msg) => Args::command().error(ErrorKind::InvalidValue, msg).exit(),
            };

            let server_config = ServerConfig {
                host,
                port,
                allow_reverse,
                allow_socks,
                tls,
                congestion: congestion.into(),
                max_connections: if max_connections == 0 {
                    None
                } else {
                    Some(max_connections)
                },
                // Admin API is on by default at `~/.rusnel/admin.sock`
                // — opt out with `--no-admin-socket`, override with
                // `--admin-socket <PATH>`. Clap enforces the
                // mutual-exclusion via `conflicts_with`.
                admin_socket: if no_admin_socket {
                    None
                } else {
                    Some(admin_socket.unwrap_or_else(rusnel::ctl::default_socket_path))
                },
            };
            debug!("Initialized server with config: {:?}", server_config);
            run_server(server_config);
        }
        Mode::Client {
            server,
            remotes,
            insecure,
            tls_fingerprint,
            tls_ca,
            tls_cert,
            tls_key,
            tls_server_name,
            congestion,
            max_retry_count,
            max_retry_interval,
            proxy,
            is_verbose,
            is_debug,
        } => {
            set_log_level(is_verbose, is_debug);

            let embedded = match embedded::materialize() {
                Ok(m) => m,
                Err(e) => Args::command()
                    .error(
                        ErrorKind::Io,
                        format!("failed to materialize embedded credentials: {e:#}"),
                    )
                    .exit(),
            };

            let tls = match resolve_client_tls(
                insecure,
                tls_fingerprint,
                tls_ca,
                tls_cert,
                tls_key,
                tls_server_name,
                embedded,
            ) {
                Ok(t) => t,
                Err(msg) => Args::command().error(ErrorKind::InvalidValue, msg).exit(),
            };

            let max_retries = match max_retries_from_cli(max_retry_count) {
                Ok(v) => v,
                Err(msg) => Args::command().error(ErrorKind::InvalidValue, msg).exit(),
            };
            let reconnect = ReconnectConfig {
                max_retries,
                max_backoff: max_retry_interval,
                ..ReconnectConfig::default()
            };
            let client_config = ClientConfig {
                server,
                remotes,
                tls,
                congestion: congestion.into(),
                reconnect,
                proxy,
            };
            debug!("Initialized client with config: {:?}", client_config);
            run_client(client_config);
        }
        Mode::Ctl {
            socket,
            json,
            action,
        } => {
            // ctl is a one-shot client; minimal logger so error messages
            // surface but we don't pollute the formatted-table output.
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::WARN)
                .with_target(false)
                .without_time()
                .init();
            let socket_path = socket.unwrap_or_else(rusnel::ctl::default_socket_path);
            if let Err(e) = run_ctl(&socket_path, json, action) {
                eprintln!("ctl: {}", rusnel::ctl::flatten_error(e));
                std::process::exit(1);
            }
        }
        Mode::Cert { action } => {
            // Cert generation is a one-shot tool, not a server. Use a minimal
            // INFO logger so file paths are visible without --verbose.
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::INFO)
                .with_target(false)
                .without_time()
                .init();
            if let Err(e) = run_cert(action) {
                Args::command()
                    .error(ErrorKind::Io, format!("{e:#}"))
                    .exit();
            }
        }
    }
}

fn run_ctl(socket: &std::path::Path, json: bool, action: CtlAction) -> anyhow::Result<()> {
    use rusnel::ctl::{self, Format};
    let format = if json { Format::Json } else { Format::Table };
    let rt = tokio::runtime::Runtime::new()?;
    let output = rt.block_on(async {
        match action {
            CtlAction::Server => {
                let payload = ctl::get(socket, "/api/v1/server").await?;
                ctl::render_server(payload, format)
            }
            CtlAction::Clients => {
                let payload = ctl::get(socket, "/api/v1/clients").await?;
                ctl::render_clients(payload, format)
            }
            CtlAction::Client { id } => {
                let payload = ctl::get(socket, &format!("/api/v1/clients/{id}")).await?;
                ctl::render_client_detail(payload, format)
            }
            CtlAction::ClientConns { id } => {
                let payload = ctl::get(socket, &format!("/api/v1/clients/{id}/conns")).await?;
                ctl::render_conns(payload, format)
            }
            CtlAction::Tunnels => {
                let payload = ctl::get(socket, "/api/v1/tunnels").await?;
                ctl::render_tunnels(payload, format)
            }
            CtlAction::Tunnel { id } => {
                let payload = ctl::get(socket, &format!("/api/v1/tunnels/{id}")).await?;
                ctl::render_tunnel_detail(payload, format)
            }
            CtlAction::TunnelConns { id } => {
                let payload = ctl::get(socket, &format!("/api/v1/tunnels/{id}/conns")).await?;
                ctl::render_conns(payload, format)
            }
            CtlAction::Conns => {
                let payload = ctl::get(socket, "/api/v1/conns").await?;
                ctl::render_conns(payload, format)
            }
            CtlAction::History { limit } => {
                let path = match limit {
                    Some(n) => format!("/api/v1/history?limit={n}"),
                    None => "/api/v1/history".to_string(),
                };
                let payload = ctl::get(socket, &path).await?;
                ctl::render_history(payload, format)
            }
        }
    })?;
    print!("{output}");
    if !output.ends_with('\n') {
        println!();
    }
    Ok(())
}

fn run_cert(action: CertAction) -> anyhow::Result<()> {
    match action {
        CertAction::Ca(a) => {
            cert::generate_ca(&a.out_dir, &a.common_name)?;
        }
        CertAction::Server(a) => {
            let cn = a
                .common_name
                .clone()
                .or_else(|| a.names.first().cloned())
                .unwrap_or_else(|| "rusnel-server".to_string());
            cert::generate_server_cert(
                &a.out_dir,
                &a.ca,
                &a.ca_key,
                &cn,
                &a.names,
                &a.ips,
                &a.file_stem,
            )?;
        }
        CertAction::Client(a) => {
            let stem = a.file_stem.clone().unwrap_or_else(|| a.common_name.clone());
            cert::generate_client_cert(&a.out_dir, &a.ca, &a.ca_key, &a.common_name, &stem)?;
        }
        CertAction::Fingerprint { cert } => {
            let fp = cert::print_fingerprint(&cert)?;
            println!("{fp}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

    fn v4(p: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, p as u8)), p)
    }
    fn v6(p: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, p as u16)), p)
    }

    #[test]
    fn interleave_alternates_families_v6_first() {
        // Resolver returned v6 first (typical macOS / RFC 6724 ordering).
        let got = interleave_address_families(vec![v6(1), v6(2), v4(3), v4(4)]);
        assert_eq!(got, vec![v6(1), v4(3), v6(2), v4(4)]);
    }

    #[test]
    fn interleave_alternates_families_v4_first() {
        let got = interleave_address_families(vec![v4(1), v4(2), v6(3), v6(4)]);
        // Both families present → v6 still goes first per resolver default.
        assert_eq!(got, vec![v6(3), v4(1), v6(4), v4(2)]);
    }

    #[test]
    fn interleave_single_family_preserves_order() {
        let got = interleave_address_families(vec![v4(1), v4(2), v4(3)]);
        assert_eq!(got, vec![v4(1), v4(2), v4(3)]);
        let got = interleave_address_families(vec![v6(1), v6(2)]);
        assert_eq!(got, vec![v6(1), v6(2)]);
    }
}

fn set_log_level(is_verbose: bool, is_debug: bool) {
    let log_level = if is_debug {
        tracing::Level::TRACE
    } else if is_verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };
    tracing_subscriber::fmt().with_max_level(log_level).init();

    debug!("log level: {}", log_level);
}
