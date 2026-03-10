use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use std::path::Path;
use std::time::Duration;

/// Generate a self-signed CA certificate and key pair
pub fn generate_ca() -> Result<(String, String)> {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(DnType::CommonName, "devproxy Local CA");
    params
        .distinguished_name
        .push(DnType::OrganizationName, "devproxy");
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.not_before = time::OffsetDateTime::now_utc() - Duration::from_secs(3600);
    params.not_after = time::OffsetDateTime::now_utc() + Duration::from_secs(365 * 24 * 3600 * 10);

    let key_pair = KeyPair::generate().context("failed to generate CA key pair")?;
    let cert = params
        .self_signed(&key_pair)
        .context("failed to self-sign CA certificate")?;

    Ok((cert.pem(), key_pair.serialize_pem()))
}

/// Generate a wildcard TLS certificate signed by the given CA
pub fn generate_wildcard_cert(
    domain: &str,
    ca_cert_pem: &str,
    ca_key_pem: &str,
) -> Result<(String, String)> {
    let ca_key = KeyPair::from_pem(ca_key_pem).context("failed to parse CA key")?;
    let ca_params = CertificateParams::from_ca_cert_pem(ca_cert_pem)
        .context("failed to parse CA cert params")?;
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .context("failed to reconstruct CA cert")?;

    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, format!("*.{domain}"));
    params.subject_alt_names = vec![
        SanType::DnsName(
            format!("*.{domain}")
                .try_into()
                .context("invalid wildcard DNS name")?,
        ),
        SanType::DnsName(domain.to_string().try_into().context("invalid DNS name")?),
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.not_before = time::OffsetDateTime::now_utc() - Duration::from_secs(3600);
    params.not_after = time::OffsetDateTime::now_utc() + Duration::from_secs(365 * 24 * 3600);

    let key_pair = KeyPair::generate().context("failed to generate TLS key pair")?;
    let cert = params
        .signed_by(&key_pair, &ca_cert, &ca_key)
        .context("failed to sign wildcard certificate")?;

    Ok((cert.pem(), key_pair.serialize_pem()))
}

/// Write PEM data to a file, creating parent directories.
/// If `is_key` is true, restricts file permissions to owner-only (0600).
pub fn write_pem(path: &Path, pem: &str) -> Result<()> {
    write_pem_with_mode(path, pem, false)
}

/// Write a private key PEM file with restrictive permissions (0600).
pub fn write_key_pem(path: &Path, pem: &str) -> Result<()> {
    write_pem_with_mode(path, pem, true)
}

fn write_pem_with_mode(path: &Path, pem: &str, is_key: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if is_key {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(pem.as_bytes())?;
    } else {
        std::fs::write(path, pem)?;
    }
    Ok(())
}

/// Load TLS certificate and key into rustls ServerConfig
pub fn load_tls_config(cert_path: &Path, key_path: &Path) -> Result<tokio_rustls::TlsAcceptor> {
    use rustls::ServerConfig;
    use rustls_pemfile::{certs, pkcs8_private_keys};
    use std::io::BufReader;
    use std::sync::Arc;

    let cert_file = std::fs::File::open(cert_path)
        .with_context(|| format!("could not open cert file: {}", cert_path.display()))?;
    let key_file = std::fs::File::open(key_path)
        .with_context(|| format!("could not open key file: {}", key_path.display()))?;

    let certs: Vec<_> = certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .context("could not parse certificates")?;

    let keys: Vec<_> = pkcs8_private_keys(&mut BufReader::new(key_file))
        .collect::<Result<Vec<_>, _>>()
        .context("could not parse private keys")?;

    let key = keys
        .into_iter()
        .next()
        .context("no private key found in key file")?;

    // Ensure a crypto provider is installed
    let _ = rustls::crypto::ring::default_provider().install_default();

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, rustls::pki_types::PrivateKeyDer::Pkcs8(key))
        .context("invalid TLS configuration")?;

    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

/// Return the path to the current user's login keychain on macOS.
#[cfg(target_os = "macos")]
fn login_keychain_path() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join("Library/Keychains/login.keychain-db"))
}

