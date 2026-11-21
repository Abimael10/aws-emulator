// SPDX-License-Identifier: CC0-1.0

//! The TLS SERVER a service that TERMINATES a connection under a Certificate Manager
//! certificate answers a handshake with: the key each certificate's material loads as, and
//! the selection among several of them by the name the `ClientHello` asked for.
//!
//! A certificate is held by Certificate Manager and PRESENTED by another service — a load
//! balancer listener, a custom domain name of either API Gateway — and each of them has the
//! same two things to do with the material the shared directory hands it
//! ([`super::acm_certificate_key_material`]): load the leaf, its chain and the private key
//! into the key a handshake signs with, and choose WHICH of the certificates the resource
//! names to present. Both are stated here once, so every presenting service presents the
//! same certificate for the same `ClientHello`.
//!
//! The selection follows the server name of RFC 6066 §3: the first certificate whose own
//! names cover the name the client asked for, and the resource's FIRST certificate for a
//! client that asked for a name none of them covers or for no name at all — which is the
//! default certificate of the resource that holds them.
//!
//! The private key never leaves this process: it is read out of the material to build the
//! handshake key and is named in no log — the types here carry a `Debug` that reports the
//! names a selection can serve and nothing of the keys.
//!
//! What this module does NOT decide is whether a CLIENT certificate is asked for: that is
//! the presenting resource's own configuration (a mutual-TLS truststore), so the verifier
//! is handed in by the caller that enforces it. Neither is the APPLICATION PROTOCOL the
//! handshake negotiates: which protocols a resource offers is its own configuration too (a
//! listener's ALPN policy, an attribute of the load balancer in front of it), so they are
//! handed in by the caller that SERVES them ([`server_config_offering`]).

use std::sync::Arc;

use crate::acm_certificate_key_material::CertificateKeyMaterial;

// The `rustls` module whose NAME warns that implementing its trait replaces a verification
// the library would otherwise do, imported under a name that says which side of the
// handshake it belongs to. Nothing here implements it and nothing here turns a verification
// off: this module is the SERVER half of a handshake, it verifies no peer at all, and the
// verifier of a resource that asks its clients for a certificate is handed in by the service
// that enforces that.
use ::rustls::server::{danger as client_certificates};

/// The cryptographic provider every handshake this tree terminates is run with.
///
/// Named explicitly rather than taken from the process default: this process links more
/// than one provider, and a configuration that has to work whichever was installed cannot
/// depend on which.
#[must_use]
pub fn provider() -> Arc<::rustls::crypto::CryptoProvider> {
    Arc::new(::rustls::crypto::aws_lc_rs::default_provider())
}

