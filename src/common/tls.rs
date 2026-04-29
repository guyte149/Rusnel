//! TLS configuration types shared by the server and client.
//!
//! These enums are the single source of truth for how peer authentication is
//! performed. `quic.rs` consumes them to build the underlying `rustls`
//! configurations.
//!
//! Variants beyond [`ServerTlsConfig::Insecure`] / [`ClientTlsConfig::Insecure`]
//! are scaffolded here so that follow-up PRs that add fingerprint pinning and
//! full mTLS can plug in without churning the public API again. They are not
//! constructed yet — `quic.rs` will return an error if a non-`Insecure`
//! variant is passed in until those PRs land.

use std::path::PathBuf;

use rustls::pki_types::CertificateDer;
use sha2::{Digest, Sha256};

/// How the server presents itself and (optionally) authenticates clients.
#[derive(Debug, Clone)]
pub enum ServerTlsConfig {
    /// Generate a fresh, ephemeral self-signed certificate at startup and
    /// accept any client. Equivalent to the pre-mTLS behaviour. MITM-vulnerable
    /// — intended for tests and explicit `--insecure` usage.
    Insecure,

    /// Use a self-signed certificate persisted under `state_dir`, generating
    /// it on first run. Accepts any client. Stable fingerprint across restarts
    /// so clients can pin via `--tls-fingerprint`.
    SelfSigned { state_dir: PathBuf },

    /// Use the provided cert/key pair. Accepts any client.
    Provided { cert: PathBuf, key: PathBuf },

    /// Full mTLS: present `cert`/`key` and require the peer to present a
    /// client cert chained to `ca`.
    Mtls {
        cert: PathBuf,
        key: PathBuf,
        ca: PathBuf,
    },
}

/// How the client verifies the server and (optionally) authenticates itself.
#[derive(Debug, Clone)]
pub enum ClientTlsConfig {
    /// Skip server verification entirely. Equivalent to the pre-mTLS behaviour.
    /// MITM-vulnerable — intended for tests and explicit `--insecure` usage.
    Insecure,

    /// Pin the server's leaf certificate by SHA-256 of its DER encoding.
    /// `server_name` overrides the SNI sent during the TLS handshake; when
    /// `None` the value passed to `Endpoint::connect` is used.
    Fingerprint {
        sha256: [u8; 32],
        server_name: Option<String>,
    },

    /// Verify the server certificate against the given CA. Server-auth only.
    Ca {
        ca: PathBuf,
        server_name: Option<String>,
    },

    /// Full mTLS: verify the server with `ca` and present `cert`/`key`.
    Mtls {
        ca: PathBuf,
        cert: PathBuf,
        key: PathBuf,
        server_name: Option<String>,
    },
}

/// SHA-256 of the DER encoding of a certificate. This matches what tools like
/// `openssl x509 -fingerprint -sha256` and Chisel's `--fingerprint` produce
/// (after `:` stripping / case folding).
pub fn cert_sha256(cert: &CertificateDer<'_>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(cert.as_ref());
    hasher.finalize().into()
}

/// Format a SHA-256 digest as `sha256:<lowercase-hex>`.
pub fn format_fingerprint(digest: &[u8; 32]) -> String {
    let mut s = String::with_capacity(7 + 64);
    s.push_str("sha256:");
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Parse a user-provided fingerprint string. Accepts:
///   * `sha256:<hex>` (case-insensitive)
///   * bare `<hex>` (case-insensitive)
///   * either of the above with `:` separators between bytes (openssl style)
///
/// Returns the 32-byte digest on success.
pub fn parse_fingerprint(s: &str) -> Result<[u8; 32], String> {
    let trimmed = s.trim();
    let body = trimmed
        .strip_prefix("sha256:")
        .or_else(|| trimmed.strip_prefix("SHA256:"))
        .unwrap_or(trimmed);
    let cleaned: String = body.chars().filter(|c| *c != ':').collect();
    if cleaned.len() != 64 {
        return Err(format!(
            "expected 64 hex chars (32 bytes) in fingerprint, got {}",
            cleaned.len()
        ));
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let chunk = &cleaned[i * 2..i * 2 + 2];
        *byte = u8::from_str_radix(chunk, 16)
            .map_err(|e| format!("invalid hex `{chunk}` at byte {i}: {e}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_roundtrip() {
        let digest = [0xabu8; 32];
        let s = format_fingerprint(&digest);
        assert_eq!(s, format!("sha256:{}", "ab".repeat(32)));
        assert_eq!(parse_fingerprint(&s).unwrap(), digest);
    }

    #[test]
    fn fingerprint_accepts_bare_hex() {
        let digest = [0x12u8; 32];
        let bare = "12".repeat(32);
        assert_eq!(parse_fingerprint(&bare).unwrap(), digest);
    }

    #[test]
    fn fingerprint_accepts_colon_separators() {
        let digest = [0x42u8; 32];
        let with_colons = std::iter::repeat_n("42", 32).collect::<Vec<_>>().join(":");
        assert_eq!(parse_fingerprint(&with_colons).unwrap(), digest);
    }

    #[test]
    fn fingerprint_rejects_wrong_length() {
        assert!(parse_fingerprint("sha256:deadbeef").is_err());
    }

    #[test]
    fn fingerprint_rejects_non_hex() {
        let s = format!("sha256:{}", "zz".repeat(32));
        assert!(parse_fingerprint(&s).is_err());
    }
}
