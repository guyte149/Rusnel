//! TOML config-file schema for `rusnel server --config` and
//! `rusnel client --config`.
//!
//! The schema mirrors the CLI flags one-for-one (snake_case) but every
//! field is optional. CLI flags **override** any value in the file; the
//! file overrides clap defaults. Detection of "explicitly provided on
//! the CLI" is done via `clap::ArgMatches::value_source` in `main.rs`
//! (`merge_*_from_file`).
//!
//! Unknown keys fail the parse so typos surface immediately rather
//! than silently no-oping.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use serde::Deserialize;

/// Full file schema: at least one of `[server]` / `[client]` is
/// expected, but neither is required (an empty file is valid and is
/// equivalent to passing no `--config` at all).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(default)]
    pub server: Option<ServerSection>,
    #[serde(default)]
    pub client: Option<ClientSection>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    pub host: Option<std::net::IpAddr>,
    pub port: Option<u16>,
    pub allow_reverse: Option<bool>,
    pub allow_socks: Option<bool>,
    pub insecure: Option<bool>,
    pub tls_self_signed: Option<bool>,
    pub tls_state_dir: Option<PathBuf>,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub tls_ca: Option<PathBuf>,
    pub congestion: Option<CongestionStr>,
    pub max_connections: Option<usize>,
    pub admin_socket: Option<PathBuf>,
    pub no_admin_socket: Option<bool>,
    pub log_format: Option<LogFormatStr>,
    pub verbose: Option<bool>,
    pub debug: Option<bool>,
    pub quiet: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientSection {
    pub server: Option<String>,
    pub remotes: Option<Vec<String>>,
    pub insecure: Option<bool>,
    pub tls_fingerprint: Option<String>,
    pub tls_ca: Option<PathBuf>,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub tls_server_name: Option<String>,
    pub congestion: Option<CongestionStr>,
    pub max_retry_count: Option<i64>,
    /// Cap on the exponential reconnect backoff, in seconds.
    pub max_retry_interval: Option<u64>,
    pub proxy: Option<String>,
    pub log_format: Option<LogFormatStr>,
    pub verbose: Option<bool>,
    pub debug: Option<bool>,
    pub quiet: Option<bool>,
}

/// Mirror of the clap `CongestionArg` enum. Kept as a separate type so
/// users get a typed parse error pointing at the offending TOML line
/// instead of the generic "expected one of …" you'd get from a
/// `String` field.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CongestionStr {
    Cubic,
    Bbr,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormatStr {
    Compact,
    Json,
}

/// Read a TOML config file from disk. Returns a wrapped error with the
/// path attached so the operator can tell which file is malformed when
/// chaining through `--config`.
pub fn load(path: &Path) -> anyhow::Result<ConfigFile> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file `{}`", path.display()))?;
    toml::from_str::<ConfigFile>(&raw)
        .map_err(|e| anyhow!("invalid config file `{}`:\n{e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_server_section() {
        let toml = r#"
[server]
host = "127.0.0.1"
port = 9090
allow_reverse = true
allow_socks = true
tls_self_signed = true
congestion = "bbr"
max_connections = 100
log_format = "json"
verbose = true
"#;
        let cfg: ConfigFile = toml::from_str(toml).expect("parse");
        let s = cfg.server.expect("server section");
        assert_eq!(s.port, Some(9090));
        assert!(matches!(s.congestion, Some(CongestionStr::Bbr)));
        assert!(matches!(s.log_format, Some(LogFormatStr::Json)));
        assert_eq!(s.allow_reverse, Some(true));
        assert_eq!(s.verbose, Some(true));
    }

    #[test]
    fn parses_full_client_section() {
        let toml = r#"
[client]
server = "1.2.3.4:8080"
remotes = ["R:2222:localhost:22", "1.1.1.1:53/udp"]
tls_fingerprint = "sha256:abcd"
max_retry_count = -1
max_retry_interval = 60
proxy = "socks5://user:pass@proxy:1080"
"#;
        let cfg: ConfigFile = toml::from_str(toml).expect("parse");
        let c = cfg.client.expect("client section");
        assert_eq!(c.server.as_deref(), Some("1.2.3.4:8080"));
        assert_eq!(
            c.remotes,
            Some(vec!["R:2222:localhost:22".into(), "1.1.1.1:53/udp".into()])
        );
        assert_eq!(c.max_retry_count, Some(-1));
        assert_eq!(c.max_retry_interval, Some(60));
    }

    #[test]
    fn empty_file_is_valid() {
        let cfg: ConfigFile = toml::from_str("").expect("empty parse");
        assert!(cfg.server.is_none());
        assert!(cfg.client.is_none());
    }

    #[test]
    fn unknown_top_level_key_rejected() {
        let toml = r#"
[srever]   # typo
port = 9090
"#;
        let err = toml::from_str::<ConfigFile>(toml).expect_err("should reject typo");
        assert!(format!("{err}").contains("srever"), "got: {err}");
    }

    #[test]
    fn unknown_field_in_section_rejected() {
        let toml = r#"
[server]
prot = 9090   # typo
"#;
        let err = toml::from_str::<ConfigFile>(toml).expect_err("should reject typo");
        assert!(format!("{err}").contains("prot"), "got: {err}");
    }

    #[test]
    fn invalid_congestion_value_rejected() {
        let toml = r#"
[server]
congestion = "reno"
"#;
        let err = toml::from_str::<ConfigFile>(toml).expect_err("should reject");
        assert!(format!("{err}").contains("reno"), "got: {err}");
    }
}
