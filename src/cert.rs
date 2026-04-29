//! Certificate generation helpers backing the `rusnel cert` subcommand.
//!
//! All output is PEM. ECDSA P-256 is used for new keys (smaller than RSA,
//! widely supported, and what `rcgen::KeyPair::generate()` produces by
//! default). Each helper writes to disk (key files mode 0600 on unix) and
//! returns the produced paths for logging.

use std::fs;
use std::io::BufReader;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose, SanType};
use rustls::pki_types::CertificateDer;
use tracing::info;

use crate::common::tls::{cert_sha256, format_fingerprint};

/// Outputs from a successful generation.
#[derive(Debug)]
pub struct CertOutput {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

/// Generate a self-signed certificate authority. The resulting cert is marked
/// `BasicConstraints::CA` and is suitable for signing leaf certs via
/// [`generate_server_cert`] and [`generate_client_cert`].
pub fn generate_ca(out_dir: &Path, common_name: &str) -> Result<CertOutput> {
    fs::create_dir_all(out_dir)
        .with_context(|| format!("failed to create {}", out_dir.display()))?;

    let mut params = CertificateParams::new(vec![common_name.to_string()])
        .context("failed to build CA params")?;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let key_pair = KeyPair::generate().context("failed to generate CA key pair")?;
    let cert = params
        .self_signed(&key_pair)
        .context("failed to self-sign CA cert")?;

    let cert_path = out_dir.join("ca.pem");
    let key_path = out_dir.join("ca.key");
    write_pem(&cert_path, cert.pem().as_bytes())?;
    write_secret_pem(&key_path, key_pair.serialize_pem().as_bytes())?;
    info!("wrote {} and {}", cert_path.display(), key_path.display());
    Ok(CertOutput {
        cert_path,
        key_path,
    })
}

/// Generate a server cert signed by the CA. At least one DNS SAN or IP SAN
/// must be provided so clients can verify the cert against the address they
/// connect to.
pub fn generate_server_cert(
    out_dir: &Path,
    ca_cert_path: &Path,
    ca_key_path: &Path,
    common_name: &str,
    dns_sans: &[String],
    ip_sans: &[IpAddr],
    file_stem: &str,
) -> Result<CertOutput> {
    if dns_sans.is_empty() && ip_sans.is_empty() {
        return Err(anyhow!(
            "server certs require at least one --name or --ip SAN; \
             without it clients won't be able to verify the connection"
        ));
    }
    fs::create_dir_all(out_dir)
        .with_context(|| format!("failed to create {}", out_dir.display()))?;

    let mut params = CertificateParams::new(vec![common_name.to_string()])
        .context("failed to build server cert params")?;
    for dns in dns_sans {
        params.subject_alt_names.push(SanType::DnsName(
            dns.clone()
                .try_into()
                .with_context(|| format!("invalid DNS SAN `{dns}`"))?,
        ));
    }
    for ip in ip_sans {
        params.subject_alt_names.push(SanType::IpAddress(*ip));
    }
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];

    let issuer = load_signing_ca(ca_cert_path, ca_key_path)?;
    let key_pair = KeyPair::generate().context("failed to generate server key pair")?;
    let cert = params
        .signed_by(&key_pair, &issuer)
        .context("failed to sign server cert")?;

    let cert_path = out_dir.join(format!("{file_stem}.pem"));
    let key_path = out_dir.join(format!("{file_stem}.key"));
    write_pem(&cert_path, cert.pem().as_bytes())?;
    write_secret_pem(&key_path, key_pair.serialize_pem().as_bytes())?;
    info!("wrote {} and {}", cert_path.display(), key_path.display());
    Ok(CertOutput {
        cert_path,
        key_path,
    })
}

