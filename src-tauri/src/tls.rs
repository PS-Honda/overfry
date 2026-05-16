use std::path::Path;

use rcgen::{BasicConstraints, Certificate, CertificateParams, DnType, IsCa, KeyPair};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer},
    ServerConfig,
};

use crate::error::AppError;

// File names in app_data_dir
const CA_CERT_FILE: &str = "overfry-ca.pem";
const CA_KEY_FILE: &str = "overfry-ca-key.pem";
const LEAF_CERT_FILE: &str = "overfry-cert.pem";
const LEAF_KEY_FILE: &str = "overfry-key.pem";

pub struct TlsCerts {
    pub leaf_cert_pem: String,
    pub leaf_key_pem: String,
}

/// Generate a local CA certificate and key pair.
pub fn generate_ca() -> Result<(Certificate, KeyPair), AppError> {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(DnType::CommonName, "Overfry Local CA");
    params
        .distinguished_name
        .push(DnType::OrganizationName, "Overfry");
    let key = KeyPair::generate().map_err(|e| AppError::Other(e.to_string()))?;
    let cert = params
        .self_signed(&key)
        .map_err(|e| AppError::Other(e.to_string()))?;
    Ok((cert, key))
}

/// Generate a leaf certificate for 127.0.0.1 / localhost, signed by the given CA.
pub fn generate_leaf(ca_cert: &Certificate, ca_key: &KeyPair) -> Result<(String, String), AppError> {
    let mut params =
        CertificateParams::new(vec!["127.0.0.1".to_string(), "localhost".to_string()])
            .map_err(|e| AppError::Other(e.to_string()))?;
    params.is_ca = IsCa::NoCa;
    params
        .distinguished_name
        .push(DnType::CommonName, "Overfry MCP Server");
    let key = KeyPair::generate().map_err(|e| AppError::Other(e.to_string()))?;
    let cert = params
        .signed_by(&key, ca_cert, ca_key)
        .map_err(|e| AppError::Other(e.to_string()))?;
    Ok((cert.pem(), key.serialize_pem()))
}

/// Persist CA + leaf certs to app data directory.
pub fn persist_certs(
    data_dir: &Path,
    ca_cert_pem: &str,
    ca_key_pem: &str,
    leaf_cert_pem: &str,
    leaf_key_pem: &str,
) -> Result<(), AppError> {
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(data_dir.join(CA_CERT_FILE), ca_cert_pem)?;
    std::fs::write(data_dir.join(CA_KEY_FILE), ca_key_pem)?;
    std::fs::write(data_dir.join(LEAF_CERT_FILE), leaf_cert_pem)?;
    std::fs::write(data_dir.join(LEAF_KEY_FILE), leaf_key_pem)?;
    Ok(())
}

/// Load existing leaf certs from app data dir. Returns None if missing.
pub fn load_certs(data_dir: &Path) -> Option<TlsCerts> {
    let leaf_cert = std::fs::read_to_string(data_dir.join(LEAF_CERT_FILE)).ok()?;
    let leaf_key = std::fs::read_to_string(data_dir.join(LEAF_KEY_FILE)).ok()?;
    // Also verify CA file exists
    if !data_dir.join(CA_CERT_FILE).exists() {
        return None;
    }
    Some(TlsCerts {
        leaf_cert_pem: leaf_cert,
        leaf_key_pem: leaf_key,
    })
}

/// Install the CA cert to the OS trust store. Returns Ok(()) on success.
#[cfg(target_os = "windows")]
pub fn install_ca(data_dir: &Path) -> Result<(), AppError> {
    let ca_path = data_dir.join(CA_CERT_FILE);
    let out = std::process::Command::new("certutil")
        .args([
            "-addstore",
            "-user",
            "Root",
            ca_path.to_str().unwrap(),
        ])
        .output()
        .map_err(|e| AppError::Other(format!("certutil failed: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(AppError::Other(format!("certutil error: {stderr}")));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn install_ca(data_dir: &Path) -> Result<(), AppError> {
    let ca_path = data_dir.join(CA_CERT_FILE);
    let home = std::env::var("HOME").unwrap_or_default();
    let out = std::process::Command::new("security")
        .args([
            "add-trusted-cert",
            "-d",
            "-r",
            "trustRoot",
            "-k",
            &format!("{home}/Library/Keychains/login.keychain-db"),
            ca_path.to_str().unwrap(),
        ])
        .output()
        .map_err(|e| AppError::Other(format!("security command failed: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(AppError::Other(format!(
            "security add-trusted-cert error: {stderr}"
        )));
    }
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn install_ca(_data_dir: &Path) -> Result<(), AppError> {
    Err(AppError::Other(
        "CA installation not supported on this platform".to_string(),
    ))
}

/// Build rustls ServerConfig from PEM strings.
pub fn make_tls_config(cert_pem: &str, key_pem: &str) -> Result<ServerConfig, AppError> {
    let cert_der: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut cert_pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                AppError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            })?;

    let key_der: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut key_pem.as_bytes())
            .map_err(|e| {
                AppError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            })?
            .ok_or_else(|| {
                AppError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "no private key",
                ))
            })?;

    ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_der, key_der)
        .map_err(|e| AppError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
}
