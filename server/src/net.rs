//! TLS and HTTP plumbing shared by the sources.

use std::error::Error as _;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};

/// Root of Frank Home CA. Caddy on ml60 serves the agent with a certificate
/// from this CA. Public, so it is built in; it expires 2036-10-08.
const HOME_CA: &[u8] = include_bytes!("../../certs/home-root.crt");

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// For PVE and TrueNAS, whose certificates are self-signed. Not a thing to
/// fix (spec §3.1); the app skips verification for them too.
pub fn self_signed_tls() -> Arc<ClientConfig> {
    let provider = provider();
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
        .with_no_client_auth();
    Arc::new(config)
}

/// For the ml60 agent: the home CA root and nothing else. Verification is
/// never switched off here.
pub fn home_ca_tls() -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(HOME_CA).flatten() {
        let _ = roots.add(cert);
    }
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(config)
}

pub fn http(tls: Arc<ClientConfig>, timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new().tls_config(tls).timeout(timeout).build()
}

/// A short reason for a failed request. Never includes the URL's query or
/// any header, so no credential can end up on screen.
pub fn describe(err: ureq::Error, started: Instant) -> String {
    match err {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => {
            let io_kind = t
                .source()
                .and_then(|s| s.downcast_ref::<io::Error>())
                .map(io::Error::kind);
            let text = t.to_string();
            if matches!(io_kind, Some(io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock))
                || text.contains("timed out")
            {
                return timed_out(started);
            }
            let detail = t
                .message()
                .map(str::to_string)
                .or_else(|| t.source().map(|s| s.to_string()));
            match detail {
                Some(detail) => format!("{}: {detail}", t.kind()),
                None => t.kind().to_string(),
            }
        }
    }
}

/// How long the poll actually waited. The limit that fired is a request's own
/// timeout, never the poll interval; a poll makes several requests in turn,
/// so the total can be more than one limit.
pub fn timed_out(started: Instant) -> String {
    format!("timed out after {:.0}s", started.elapsed().as_secs_f64())
}

pub fn is_timeout(err: &io::Error) -> bool {
    // A socket read timeout is EAGAIN on Linux, which reads as WouldBlock.
    matches!(err.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
}

#[derive(Debug)]
struct AcceptAny(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
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

    // The signatures are still checked: the handshake has to be with the
    // holder of whatever certificate was presented.
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_ca_parses() {
        let certs: Vec<_> = CertificateDer::pem_slice_iter(HOME_CA)
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(certs.len(), 1);
    }
}
