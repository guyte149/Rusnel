//! Build-time embedded credentials.
//!
//! `build.rs` writes a generated `embedded.rs` into `$OUT_DIR` containing
//! optional `EMBED_*` constants. When the corresponding env var was set at
//! build time, the constant is `Some(...)` and contains the file bytes / the
//! literal string. When unset, it's `None` and the binary behaves exactly as
//! a non-embedded build.
//!
//! At runtime, [`materialize`] writes any embedded byte payloads into a
//! process-lifetime tempdir and returns paths the existing TLS-config code
//! can consume. We deliberately avoid reworking the [`crate::common::tls`]
//! types to accept in-memory bytes — embedded creds are an ergonomics layer
//! around the same `Provided` / `Mtls` paths.

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Context, Result};

/// Are any embedded credentials baked into this binary?
pub fn any_embedded() -> bool {
    EMBED_CA.is_some()
        || EMBED_SERVER_CERT.is_some()
        || EMBED_SERVER_KEY.is_some()
        || EMBED_CLIENT_CERT.is_some()
        || EMBED_CLIENT_KEY.is_some()
        || EMBED_SERVER_ADDR.is_some()
        || EMBED_FINGERPRINT.is_some()
        || EMBED_SERVER_NAME.is_some()
}

/// Resolved on-disk locations of any embedded credentials. Each field is
/// `None` if the corresponding constant was not embedded at build time.
#[derive(Debug, Default, Clone)]
pub struct Materialized {
    pub ca: Option<PathBuf>,
    pub server_cert: Option<PathBuf>,
    pub server_key: Option<PathBuf>,
    pub client_cert: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
}

/// Materialize embedded byte payloads into a process-lifetime tempdir under
/// `$TMPDIR/rusnel-embed-<pid>/`. Returns the resolved paths. Cheap enough to
/// call unconditionally on startup; if nothing is embedded, the returned
/// struct is all-`None` and no directory is created.
pub fn materialize() -> Result<&'static Materialized> {
    static CELL: OnceLock<Materialized> = OnceLock::new();
    if let Some(m) = CELL.get() {
        return Ok(m);
    }
    let m = materialize_uncached()?;
    Ok(CELL.get_or_init(|| m))
}

fn materialize_uncached() -> Result<Materialized> {
    if !any_byte_embedded() {
        return Ok(Materialized::default());
    }

    let dir = std::env::temp_dir().join(format!("rusnel-embed-{}", std::process::id()));
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create embed tempdir {}", dir.display()))?;

    Ok(Materialized {
        ca: write_if_some(&dir, "ca.pem", EMBED_CA, false)?,
        server_cert: write_if_some(&dir, "server.pem", EMBED_SERVER_CERT, false)?,
        server_key: write_if_some(&dir, "server.key", EMBED_SERVER_KEY, true)?,
        client_cert: write_if_some(&dir, "client.pem", EMBED_CLIENT_CERT, false)?,
        client_key: write_if_some(&dir, "client.key", EMBED_CLIENT_KEY, true)?,
    })
}

fn any_byte_embedded() -> bool {
    EMBED_CA.is_some()
        || EMBED_SERVER_CERT.is_some()
        || EMBED_SERVER_KEY.is_some()
        || EMBED_CLIENT_CERT.is_some()
        || EMBED_CLIENT_KEY.is_some()
}

fn write_if_some(
    dir: &std::path::Path,
    name: &str,
    bytes: Option<&[u8]>,
    secret: bool,
) -> Result<Option<PathBuf>> {
    let Some(bytes) = bytes else { return Ok(None) };
    let path = dir.join(name);
    fs::write(&path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
    if secret {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&path, perms)?;
        }
    }
    Ok(Some(path))
}
