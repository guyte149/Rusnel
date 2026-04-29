use clap::crate_version;
use clap::error::ErrorKind;
use clap::{ArgGroup, Args as ClapArgs, CommandFactory, Parser, Subcommand};
use rusnel::cert;
use rusnel::common::remote::RemoteRequest;
use rusnel::common::tls::{parse_fingerprint, ClientTlsConfig, ServerTlsConfig};
use rusnel::{run_client, run_server, ClientConfig, ServerConfig};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::str::FromStr;
use tracing::debug;

/// Resolve `host:port` to a `SocketAddr`. Used as a clap `value_parser` so
/// resolution failures surface as `clap::Error` (consistent formatting +
/// exit code 2) instead of a panic (#20 §4).
fn parse_server_addr(s: &str) -> Result<SocketAddr, String> {
    s.to_socket_addrs()
        .map_err(|e| format!("failed to resolve server address `{s}`: {e}"))?
        .next()
        .ok_or_else(|| format!("no addresses found for server `{s}`"))
}

/// Parse a remote spec via `RemoteRequest::from_str`, surfacing parse errors
/// as `clap` errors instead of `eprintln! + process::exit` (#20 §4 + §5).
fn parse_remote(s: &str) -> Result<RemoteRequest, String> {
    RemoteRequest::from_str(s).map_err(|e| format!("invalid remote `{s}`: {e}"))
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
    #[command(group(
        ArgGroup::new("server_tls_mode")
            .required(true)
            .args(["insecure", "tls_self_signed", "tls_cert"]),
    ))]
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

        /// Disable all TLS authentication. Uses an ephemeral self-signed
        /// certificate and accepts any client. MITM-vulnerable; for testing
        /// only.
        #[arg(long, default_value_t = false)]
        insecure: bool,

        /// Use a self-signed certificate persisted under --tls-state-dir
        /// (default: ~/.rusnel). Generated on first run; reused on subsequent
        /// runs so the fingerprint stays stable.
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

        /// enable verbose logging
        #[arg(short('v'), long("verbose"), default_value_t = false)]
        is_verbose: bool,

        /// enable debug logging
        #[arg(long("debug"), default_value_t = false)]
        is_debug: bool,
    },
    /// run Rusnel in client mode
    #[command(group(
        ArgGroup::new("client_tls_mode")
            .required(true)
            .args(["insecure", "tls_fingerprint", "tls_ca"]),
    ))]
    #[allow(clippy::too_many_arguments)]
    Client {
        /// defines the Rusnel server address (in form of host:port)
        #[arg(value_parser = parse_server_addr)]
        server: SocketAddr,

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
    
    When the Rusnel server has --allow-reverse enabled, remotes can be prefixed with R to denote that they are reversed.

    Remotes can specify "socks" in place of remote-host and remote-port.
    The default local host and port for a "socks" remote is 127.0.0.1:1080.
        "#)]
        remotes: Vec<RemoteRequest>,

        /// Disable server certificate verification. MITM-vulnerable; for
        /// testing only.
        #[arg(long, default_value_t = false)]
        insecure: bool,

        /// Pin the server's leaf certificate by SHA-256 fingerprint. Accepts
        /// `sha256:<hex>`, bare hex, or colon-separated hex. The expected
        /// value is logged by the server at startup as
        /// `server cert fingerprint: sha256:<hex>`.
        #[arg(long, value_name = "SHA256", conflicts_with_all = ["insecure", "tls_ca"])]
        tls_fingerprint: Option<String>,

        /// Verify the server certificate against this CA bundle. Use alone
        /// for server-auth-only TLS, or pair with --tls-cert/--tls-key for
        /// full mTLS.
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

        /// Override the SNI / server name sent during the TLS handshake. With
        /// --tls-ca, this name must match a SAN in the server certificate.
        /// With --tls-fingerprint, the value is sent as SNI but ignored
        /// during verification.
        #[arg(long, value_name = "NAME")]
        tls_server_name: Option<String>,

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

/// Resolve the server CLI flags into a [`ServerTlsConfig`]. Clap's `ArgGroup`
/// already guarantees exactly one mode flag is set, so this is just a mapping.
fn resolve_server_tls(
    insecure: bool,
    tls_self_signed: bool,
    tls_state_dir: Option<PathBuf>,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
    tls_ca: Option<PathBuf>,
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
    match (tls_cert, tls_key, tls_ca) {
        (Some(cert), Some(key), Some(ca)) => Ok(ServerTlsConfig::Mtls { cert, key, ca }),
        (Some(cert), Some(key), None) => Ok(ServerTlsConfig::Provided { cert, key }),
        // Unreachable in practice: clap's `requires` enforces the pairing and
        // the ArgGroup guarantees one mode is selected.
        _ => Err("internal error: TLS mode flags not validated correctly".into()),
    }
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
) -> Result<ClientTlsConfig, String> {
    if insecure {
        return Ok(ClientTlsConfig::Insecure);
    }
    if let Some(raw) = tls_fingerprint {
        let sha256 = parse_fingerprint(&raw)
            .map_err(|e| format!("invalid --tls-fingerprint value `{raw}`: {e}"))?;
        return Ok(ClientTlsConfig::Fingerprint {
            sha256,
            server_name: tls_server_name,
        });
    }
    if let Some(ca) = tls_ca {
        return Ok(match (tls_cert, tls_key) {
            (Some(cert), Some(key)) => ClientTlsConfig::Mtls {
                ca,
                cert,
                key,
                server_name: tls_server_name,
            },
            _ => ClientTlsConfig::Ca {
                ca,
                server_name: tls_server_name,
            },
        });
    }
    Err("internal error: client TLS mode flags not validated correctly".into())
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
            insecure,
            tls_self_signed,
            tls_state_dir,
            tls_cert,
            tls_key,
            tls_ca,
            is_verbose,
            is_debug,
        } => {
            set_log_level(is_verbose, is_debug);

            let tls = match resolve_server_tls(
                insecure,
                tls_self_signed,
                tls_state_dir,
                tls_cert,
                tls_key,
                tls_ca,
            ) {
                Ok(t) => t,
                Err(msg) => Args::command().error(ErrorKind::InvalidValue, msg).exit(),
            };

            let server_config = ServerConfig {
                host,
                port,
                allow_reverse,
                tls,
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
            is_verbose,
            is_debug,
        } => {
            set_log_level(is_verbose, is_debug);

            let tls = match resolve_client_tls(
                insecure,
                tls_fingerprint,
                tls_ca,
                tls_cert,
                tls_key,
                tls_server_name,
            ) {
                Ok(t) => t,
                Err(msg) => Args::command().error(ErrorKind::InvalidValue, msg).exit(),
            };

            let client_config = ClientConfig {
                server,
                remotes,
                tls,
            };
            debug!("Initialized client with config: {:?}", client_config);
            run_client(client_config);
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