/// Trust the CA certificate in the OS certificate store.
///
/// On macOS, adds to the user's login keychain (no sudo required).
/// On Linux, copies to /usr/local/share/ca-certificates and runs update-ca-certificates.
/// Warns on other platforms where automatic trust is not implemented.
pub fn trust_ca_in_system(ca_cert_path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let keychain = login_keychain_path()?;
        let status = std::process::Command::new("security")
            .args(["add-trusted-cert", "-r", "trustRoot", "-k"])
            .arg(&keychain)
            .arg(ca_cert_path)
            .status()
            .context("failed to run security command")?;

        if !status.success() {
            anyhow::bail!(
                "failed to trust CA cert in login keychain ({})",
                keychain.display()
            );
        }

        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        let dest = Path::new("/usr/local/share/ca-certificates/devproxy-ca.crt");
        std::fs::copy(ca_cert_path, dest)
            .context("failed to copy CA cert. You may need to run with sudo.")?;
        let status = std::process::Command::new("update-ca-certificates")
            .status()
            .context("failed to run update-ca-certificates")?;
        if !status.success() {
            anyhow::bail!("failed to update CA certificates");
        }

        return Ok(());
    }

    #[allow(unreachable_code)]
    {
        eprintln!(
            "warning: automatic CA trust is not supported on this platform. \
             Please manually trust: {}",
            ca_cert_path.display()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_ca_produces_valid_pem() {
        let (cert_pem, key_pem) = generate_ca().unwrap();
        assert!(cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn generate_wildcard_cert_produces_valid_pem() {
        let (ca_cert, ca_key) = generate_ca().unwrap();
        let (cert_pem, key_pem) = generate_wildcard_cert("mysite.dev", &ca_cert, &ca_key).unwrap();
        assert!(cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn login_keychain_path_points_to_login_keychain() {
        let path = super::login_keychain_path().unwrap();
        assert!(path
            .to_string_lossy()
            .ends_with("Library/Keychains/login.keychain-db"));
        assert!(
            path.is_absolute(),
            "path should be absolute: {}",
            path.display()
        );
    }

    #[test]
    fn tls_config_loads_from_generated_certs() {
        let (ca_cert, ca_key) = generate_ca().unwrap();
        let (cert_pem, key_pem) = generate_wildcard_cert("mysite.dev", &ca_cert, &ca_key).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        std::fs::write(&cert_path, &cert_pem).unwrap();
        std::fs::write(&key_path, &key_pem).unwrap();

        let result = load_tls_config(&cert_path, &key_path);
        assert!(result.is_ok(), "TLS config should load: {:?}", result.err());
    }

    /// Functional test: add a CA cert to the login keychain, verify it, then clean up.
    ///
    /// Marked `#[ignore]` because it touches the real login keychain and triggers
    /// a macOS Keychain Access password dialog. Run manually with:
    ///     cargo test -- cert::tests::trust_ca_login_keychain_roundtrip --ignored
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn trust_ca_login_keychain_roundtrip() {
        use std::path::PathBuf;

        /// Guard that removes the test certificate from the login keychain on drop.
        struct KeychainCleanup {
            cert_path: PathBuf,
            keychain: PathBuf,
        }

        impl Drop for KeychainCleanup {
            fn drop(&mut self) {
                // Remove trust setting
                let _ = std::process::Command::new("security")
                    .args(["remove-trusted-cert", "-d"])
                    .arg(&self.cert_path)
                    .status();

                // Remove the certificate itself
                let _ = std::process::Command::new("security")
                    .args(["delete-certificate", "-c", "devproxy Local CA"])
                    .arg(&self.keychain)
                    .status();
            }
        }

        // Generate a fresh CA cert
        let (ca_cert_pem, _ca_key_pem) = generate_ca().unwrap();

        // Write to a temp file
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("test-ca.pem");
        std::fs::write(&cert_path, &ca_cert_pem).unwrap();

        // Resolve login keychain path
        let home = dirs::home_dir().expect("could not determine home directory");
        let keychain = home.join("Library/Keychains/login.keychain-db");

        // Set up cleanup guard before calling trust (runs even on panic)
        let _cleanup = KeychainCleanup {
            cert_path: cert_path.clone(),
            keychain: keychain.clone(),
        };

        // Trust the cert in the login keychain
        super::trust_ca_in_system(&cert_path).unwrap();

        // Verify the cert is findable in the login keychain
        let output = std::process::Command::new("security")
            .args(["find-certificate", "-c", "devproxy Local CA", "-a"])
            .arg(&keychain)
            .output()
            .expect("failed to run security find-certificate");

        assert!(
            output.status.success(),
            "certificate should be present in login keychain after trust_ca_in_system"
        );
    }
}
