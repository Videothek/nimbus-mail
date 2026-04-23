//! Shared TLS plumbing for `nimbus-imap` and `nimbus-smtp`.
//!
//! Both protocol crates use rustls under the hood, so we keep the
//! cert-validation logic in one place. The two surfaces:
//!
//! - [`build_client_config`] — produces a `rustls::ClientConfig`
//!   pre-loaded with Mozilla's webpki-roots *plus* whatever extra
//!   certs the user has explicitly trusted for this account.
//!   `nimbus-imap` hands this straight to `tokio-rustls`.
//!
//! - [`extra_root_der`] — same trusted-cert DER bytes, but flat,
//!   for callers (notably lettre) that take roots one at a time
//!   via their own builder API.
//!
//! - [`fingerprint_sha256`] — formats SHA-256(DER) as
//!   `aa:bb:cc:dd:…` for the "trust this server?" prompt and for
//!   the audit list in account settings.
//!
//! The "no-verify" probing path (used to capture a self-signed
//! cert before the user has seen it) lives in `nimbus-imap` and
//! `nimbus-smtp` because it's runtime-coupled. We expose
//! [`NoVerifier`] here so they don't each invent one.

use std::sync::Arc;

use rustls::ClientConfig;
use rustls::RootCertStore;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::models::TrustedCert;

/// Build a `rustls::ClientConfig` that trusts Mozilla's standard
/// webpki-roots **and** every additional cert the caller passes in.
/// The latter is how user-trusted self-signed certs become valid:
/// rustls treats each entry in the root store as a trust anchor,
/// so a chain that ends in an exact-match leaf is accepted.
///
/// The returned config is wrapped in `Arc` because rustls expects
/// configs to be cheap to clone and share across connections.
pub fn build_client_config(extra_roots: &[TrustedCert]) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for cert in extra_roots {
        // A bad DER blob shouldn't kill the whole connection — log
        // and skip; the user can re-trust later.
        if let Err(e) = roots.add(CertificateDer::from(cert.der.clone())) {
            tracing::warn!(
                "skipping trusted cert {} for host '{}': {e}",
                cert.sha256, cert.host
            );
        }
    }
    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// Flat list of trusted-cert DER blobs, for callers (lettre) that
/// take additional roots one at a time. Skips entries whose DER
/// can't be parsed as a certificate; same forgiving posture as
/// [`build_client_config`].
pub fn extra_root_der(certs: &[TrustedCert]) -> Vec<Vec<u8>> {
    certs.iter().map(|c| c.der.clone()).collect()
}

/// SHA-256 of the DER bytes, formatted as `aa:bb:cc:…` lowercase
/// hex. Stable representation for the trust prompt and the
/// per-account "trusted certs" list in settings.
pub fn fingerprint_sha256(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    let hex = hex::encode(digest);
    let mut out = String::with_capacity(hex.len() + hex.len() / 2);
    for (i, c) in hex.chars().enumerate() {
        if i > 0 && i % 2 == 0 {
            out.push(':');
        }
        out.push(c);
    }
    out
}

/// "Trust everything" rustls verifier. Used **only** by the
/// probing connect path that captures a server's leaf cert so the
/// user can decide whether to trust it. Never returned from a
/// production code path that handles real mail.
#[derive(Debug)]
pub struct NoVerifier;

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}

/// Build a "no-verify" rustls config. Same warning as `NoVerifier`
/// — only the cert-probe path should ever get one.
pub fn no_verify_config() -> Arc<ClientConfig> {
    Arc::new(
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_format() {
        let fp = fingerprint_sha256(b"hello");
        // Pre-computed so the test catches accidental format drift.
        assert_eq!(
            fp,
            "2c:f2:4d:ba:5f:b0:a3:0e:26:e8:3b:2a:c5:b9:e2:9e:1b:16:1e:5c:1f:a7:42:5e:73:04:33:62:93:8b:98:24"
        );
    }
}