/// The TLS server configuration a resource presenting `certificates` answers a handshake
/// with, or `None` when there is no certificate to present — in which case the caller
/// decides what an encrypted connection gets instead (the core's own TLS server, or
/// nothing).
///
/// `versions` are the protocol versions the resource's own policy negotiates; an empty
/// list takes the library's safe defaults. `client_certificates` is the verifier of a
/// resource that asks the client for a certificate, and `None` asks for none.
///
/// The handshake OFFERS no application protocol: [`server_config_offering`] is the
/// constructor for a resource whose own configuration says which protocols it serves.
pub fn server_config(
    certificates: &[Arc<CertificateKeyMaterial>],
    versions: &[&'static ::rustls::SupportedProtocolVersion],
    client_certificates: Option<Arc<dyn client_certificates::ClientCertVerifier>>,
) -> Option<Arc<::rustls::ServerConfig>> {
    server_config_offering(certificates, versions, client_certificates, &[])
}

/// [`server_config`] for a resource that NEGOTIATES the application protocol: the
/// protocols in `alpn_protocols` are offered in that order, which is the server's own
/// preference order, and the handshake selects the first of them the client also offered.
///
/// An empty list offers none, which is a resource whose configuration names no protocol —
/// and leaves a client that offered some to speak the protocol it would have spoken
/// anyway. A protocol is offered only where the resource SERVES it: a selection the server
/// cannot speak would break every connection made by a client that accepted it.
pub fn server_config_offering(
    certificates: &[Arc<CertificateKeyMaterial>],
    versions: &[&'static ::rustls::SupportedProtocolVersion],
    client_certificates: Option<Arc<dyn client_certificates::ClientCertVerifier>>,
    alpn_protocols: &[&str],
) -> Option<Arc<::rustls::ServerConfig>> {
    let resolver = Arc::new(SelectedByServerName::of(certificates)?);
    let builder = ::rustls::ServerConfig::builder_with_provider(provider());
    let builder = match versions.is_empty() {
        true => builder.with_safe_default_protocol_versions().ok()?,
        false => builder.with_protocol_versions(versions).ok()?,
    };
    let mut config = match client_certificates {
        Some(verifier) => builder
            .with_client_cert_verifier(verifier)
            .with_cert_resolver(resolver),
        None => builder.with_no_client_auth().with_cert_resolver(resolver),
    };
    config.alpn_protocols = alpn_protocols
        .iter()
        .map(|protocol| protocol.as_bytes().to_vec())
        .collect();
    Some(Arc::new(config))
}

/// The certificates one resource presents, and the selection among them: the entry whose
/// own names cover the name the `ClientHello` asked for, and the resource's FIRST
/// certificate for a client that asked for a name none of them covers or for no name at
/// all.
pub struct SelectedByServerName {
    /// Each certificate's loaded key, beside the material it was loaded from — in the
    /// order the resource holds them, default certificate first.
    loaded: Vec<(Arc<CertificateKeyMaterial>, Arc<::rustls::sign::CertifiedKey>)>,
}

impl SelectedByServerName {
    /// The selection over `certificates`, or `None` when none of them loads as a key a
    /// handshake can be run with — material a `rustls` provider does not accept is
    /// material no address can present, and a resolver that answers nothing for every
    /// name would fail every handshake with no certificate having been tried.
    #[must_use]
    pub fn of(certificates: &[Arc<CertificateKeyMaterial>]) -> Option<Self> {
        let loaded: Vec<(Arc<CertificateKeyMaterial>, Arc<::rustls::sign::CertifiedKey>)> =
            certificates
                .iter()
                .filter_map(|material| {
                    certified_key(material).map(|key| (Arc::clone(material), Arc::new(key)))
                })
                .collect();
        (!loaded.is_empty()).then_some(Self { loaded })
    }
}

impl std::fmt::Debug for SelectedByServerName {
    /// The names the selection can serve, and nothing of the keys.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SelectedByServerName")
            .field(
                "domain_names",
                &self
                    .loaded
                    .iter()
                    .map(|(material, _key)| material.domain_names.clone())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ::rustls::server::ResolvesServerCert for SelectedByServerName {
    fn resolve(
        &self,
        client_hello: ::rustls::server::ClientHello<'_>,
    ) -> Option<Arc<::rustls::sign::CertifiedKey>> {
        if let Some(asked_for) = client_hello.server_name() {
            if let Some((_material, key)) = self
                .loaded
                .iter()
                .find(|(material, _key)| material.covers(asked_for))
            {
                return Some(Arc::clone(key));
            }
        }
        // The resource's DEFAULT certificate: the first entry, which is the one the create
        // that minted the resource named.
        self.loaded.first().map(|(_material, key)| Arc::clone(key))
    }
}

/// The signing key and certificate chain one certificate's material loads as, or `None`
/// for material a `rustls` provider does not accept — a key whose algorithm it cannot sign
/// with, or PEM it cannot read.
///
/// The leaf is sent first and the chain after it, which is the order a TLS handshake sends
/// a certificate chain in. The PEM framing is read by `rustls-pemfile`, the reader this
/// tree reads certificate material with.
#[must_use]
pub fn certified_key(material: &CertificateKeyMaterial) -> Option<::rustls::sign::CertifiedKey> {
    let mut chain: Vec<::rustls::pki_types::CertificateDer<'static>> = Vec::new();
    for pem in [
        Some(material.certificate_pem.as_str()),
        material.certificate_chain_pem.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let mut reader = ::std::io::BufReader::new(pem.as_bytes());
        for certificate in ::rustls_pemfile::certs(&mut reader) {
            chain.push(certificate.ok()?);
        }
    }
    if chain.is_empty() {
        return None;
    }
    let mut reader = ::std::io::BufReader::new(material.private_key_pem.as_bytes());
    let key = ::rustls_pemfile::private_key(&mut reader).ok()??;
    let signing_key = ::rustls::crypto::aws_lc_rs::sign::any_supported_type(&key).ok()?;
    Some(::rustls::sign::CertifiedKey::new(chain, signing_key))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real material for a certificate covering `names`, minted the way a caller brings
    /// its own into Certificate Manager.
    fn material(names: &[&str]) -> Arc<CertificateKeyMaterial> {
        let key = ::rcgen::KeyPair::generate().expect("a key pair");
        let params = ::rcgen::CertificateParams::new(
            names.iter().map(|name| (*name).to_string()).collect::<Vec<_>>(),
        )
        .expect("certificate parameters");
        let certificate = params.self_signed(&key).expect("a self-signed certificate");
        Arc::new(CertificateKeyMaterial {
            certificate_pem: certificate.pem(),
            certificate_chain_pem: None,
            private_key_pem: key.serialize_pem(),
            domain_names: names.iter().map(|name| (*name).to_string()).collect(),
        })
    }

    #[test]
    fn material_a_provider_cannot_read_loads_as_no_key() {
        // The rows span what the load generalises over: material whose PEM carries no
        // certificate, and material whose key is not a key.
        let cases: [(&str, &str, &str); 3] = [
            ("no certificate at all", "", "-----BEGIN PRIVATE KEY-----\n"),
            (
                "a certificate body that is not PEM",
                "not a certificate",
                "-----BEGIN PRIVATE KEY-----\n",
            ),
            ("a key that is not a key", "not a certificate", "not a key"),
        ];
        for (case, certificate_pem, private_key_pem) in cases {
            let held = CertificateKeyMaterial {
                certificate_pem: certificate_pem.to_string(),
                certificate_chain_pem: None,
                private_key_pem: private_key_pem.to_string(),
                domain_names: vec!["a.example.com".to_string()],
            };
            let loaded = certified_key(&held);
            assert!(
                loaded.is_none(),
                "case {case:?}: expected no handshake key, got one"
            );
        }
    }

    #[test]
    fn real_material_loads_as_a_handshake_key_and_configures_a_server() {
        let held = material(&["a.example.com"]);
        assert!(
            certified_key(&held).is_some(),
            "case a real leaf and its key: expected a handshake key, got none"
        );
        assert!(
            server_config(&[Arc::clone(&held)], &[], None).is_some(),
            "case one presentable certificate: expected a server configuration, got none"
        );
        assert!(
            server_config(&[], &[], None).is_none(),
            "case no certificate at all: expected no server configuration, got one"
        );
        // The protocol versions a resource's own policy names are configurable, and a
        // selection that can serve no name is still no configuration.
        assert!(
            server_config(&[Arc::clone(&held)], &[&::rustls::version::TLS12], None).is_some(),
            "case one named protocol version: expected a server configuration, got none"
        );
    }

    #[test]
    fn the_selection_reports_its_names_and_nothing_of_its_keys() {
        let selection = SelectedByServerName::of(&[material(&["a.example.com"])])
            .expect("real material loads");
        let formatted = format!("{selection:?}");
        assert!(
            formatted.contains("a.example.com") && !formatted.contains("PRIVATE KEY"),
            "case a formatted selection: expected the names and no key, got {formatted}"
        );
    }
}