/// Generate a client cert signed by the CA. Client certs don't need SANs —
/// rustls's `WebPkiClientVerifier` only checks chain + key usage.
pub fn generate_client_cert(
    out_dir: &Path,
    ca_cert_path: &Path,
    ca_key_path: &Path,
    common_name: &str,
    file_stem: &str,
) -> Result<CertOutput> {
    fs::create_dir_all(out_dir)
        .with_context(|| format!("failed to create {}", out_dir.display()))?;

    let mut params = CertificateParams::new(vec![common_name.to_string()])
        .context("failed to build client cert params")?;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];

    let issuer = load_signing_ca(ca_cert_path, ca_key_path)?;
    let key_pair = KeyPair::generate().context("failed to generate client key pair")?;
    let cert = params
        .signed_by(&key_pair, &issuer)
        .context("failed to sign client cert")?;

    let cert_path = out_dir.join(format!("{file_stem}.pem"));
    let key_path = out_dir.join(format!("{file_stem}.key"));
    write_pem(&cert_path, cert.pem().as_bytes())?;
    write_secret_pem(&key_path, key_pair.serialize_pem().as_bytes())?;
    info!("wrote {} and {}", cert_path.display(), key_path.display());
    Ok(CertOutput {
        cert_path,
        key_path,
    })
}

/// Compute and print the SHA-256 fingerprint of the leaf cert in `path`. This
/// is what `--tls-fingerprint` expects.
pub fn print_fingerprint(path: &Path) -> Result<String> {
    let pem =
        fs::read(path).with_context(|| format!("failed to read cert file {}", path.display()))?;
    let mut reader = BufReader::new(pem.as_slice());
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("failed to parse PEM certs in {}", path.display()))?;
    let leaf = certs
        .first()
        .ok_or_else(|| anyhow!("no certificates found in {}", path.display()))?;
    Ok(format_fingerprint(&cert_sha256(leaf)))
}

/// Load a CA cert + key pair into an [`Issuer`] so it can sign new leaf certs.
/// In rcgen 0.14 the signing-side params (DN, key id method, key usages) are
/// encapsulated in `Issuer` rather than being threaded through `signed_by`.
fn load_signing_ca(cert_path: &Path, key_path: &Path) -> Result<Issuer<'static, KeyPair>> {
    let cert_pem = fs::read_to_string(cert_path)
        .with_context(|| format!("failed to read CA cert {}", cert_path.display()))?;
    let key_pem = fs::read_to_string(key_path)
        .with_context(|| format!("failed to read CA key {}", key_path.display()))?;

    let key_pair = KeyPair::from_pem(&key_pem).context("failed to parse CA key as PEM")?;
    Issuer::from_ca_cert_pem(&cert_pem, key_pair).context("failed to parse CA cert as PEM")
}

fn write_pem(path: &Path, contents: &[u8]) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))
}

fn write_secret_pem(path: &Path, contents: &[u8]) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn scratch() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rusnel-cert-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn ca_then_server_and_client_roundtrip() {
        let dir = scratch();
        let ca = generate_ca(&dir, "test-ca").unwrap();

        let srv = generate_server_cert(
            &dir,
            &ca.cert_path,
            &ca.key_path,
            "localhost",
            &["localhost".into()],
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            "server",
        )
        .unwrap();
        assert!(srv.cert_path.exists());
        assert!(srv.key_path.exists());

        let cli =
            generate_client_cert(&dir, &ca.cert_path, &ca.key_path, "alice", "alice").unwrap();
        assert!(cli.cert_path.exists());
        assert!(cli.key_path.exists());

        let fp = print_fingerprint(&srv.cert_path).unwrap();
        assert!(fp.starts_with("sha256:"));
        assert_eq!(fp.len(), 7 + 64);
    }

    #[test]
    fn server_cert_requires_at_least_one_san() {
        let dir = scratch();
        let ca = generate_ca(&dir, "test-ca").unwrap();
        let err = generate_server_cert(
            &dir,
            &ca.cert_path,
            &ca.key_path,
            "no-sans",
            &[],
            &[],
            "server",
        )
        .unwrap_err();
        assert!(err.to_string().contains("--name"));
    }
}
