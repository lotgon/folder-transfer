//! TLS via rustls (ring provider) with rcgen self-signed certs and SHA-256
//! certificate pinning. See RUST-PORT-SPEC.md sections 3 and 4.1.
//!
//! - Server: in-memory self-signed cert, subject `CN=ft-onetime`.
//! - Client: a custom verifier that accepts ANY certificate whose DER SHA-256
//!   equals the pinned fingerprint, ignoring the hostname (mirrors the
//!   PowerShell `RemoteCertificateValidationCallback`).
//! - Both allow TLS 1.2 (the PowerShell server forces 1.2) and TLS 1.3.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, ServerConfig, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::BoxError;

/// Literal SNI / target name used on the wire (the verifier ignores it anyway).
pub const SNI_NAME: &str = "ft-onetime";
/// Certificate subject common name.
pub const CERT_CN: &str = "ft-onetime";

/// Allow both TLS 1.2 (for PowerShell interop) and TLS 1.3 (Rust<->Rust).
const PROTOCOL_VERSIONS: &[&rustls::SupportedProtocolVersion] =
    &[&rustls::version::TLS12, &rustls::version::TLS13];

/// lowercase hex of SHA-256(DER), no separators.
pub fn fingerprint_hex(cert_der: &[u8]) -> String {
    let digest = Sha256::digest(cert_der);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Parse a hex fingerprint (any case, optional `:`/`-` separators) to raw bytes.
fn parse_fingerprint(fp: &str) -> Result<Vec<u8>, BoxError> {
    let clean: String = fp.chars().filter(|c| *c != ':' && *c != '-').collect();
    if clean.len() != 64 {
        return Err(format!("fingerprint must be 64 hex chars, got {}", clean.len()).into());
    }
    let mut out = Vec::with_capacity(32);
    let bytes = clean.as_bytes();
    for pair in bytes.chunks(2) {
        let s = std::str::from_utf8(pair)?;
        out.push(u8::from_str_radix(s, 16)?);
    }
    Ok(out)
}

/// A freshly minted server identity: its rustls config and printed fingerprint.
pub struct ServerIdentity {
    pub config: Arc<ServerConfig>,
    pub fingerprint: String,
}

/// Mint an in-memory self-signed cert (`CN=ft-onetime`) and build a server config.
pub fn make_server_identity() -> Result<ServerIdentity, BoxError> {
    let mut params = rcgen::CertificateParams::new(vec![SNI_NAME.to_string()])?;
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, CERT_CN);
    params.distinguished_name = dn;

    let key_pair = rcgen::KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;
    let cert_der: CertificateDer<'static> = cert.der().clone();
    let fingerprint = fingerprint_hex(cert_der.as_ref());
    let key_der = PrivatePkcs8KeyDer::from(key_pair.serialize_der());

    let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_protocol_versions(PROTOCOL_VERSIONS)?
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der.into())?;

    Ok(ServerIdentity { config: Arc::new(config), fingerprint })
}

/// Build a client config that pins the server's certificate by SHA-256.
pub fn make_client_config(fingerprint: &str) -> Result<Arc<ClientConfig>, BoxError> {
    let pinned = parse_fingerprint(fingerprint)?;
    let provider = Arc::new(ring::default_provider());
    let verifier = Arc::new(PinnedVerifier { pinned, provider: provider.clone() });
    let config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(PROTOCOL_VERSIONS)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// The literal `ServerName` to dial with.
pub fn sni_server_name() -> ServerName<'static> {
    ServerName::try_from(SNI_NAME).expect("static SNI name is valid")
}

/// Certificate verifier that trusts exactly one cert, identified by its DER SHA-256.
#[derive(Debug)]
struct PinnedVerifier {
    pinned: Vec<u8>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let digest = Sha256::digest(end_entity.as_ref());
        if digest.as_slice() == self.pinned.as_slice() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::General("server certificate fingerprint mismatch".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_64_lowercase_hex() {
        let fp = fingerprint_hex(b"hello");
        assert_eq!(fp.len(), 64);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // Known SHA-256("hello").
        assert_eq!(
            fp,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn parse_fingerprint_round_trip() {
        let fp = fingerprint_hex(b"abc");
        let bytes = parse_fingerprint(&fp).unwrap();
        assert_eq!(bytes.len(), 32);
        assert_eq!(fingerprint_hex(b"abc"), fp);
    }

    #[test]
    fn server_identity_builds() {
        let id = make_server_identity().unwrap();
        assert_eq!(id.fingerprint.len(), 64);
        // The same fingerprint must build a working client config.
        make_client_config(&id.fingerprint).unwrap();
    }
}
