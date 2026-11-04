//! The LAN address, TLS certificate and token for the phone microphone.
//!
//! The phone opens `https://<ip>:<port>/?token=<token>`. The certificate is
//! self-signed for that IP, so the phone needs no CA install. It is made again
//! when the IP changes.

use anyhow::Context;
use rcgen::{CertificateParams, KeyPair, SanType};
use rustls_pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::path::Path;

const CERT_FILE: &str = "phone-cert.pem";
const KEY_FILE: &str = "phone-key.pem";
const IP_FILE: &str = "phone-ip.txt";

/// The certificate chain and private key that the HTTPS server uses.
pub struct TlsIdentity {
    pub cert_chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
}

/// Keep the candidate only if it is a private (LAN) address.
pub fn pick_private(candidate: Option<Ipv4Addr>) -> Option<Ipv4Addr> {
    candidate.filter(|ip| ip.is_private())
}

/// The LAN IPv4 address of this computer, if it has one.
///
/// A UDP `connect` sends no packet. It only makes the OS choose the outgoing interface.
pub fn local_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.168.0.1:9").ok()?;
    let candidate = match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) => Some(ip),
        IpAddr::V6(_) => None,
    };
    pick_private(candidate)
}

/// Reuse the saved certificate when it is for `ip`. Otherwise make a new one and save it.
pub fn load_or_create_identity(dir: &Path, ip: Ipv4Addr) -> anyhow::Result<TlsIdentity> {
    if let Some(identity) = load_saved(dir, ip) {
        return Ok(identity);
    }

    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(ip)));
    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;
    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    // Write the IP file last: a half-written set is never taken as valid.
    std::fs::write(dir.join(CERT_FILE), &cert_pem)?;
    std::fs::write(dir.join(KEY_FILE), &key_pem)?;
    std::fs::write(dir.join(IP_FILE), ip.to_string())?;

    parse_identity(cert_pem.as_bytes(), key_pem.as_bytes())
}

/// The saved identity, or `None` if a file is missing, unreadable or for another IP.
fn load_saved(dir: &Path, ip: Ipv4Addr) -> Option<TlsIdentity> {
    let saved_ip = std::fs::read_to_string(dir.join(IP_FILE)).ok()?;
    if saved_ip.trim() != ip.to_string() {
        return None;
    }
    let cert_pem = std::fs::read(dir.join(CERT_FILE)).ok()?;
    let key_pem = std::fs::read(dir.join(KEY_FILE)).ok()?;
    parse_identity(&cert_pem, &key_pem).ok()
}

fn parse_identity(cert_pem: &[u8], key_pem: &[u8]) -> anyhow::Result<TlsIdentity> {
    let cert_chain = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .context("parse certificate PEM")?;
    anyhow::ensure!(!cert_chain.is_empty(), "no certificate in PEM");
    let key = PrivateKeyDer::from_pem_slice(key_pem).context("parse private key PEM")?;
    Ok(TlsIdentity { cert_chain, key })
}

/// A random token: 32 lowercase hex characters, safe in a URL query.
pub fn new_token() -> String {
    rand::random::<[u8; 16]>()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ipv4_prefers_private() {
        let private = Ipv4Addr::new(10, 0, 0, 5);
        assert_eq!(pick_private(Some(private)), Some(private));
        assert_eq!(pick_private(Some(Ipv4Addr::new(8, 8, 8, 8))), None);
        assert_eq!(pick_private(None), None);
    }

    #[test]
    fn reuses_cert_for_same_ip() {
        let dir = tempfile::tempdir().unwrap();
        let ip = Ipv4Addr::new(192, 168, 1, 10);
        let first = load_or_create_identity(dir.path(), ip).unwrap();
        let second = load_or_create_identity(dir.path(), ip).unwrap();
        assert_eq!(first.cert_chain, second.cert_chain);
    }

    #[test]
    fn regenerates_cert_when_ip_changes() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create_identity(dir.path(), Ipv4Addr::new(192, 168, 1, 10)).unwrap();
        let second = load_or_create_identity(dir.path(), Ipv4Addr::new(192, 168, 1, 11)).unwrap();
        assert_ne!(first.cert_chain, second.cert_chain);
        let saved = std::fs::read_to_string(dir.path().join(IP_FILE)).unwrap();
        assert_eq!(saved, "192.168.1.11");
    }

    #[test]
    fn token_is_32_hex() {
        let token = new_token();
        assert_eq!(token.len(), 32);
        assert!(token
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
        assert_ne!(token, new_token());
    }
}
