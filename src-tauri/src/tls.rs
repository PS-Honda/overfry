use std::path::Path;

use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use tokio_rustls::rustls::ServerConfig;
use crate::error::AppError;

const CERT_FILE: &str = "overfry-cert.pem";
const KEY_FILE:  &str = "overfry-key.pem";

/// Returns (cert_pem, key_pem) — generates if missing.
pub fn ensure_cert(data_dir: &Path) -> Result<(String, String), AppError> {
    let cert_path = data_dir.join(CERT_FILE);
    let key_path  = data_dir.join(KEY_FILE);

    if cert_path.exists() && key_path.exists() {
        let cert = std::fs::read_to_string(&cert_path)?;
        let key  = std::fs::read_to_string(&key_path)?;
        return Ok((cert, key));
    }

    std::fs::create_dir_all(data_dir)?;

    let mut params = CertificateParams::new(vec![
        "127.0.0.1".to_string(),
        "localhost".to_string(),
    ]).map_err(|e| AppError::Other(format!("cert params error: {e}")))?;

    let mut dn = DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, "Overfry Local");
    params.distinguished_name = dn;

    let key_pair = KeyPair::generate()
        .map_err(|e| AppError::Other(format!("key generation failed: {e}")))?;
    let cert = params.self_signed(&key_pair)
        .map_err(|e| AppError::Other(format!("cert self-sign failed: {e}")))?;

    let cert_pem = cert.pem();
    let key_pem  = key_pair.serialize_pem();

    std::fs::write(&cert_path, &cert_pem)?;
    std::fs::write(&key_path,  &key_pem)?;

    Ok((cert_pem, key_pem))
}

pub fn make_tls_config(cert_pem: &str, key_pem: &str) -> Result<ServerConfig, AppError> {
    let cert_der: Vec<_> = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
    let key_der = rustls_pemfile::private_key(&mut key_pem.as_bytes())
        .map_err(|e| AppError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?
        .ok_or_else(|| AppError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, "no private key")))?;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_der, key_der)
        .map_err(|e| AppError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;

    Ok(config)
}
