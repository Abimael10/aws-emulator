// SPDX-License-Identifier: CC0-1.0

//! The SOCKET a load balancer's listeners are reached on: the address the load balancer
//! publishes as its `DNSName`, the TLS termination an HTTPS or TLS listener performs there,
//! and the hand-off of every request it accepts to the data plane
//! ([`super::data_plane`]).
//!
//! `LoadBalancer$DNSName` is "the public DNS name of the load balancer" — the address a
//! CLIENT then dials — so it has to be an address something answers on. A name under
//! Elastic Load Balancing's own zone resolves to nothing on the machine this emulator runs
//! on, so a load balancer that published one published an address no client could reach,
//! and the one thing an HTTPS listener exists for (serving TLS under the certificate the
//! account gave it) was unreachable. So each load balancer whose listeners carry a client's
//! own traffic — an Application Load Balancer's, which answer the request, and a Network Load
//! Balancer's, which hand the connection to what their target group selects — gets a listener
//! of the EMULATOR's own, opened through the core
//! (`engine.listeners().tcp_plaintext(..)`, which owns the socket, the bind interface, the
//! port range and the connection caps), and publishes the `host:port` that listener answers
//! on ([`crate::state::LoadBalancerEndpoint`]).
//!
//! WHICH of its listeners a connection reaches is decided by what the client opened with and
//! by the load balancer's own family ([`cleartext_protocols`]): a connection in the clear
//! reaches an `HTTP` listener of the Application family and a `TCP` one of the Network family,
//! and a TLS connection the listener that terminates TLS (`HTTPS`, `TLS`).
//!
//! # What this address serves, and under which certificate
//!
//! The first byte of a connection says what the client opened with: a TLS record (the
//! `handshake` content type, [`::service_common::in_band_tls::TLS_RECORD_FIRST_BYTE`]) or a
//! request in the clear. So ONE address serves both the `https://` a client of an HTTPS
//! listener dials and the `http://` a client of an HTTP listener dials, and the connection
//! is attributed to the load balancer's listeners of that kind.
//!
//! A TLS connection is terminated HERE rather than by the core's own TLS server, and that
//! is the whole point of the module: what an Application Load Balancer presents is the
//! certificate ACM holds for its listener's `Certificates[].CertificateArn` — the account's
//! own leaf and chain — not a certificate of the server's. The core's TLS server presents
//! the emulator's own certificate and takes no other ([`::emulator_server::TlsUpgrader`]),
//! so a handshake run through it can never present the account's certificate. The key
//! material is read from the ONE shared directory the certificate's owner writes
//! ([`::service_common::acm_certificate_key_material`]) — the leaf, its chain and the
//! private key that matches it — and never leaves this process: it is held in memory, used
//! to answer a handshake, and named in no log.
//!
//! WHICH certificate is presented is the client's choice of name: "Adds the specified SSL
//! server certificate to the certificate list for the specified HTTPS or TLS listener"
//! (`AddListenerCertificates`) is what the certificate list is for, so the name the
//! `ClientHello` asks for (SNI) selects the certificate of the listener's list that covers
//! it, and a client that names no certificate's name — or no name at all — is presented the
//! listener's DEFAULT certificate, the one `CreateListener` set.
//!
//! A client that dials TLS where no certificate's key material is reachable (a listener
//! whose certificate was uploaded to IAM, or issued by ACM rather than imported into it,
//! so no private key for it exists anywhere in this emulator) is handed to the core's own
//! TLS server instead: the connection is served, under the emulator's own certificate.
//! Nothing is invented for it — a client that verifies the name it asked for sees exactly
//! that.
//!
//! # What answers the request
//!
//! Nothing about the ANSWER is decided here: the listener's rules, its default actions, the
//! web ACL in front of the load balancer and the target it forwards to all stay in
//! [`super::data_plane`], which serves the same request however it was addressed. The one
//! thing this address decides is what only it can see: the FRAMING the client wrote the
//! request with. The leg handed on re-frames the body itself and carries neither framing
//! header, so the load balancer's own `routing.http.desync_mitigation_mode` is applied here
//! ([`super::request_framing`]) — a framing it refuses is answered `400` by the load balancer
//! with nothing forwarded.
//!
//! So a request this socket accepts is handed to that one plane over the in-process transport
//! this emulator reaches itself on, addressed as the RESERVED data-plane path of the load
//! balancer this socket was opened for, and carrying the PORT of the listener the
//! connection reached — so the plane evaluates the listener the client actually connected
//! to.
//!
//! The same handler serves the connections a client INSIDE this emulator opens to the very
//! address the load balancer published ([`crate::state::LoadBalancerEndpoint`]), so one load
//! balancer answers the same request however the connection reached it. Where such a client
//! came FROM travels on no header: it belongs to the connection, and the plane reads it there
//! ([`super::data_plane`]), so nothing a client writes can become the address its own request
//! is attributed to.
//!
//! Every status, header and byte is computed from what a create or a modify persisted, and
//! this module writes NOTHING: serving traffic moves no status, consumes no quota and bumps
//! no counter.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ::bytes::Bytes;
use ::emulator_server::{listener_caps, ListenerConn, NativeHandler, TlsUpgrader};
use ::http_body_util::{BodyExt as _, Full, Limited};
use ::service_common::acm_certificate_key_material::{
    CertificateKeyMaterial, CertificateKeyMaterialDirectory,
};
use ::service_common::in_band_tls::{Prefixed, TLS_RECORD_FIRST_BYTE};
use ::smithy_server_runtime::EmbeddedEmulator;
use ::tokio::io::AsyncReadExt as _;
use ::tokio::sync::{OwnedSemaphorePermit, Semaphore};
use aws_smithy_runtime_api::client::http::HttpConnector;
use aws_smithy_runtime_api::http::Request as RtRequest;
use aws_smithy_types::body::SdkBody;

use crate::state::ComAmazonawsElasticloadbalancingv2ElasticloadbalancingV10State as ElbState;

/// The host the forwarded leg addresses this service's own data plane at: the endpoint that
/// names THIS model, as opposed to the `elasticloadbalancing` prefix it shares with the
/// Classic Elastic Load Balancing model (`crate::service::ROUTE_KEY`).
///
/// A request the shared prefix names is claimed by the Classic front door as well, and two
/// claimants is what the router answers `500 AmbiguousService`; the key that names this
/// model alone keeps the forwarded leg's claimant the plane it is meant for.
const SERVICE_ENDPOINT: &str = "elasticloadbalancingv2.amazonaws.com";

/// The `ProtocolEnum` values a connection IN THE CLEAR reaches, per FAMILY: an Application
/// Load Balancer's `HTTP` listener, which terminates no TLS and answers the client's own
/// request, and a Network Load Balancer's `TCP` and `TCP_UDP` listeners, which carry a stream
/// in the clear and hand it to what their target group selects (its `TLS` listener terminates
/// TLS, so a TLS connection reaches that one). A Gateway Load Balancer's `GENEVE` listeners
/// carry no connection of this kind at all.
fn cleartext_protocols(family: crate::state::LoadBalancerFamily) -> &'static [&'static str] {
    match family {
        crate::state::LoadBalancerFamily::Application => &["HTTP"],
        crate::state::LoadBalancerFamily::Network => &["TCP", "TCP_UDP"],
        crate::state::LoadBalancerFamily::Gateway => &[],
    }
}

/// How many request BODIES this service reads at once across every load balancer's socket:
/// the core's own HTTP in-flight cap, one permit per request, taken before a byte of a body
/// is read and released when the answer is written.
///
/// What one in-flight request may buffer is capped by [`body_ceiling`], so what the bodies
/// in flight hold at once is this count times that ceiling — the product the core's own
/// HTTP listeners bound, rather than the connection count (or the number of load balancers)
/// times it. ONE semaphore for the whole service, because a caller chooses how many load
/// balancers exist and each one would otherwise raise the peak again. A request that finds
/// no permit is shed with the status a load balancer answers a request it could not take.
fn in_flight() -> Arc<Semaphore> {
    static IN_FLIGHT: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(IN_FLIGHT.get_or_init(|| Arc::new(Semaphore::new(listener_caps::MAX_HTTP_INFLIGHT))))
}

/// The headers the forwarded leg does not copy from the client's request.
///
/// `Host` is this socket's own address, which the forwarded request replaces with the
/// address of the listener the connection reached (the plane resolves the load balancer from
/// the reserved path instead, and reads the name the CLIENT dialled off
/// [`crate::service::data_plane::FORWARDED_HOST_HEADER`], which this leg SETS so a client's
/// own value cannot travel as one), and the two framing headers describe a body this leg
/// re-frames itself.
///
/// The four TLS headers are here too: the facts of the connection are what this leg
/// OBSERVES and states ([`crate::service::data_plane::TLS_HEADERS`]), so a client's own
/// value under one of those names never travels as an observation of its own handshake.
const HEADERS_NOT_FORWARDED: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    crate::service::data_plane::FORWARDED_HOST_HEADER,
    crate::service::data_plane::TLS_PROTOCOL_HEADER,
    crate::service::data_plane::TLS_CIPHER_HEADER,
    crate::service::data_plane::TLS_SERVER_NAME_HEADER,
    crate::service::data_plane::TLS_CERTIFICATE_ARN_HEADER,
    crate::service::data_plane::TLS_CLIENT_CERTIFICATE_HEADER,
];

/// Serves ONE load balancer's socket: it terminates TLS under that load balancer's own
/// certificates and hands every request it accepts to this service's data plane.
///
/// Cheap to clone — every field is a handle — which is what the core's `'static`
/// per-connection handler contract asks for.
#[derive(Clone)]
pub struct ListenerSocket {
    /// The load balancer this socket serves, published once the create that opened it has
    /// minted the ARN. A connection that arrives before then is served no load balancer.
    load_balancer: Arc<OnceLock<String>>,
    /// The ONE shared store the control plane writes, so a connection is served from
    /// exactly the listener configuration a create or a modify persisted.
    state: ::engine::ServiceState<ElbState>,
    /// The shared directory the key material of an imported ACM certificate lives in, and
    /// `None` on an engine that carries none — where a TLS connection is served by the
    /// core's own TLS server instead.
    key_material: Option<::engine::ServiceState<CertificateKeyMaterialDirectory>>,
    /// The handle into the emulator this socket belongs to: how a request it accepts
    /// reaches the data plane. The socket is not a second emulator, it is a second ADDRESS
    /// of this one.
    callback: EmbeddedEmulator,
    /// The core's own TLS server, for a TLS connection this service holds no certificate
    /// key material for. `None` when this emulator has none prepared, in which case such a
    /// connection is closed rather than served in the clear.
    upgrader: Option<TlsUpgrader>,
}

impl ListenerSocket {
    /// The socket handler for one load balancer, over the shared engine's own stores.
    pub fn new(
        load_balancer: Arc<OnceLock<String>>,
        state: ::engine::ServiceState<ElbState>,
        key_material: Option<::engine::ServiceState<CertificateKeyMaterialDirectory>>,
        callback: EmbeddedEmulator,
        upgrader: Option<TlsUpgrader>,
    ) -> Self {
        Self {
            load_balancer,
            state,
            key_material,
            callback,
            upgrader,
        }
    }
}

impl NativeHandler for ListenerSocket {
    /// One call per accepted connection. The handles are cloned into the connection's own
    /// future, which is what the core's `'static` handler contract asks for.
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let socket = self.clone();
        Box::pin(async move { socket.serve_connection(conn).await })
    }
}

/// What one accepted connection is served as: the load balancer it reached, the stream to
/// speak HTTP on, and the PORT of the listener it reached — which is what decides whose
/// rules answer the request.
struct Accepted {
    /// The load balancer this socket was opened for, which is what the request's own framing
    /// is judged against ([`Framing`]).
    load_balancer_arn: String,
    load_balancer_name: String,
    conn: ListenerConn,
    listener_port: Option<i32>,
    /// What the handshake on this connection negotiated, for a connection that ran one.
    negotiated: NegotiatedTls,
    /// The framing of each request on this connection, as the CLIENT wrote it
    /// ([`crate::service::request_head`]).
    framings: Arc<crate::service::request_head::Framings>,
}

/// What the TLS handshake on one connection negotiated — the facts of the CONNECTION, which
/// only the end that ran the handshake observes, and which the load balancer's own
/// access-log record carries for the request it carried ([`super::access_logs`]).
///
/// Every field is absent for a connection in the clear, and the two name fields for a client
/// that asked for no server name. Cloned per request on the connection, as the handles
/// around it are.
#[derive(Clone, Debug, Default)]
struct NegotiatedTls {
    /// The protocol version, spelled as the published `SslProtocols` catalogue spells it.
    protocol: Option<&'static str>,
    /// The cipher suite, spelled as the published `Cipher$Name` catalogue spells it.
    cipher: Option<&'static str>,
    /// The name the `ClientHello` asked for (RFC 6066 §3).
    server_name: Option<String>,
    /// The ARN of the certificate presented for that name.
    certificate_arn: Option<String>,
    /// The chain the CLIENT presented, for a listener that asked for one and a client that
    /// answered with a certificate ([`crate::service::client_certificates`]).
    client_certificate: Option<String>,
}

/// What one request's FRAMING is judged against: the framings the client wrote on this
/// connection, the store the control plane writes, and the load balancer whose
/// `routing.http.desync_mitigation_mode` decides which framings it refuses
/// ([`crate::service::request_framing`]).
///
/// The framing is read off the CONNECTION rather than off the parsed request: the server this
/// address speaks through normalises a request's framing as it parses it, and the leg this
/// socket hands the data plane re-frames the body itself and carries neither framing header
/// ([`HEADERS_NOT_FORWARDED`]), so the client's own framing exists only here.
struct Framing {
    state: ::engine::ServiceState<ElbState>,
    load_balancer_arn: String,
    framings: Arc<crate::service::request_head::Framings>,
}

impl Framing {
    /// Whether the load balancer refuses the next request of this connection rather than
    /// passing it on: the framing the client wrote it with, judged by the mode the load
    /// balancer is at.
    ///
    /// Read under the store's lock, which is released before the answer is produced. A request
    /// no framing was read for — one on a connection whose framings can no longer be followed
    /// — and a load balancer this store no longer holds are each passed on: the request is
    /// then answered as the plane answers it.
    fn refuses(&self) -> bool {
        let Some(framing) = self.framings.next_request() else {
            return false;
        };
        let store = self.state.lock();
        let Some(record) = store.load_balancers.get(&self.load_balancer_arn) else {
            return false;
        };
        crate::service::request_framing::refuses(
            crate::service::request_framing::mitigation(record),
            framing,
        )
    }
}

impl ListenerSocket {
    /// Serve one accepted connection: negotiate what the client opened with, then speak
    /// HTTP/1.1 on it until the client closes it.
    async fn serve_connection(&self, conn: ListenerConn) {
        let Some(accepted) = self.accepted(conn).await else {
            return;
        };
        let Accepted {
            load_balancer_arn,
            load_balancer_name,
            conn,
            listener_port,
            negotiated,
            framings,
        } = accepted;
        let io = ::hyper_util::rt::TokioIo::new(conn);
        let callback = self.callback.clone();
        let state = self.state.clone();
        let service = ::hyper::service::service_fn(move |request: ::http::Request<_>| {
            let callback = callback.clone();
            let load_balancer_name = load_balancer_name.clone();
            let negotiated = negotiated.clone();
            // What this request was framed with, the store, and the load balancer it reached,
            // so each request of the connection is judged by the mode that load balancer is
            // at WHEN it arrives: a client that modifies the attribute between two requests
            // is answered out of the value it wrote.
            let framing = Framing {
                state: state.clone(),
                load_balancer_arn: load_balancer_arn.clone(),
                framings: Arc::clone(&framings),
            };
            async move {
                // A panic anywhere behind this address — in the dispatch machinery or in
                // the plane the forwarded leg reaches — is CONTAINED here and answered as
                // the protocol-shaped internal failure, exactly as the endpoint a control
                // call arrives on answers the same request: one client cannot take the
                // connection, the listener or the process down with it, and the next
                // request is served normally.
                let facts = ::emulator_server::RequestFacts::from_request(&request);
                Ok::<_, ::std::convert::Infallible>(
                    ::emulator_server::contain_panics(
                        facts,
                        answer(
                            callback,
                            framing,
                            load_balancer_name,
                            listener_port,
                            negotiated,
                            request,
                        ),
                    )
                    .await,
                )
            }
        });
        if let Err(error) = ::hyper::server::conn::http1::Builder::new()
            .timer(::hyper_util::rt::TokioTimer::new())
            .header_read_timeout(request_deadline())
            .serve_connection(io, service)
            .await
        {
            // A connection the client dropped, or a request this emulator could not frame,
            // ends here and takes nothing else with it: the next connection is served
            // normally.
            ::tracing::debug!(
                %error,
                "a connection to a load balancer's own address ended before its response"
            );
        }
    }

    /// Read what the client opened the connection with and hand back the stream the request
    /// is read from, or `None` when there is nothing to serve on it: a socket whose load
    /// balancer is gone, a client that sent nothing, a TLS handshake this emulator could not
    /// complete, and a TLS connection to a load balancer that terminates none.
    ///
    /// Every phase BEFORE a request is read is bounded by [`handshake_deadline`], the first
    /// byte included: a client that completes TCP and then sends nothing holds the connection
    /// permit the core issued for it only until that deadline, so cheap connections cannot
    /// park this socket's whole allowance without a request being sent.
    async fn accepted(&self, mut conn: ListenerConn) -> Option<Accepted> {
        let load_balancer_arn = self.load_balancer.get().cloned()?;
        let mut first = [0u8; 1];
        match ::tokio::time::timeout(handshake_deadline(), conn.read(&mut first)).await {
            Ok(Ok(1)) => {}
            _ => return None,
        }
        let prefixed: ListenerConn = Box::new(Prefixed::new(first.to_vec(), conn));
        // What this load balancer holds for a connection of this kind, read under the one
        // store the control plane writes and RELEASED before the handshake is run.
        let plan = self.plan(&load_balancer_arn, first[0] == TLS_RECORD_FIRST_BYTE)?;
        if first[0] != TLS_RECORD_FIRST_BYTE {
            // The framing of every request on this connection, read off the bytes the client
            // wrote before the server parses them.
            let (sniffed, framings) = crate::service::request_head::sniffed(prefixed);
            return Some(Accepted {
                load_balancer_arn,
                load_balancer_name: plan.load_balancer_name,
                conn: Box::new(sniffed),
                listener_port: plan.listener_port,
                // A connection in the clear negotiated nothing: there is no protocol, no
                // cipher, no server name and no certificate to report for it.
                negotiated: NegotiatedTls::default(),
                framings,
            });
        }
        let presented: Vec<Arc<CertificateKeyMaterial>> = plan
            .certificates
            .iter()
            .map(|(_arn, material)| Arc::clone(material))
            .collect();
        let (conn, negotiated) = match tls_server_config(
            &presented,
            plan.ssl_policy.as_deref(),
            plan.asks_for_client_certificates,
        ) {
            // The account's own certificates, as the listener names them.
            Some(config) => {
                let acceptor = ::tokio_rustls::TlsAcceptor::from(config);
                match ::tokio::time::timeout(handshake_deadline(), acceptor.accept(prefixed)).await
                {
                    Ok(Ok(stream)) => {
                        // What the handshake just agreed, read off the connection that ran
                        // it: this is the only place it can be observed.
                        let negotiated = negotiated_tls(&stream, &plan.certificates);
                        (Box::new(stream) as ListenerConn, negotiated)
                    }
                    Ok(Err(error)) => {
                        ::tracing::debug!(
                            %error,
                            "a TLS handshake on a load balancer's own address was not completed"
                        );
                        return None;
                    }
                    Err(_) => return None,
                }
            }
            // No key material for any certificate this listener names: the connection is
            // served by the core's own TLS server, under the emulator's own certificate.
            // That handshake is the core's, so this service observed none of it and reports
            // nothing about it.
            None => {
                let upgrader = self.upgrader.clone()?;
                match ::tokio::time::timeout(
                    handshake_deadline(),
                    upgrader.accept(prefixed, None),
                )
                .await
                {
                    Ok(Ok((upgraded, _negotiated))) => (upgraded, NegotiatedTls::default()),
                    Ok(Err(error)) => {
                        ::tracing::debug!(
                            %error,
                            "a TLS handshake on a load balancer's own address was not completed \
                             by the core's TLS server"
                        );
                        return None;
                    }
                    Err(_) => return None,
                }
            }
        };
        // The framing of every request on the connection, read off the DECRYPTED bytes: the
        // client's own, as it wrote them under TLS.
        let (sniffed, framings) = crate::service::request_head::sniffed(conn);
        Some(Accepted {
            load_balancer_arn,
            load_balancer_name: plan.load_balancer_name,
            conn: Box::new(sniffed),
            listener_port: plan.listener_port,
            negotiated,
            framings,
        })
    }

    /// What this socket serves a connection of one kind with: the load balancer's name, the
    /// port of the listener such a connection reaches, and — for a TLS connection — the
    /// certificates that listener presents, in the order a selection considers them.
    ///
    /// Read under the store's lock, which is released before anything else happens: a
    /// handshake and a forwarded request never hold this service's store.
    ///
    /// `None` for a load balancer this store no longer holds, and for a connection of a
    /// kind it has no listener for at all — a TLS connection to a load balancer that
    /// terminates no TLS has no certificate to present and no rules to evaluate, so there
    /// is nothing to serve on it.
    fn plan(&self, load_balancer_arn: &str, tls: bool) -> Option<ConnectionPlan> {
        let (
            load_balancer_name,
            listener_port,
            ssl_policy,
            certificate_arns,
            asks_for_client_certificates,
        ) = {
            let store = self.state.lock();
            let record = store.load_balancers.get(load_balancer_arn)?;
            // Every listener of this load balancer that answers a connection of this kind,
            // lowest port first — the port its first listener is given.
            let mut listeners: Vec<&crate::state::ListenerRecord> = record
                .listener_arns
                .iter()
                .filter_map(|arn| store.listeners.get(arn))
                .filter(|listener| {
                    // A connection in the clear is served by a listener that terminates no
                    // TLS, and a TLS connection by one that does — which is the bracket the
                    // model puts on a listener's certificates and security policy
                    // ([`crate::state::terminates_tls`]). WHICH protocols carry a connection
                    // in the clear is the load balancer's own family's
                    // ([`cleartext_protocols`]): an Application Load Balancer's `HTTP`
                    // listener, and a Network Load Balancer's `TCP` one.
                    match tls {
                        true => crate::state::terminates_tls(listener.protocol.as_deref()),
                        false => listener.protocol.as_deref().is_some_and(|protocol| {
                            cleartext_protocols(record.family()).contains(&protocol)
                        }),
                    }
                })
                .collect();
            listeners.sort_by(|left, right| {
                left.port.cmp(&right.port).then_with(|| left.arn.cmp(&right.arn))
            });
            let listener = listeners.first().copied()?;
            (
                record.name.clone(),
                listener.port,
                listener.ssl_policy.clone(),
                listener.presented_certificate_arns(),
                // Whether this listener's mutual-authentication mode has it ASK the client
                // for a certificate, which is the only way one is presented at all
                // ([`crate::service::client_certificates`]).
                listener
                    .effective_mutual_authentication()
                    .and_then(|attributes| attributes.mode)
                    .as_deref()
                    == Some(crate::service::client_certificates::PASSTHROUGH_MODE),
            )
        };
        Some(ConnectionPlan {
            load_balancer_name,
            listener_port,
            ssl_policy,
            certificates: self.material_of(&certificate_arns),
            asks_for_client_certificates,
        })
    }

    /// The key material of each certificate in `certificate_arns`, beside the ARN it is held
    /// under, in that order — the listener's default certificate first, then its certificate
    /// list — skipping every certificate no material is held for and every one whose material
    /// does not load as a key a handshake can be run with.
    ///
    /// The ARN travels with the material because the certificate a handshake PRESENTS is one
    /// of these, and the access-log record of the request it carries names it
    /// (`chosen_cert_arn`). The unloadable ones are dropped here so this list is exactly what
    /// the selection holds ([`::service_common::acm_certificate_termination`]), and the ARN
    /// this module reports for a handshake is the ARN of the certificate that selection
    /// chose.
    fn material_of(&self, certificate_arns: &[String]) -> Vec<(String, Arc<CertificateKeyMaterial>)> {
        let Some(directory) = self.key_material.as_ref() else {
            return Vec::new();
        };
        let directory = directory.lock();
        certificate_arns
            .iter()
            .filter_map(|arn| {
                let material = directory.material(arn)?;
                ::service_common::acm_certificate_termination::certified_key(&material)
                    .is_some()
                    .then(|| (arn.clone(), material))
            })
            .collect()
    }
}

/// What the handshake `stream` completed negotiated, and which of `certificates` it
/// presented.
///
/// The protocol version and the cipher suite are spelled as the published security-policy
/// catalogue spells them ([`protocol_name`], [`cipher_name`]), so what a record carries for a
/// connection and what `DescribeSSLPolicies` publishes for the policy it ran under are the
/// same strings. The certificate is resolved by the same rule the handshake chose it with
/// ([`chosen_certificate`]).
fn negotiated_tls<IO>(
    stream: &::tokio_rustls::server::TlsStream<IO>,
    certificates: &[(String, Arc<CertificateKeyMaterial>)],
) -> NegotiatedTls
where
    IO: ::tokio::io::AsyncRead + ::tokio::io::AsyncWrite + Unpin,
{
    let (_io, connection) = stream.get_ref();
    let server_name = connection.server_name().map(str::to_string);
    NegotiatedTls {
        protocol: connection.protocol_version().and_then(protocol_name),
        cipher: connection
            .negotiated_cipher_suite()
            .and_then(|suite| cipher_name(suite.suite())),
        certificate_arn: chosen_certificate(certificates, server_name.as_deref()),
        server_name,
        // What the CLIENT presented, for a listener that asked: the chain itself, which only
        // the end that ran the handshake holds
        // ([`crate::service::client_certificates::presented_chain`]).
        client_certificate: connection
            .peer_certificates()
            .and_then(crate::service::client_certificates::presented_chain),
    }
}

/// The `SslProtocol` spelling of one negotiated protocol version, and `None` for a version
/// the published catalogue names none of — nothing is reported for a version this service
/// cannot name.
fn protocol_name(version: ::rustls::ProtocolVersion) -> Option<&'static str> {
    match version {
        ::rustls::ProtocolVersion::TLSv1_2 => Some(TLS_1_2),
        ::rustls::ProtocolVersion::TLSv1_3 => Some(TLS_1_3),
        _ => None,
    }
}

/// The `Cipher$Name` spelling of one negotiated cipher suite, and `None` for a suite this
/// table does not name.
fn cipher_name(suite: ::rustls::CipherSuite) -> Option<&'static str> {
    NEGOTIATED_CIPHER_NAMES
        .iter()
        .find(|(negotiated, _name)| *negotiated == suite)
        .map(|(_negotiated, name)| *name)
}

/// How the published cipher catalogue spells each suite this emulator's TLS implementation
/// can negotiate.
///
/// `Cipher$Name` is "the name of the cipher", and the catalogue
/// `DescribeSSLPolicies` publishes ([`crate::ops::describe_ssl_policies`]) spells a TLS 1.3
/// suite by its registered name (`TLS_AES_128_GCM_SHA256`) and a TLS 1.2 suite by its
/// OpenSSL name (`ECDHE-RSA-AES128-GCM-SHA256`) — so that is the spelling a record of a
/// connection carries, and the one a reader can look up in that catalogue.
const NEGOTIATED_CIPHER_NAMES: &[(::rustls::CipherSuite, &str)] = &[
    (
        ::rustls::CipherSuite::TLS13_AES_128_GCM_SHA256,
        "TLS_AES_128_GCM_SHA256",
    ),
    (
        ::rustls::CipherSuite::TLS13_AES_256_GCM_SHA384,
        "TLS_AES_256_GCM_SHA384",
    ),
    (
        ::rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
        "TLS_CHACHA20_POLY1305_SHA256",
    ),
    (
        ::rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
        "ECDHE-ECDSA-AES128-GCM-SHA256",
    ),
    (
        ::rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
        "ECDHE-ECDSA-AES256-GCM-SHA384",
    ),
    (
        ::rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
        "ECDHE-RSA-AES128-GCM-SHA256",
    ),
    (
        ::rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
        "ECDHE-RSA-AES256-GCM-SHA384",
    ),
    (
        ::rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
        "ECDHE-ECDSA-CHACHA20-POLY1305",
    ),
    (
        ::rustls::CipherSuite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
        "ECDHE-RSA-CHACHA20-POLY1305",
    ),
];

/// Which of `certificates` a handshake asking for `server_name` was presented: the first
/// whose own names cover that name, and the listener's DEFAULT certificate — the first entry
/// — for a client that asked for a name none of them covers or for no name at all.
///
/// The same selection the handshake itself ran
/// ([`::service_common::acm_certificate_termination`]), over the same list in the same
/// order, so the ARN reported is the ARN of the certificate the client was really presented.
fn chosen_certificate(
    certificates: &[(String, Arc<CertificateKeyMaterial>)],
    server_name: Option<&str>,
) -> Option<String> {
    if let Some(asked_for) = server_name {
        if let Some((arn, _material)) = certificates
            .iter()
            .find(|(_arn, material)| material.covers(asked_for))
        {
            return Some(arn.clone());
        }
    }
    certificates.first().map(|(arn, _material)| arn.clone())
}

/// What a connection of one kind is served with ([`ListenerSocket::plan`]).
struct ConnectionPlan {
    load_balancer_name: String,
    listener_port: Option<i32>,
    /// The security policy the addressed listener was configured with, and `None` for a
    /// listener that names none — which negotiates under the default policy.
    ssl_policy: Option<String>,
    /// The certificates the addressed listener presents, default certificate first, each
    /// beside the ARN it is held under. Empty for a connection in the clear, and for a TLS
    /// listener this emulator holds no presentable key material for.
    certificates: Vec<(String, Arc<CertificateKeyMaterial>)>,
    /// Whether the addressed listener asks the client for a certificate of its own
    /// ([`crate::service::client_certificates`]).
    asks_for_client_certificates: bool,
}

/// The TLS server configuration a listener presenting `certificates` under the security
/// policy `ssl_policy` answers a handshake with, or `None` when there is no certificate to
/// present — in which case the caller falls back to the core's own TLS server.
///
/// The certificate is chosen per handshake by the name the `ClientHello` asks for (the one
/// selection [`::service_common::acm_certificate_termination`] states), so one listener serves
/// every name its certificate list
/// covers. The protocol versions are the ones the listener's security policy negotiates
/// ([`negotiated_versions`]), so a client reads the version that policy allows rather than
/// whatever this library would prefer.
///
/// `asks_for_client_certificates` is the listener's own mutual-authentication mode: a listener
/// that hands a client certificate to its target asks every client for one
/// ([`crate::service::client_certificates`]), and every other listener asks for none.
fn tls_server_config(
    certificates: &[Arc<CertificateKeyMaterial>],
    ssl_policy: Option<&str>,
    asks_for_client_certificates: bool,
) -> Option<Arc<::rustls::ServerConfig>> {
    ::service_common::acm_certificate_termination::server_config(
        certificates,
        &negotiated_versions(ssl_policy),
        asks_for_client_certificates
            .then(crate::service::client_certificates::PresentedWithoutValidation::asking),
    )
}

/// The TLS protocol versions a listener on the security policy `ssl_policy` completes a
/// handshake under: the `SslProtocols` that policy publishes
/// ([`crate::ops::describe_ssl_policies::policy_ssl_protocols`]), narrowed to the versions
/// this emulator's TLS implementation can negotiate at all.
///
/// "[HTTPS and TLS listeners] The security policy that defines which protocols and ciphers
/// are supported" (`CreateListenerInput$SslPolicy`) is what decides this, so a listener set
/// to a policy that stops at TLS 1.2 negotiates TLS 1.2 and one set to a policy that lists
/// TLS 1.3 may negotiate TLS 1.3. A listener that names no policy negotiates under the
/// default policy ([`crate::ops::describe_ssl_policies::DEFAULT_SSL_POLICY`]), which is the
/// policy such a listener runs under, and so does one naming a policy this service
/// publishes no protocol list for.
///
/// TLS 1.0 and TLS 1.1 are named by several published policies and negotiated by none of
/// this emulator's connections: the TLS library it serves handshakes with implements TLS
/// 1.2 and TLS 1.3 only. Every published policy that names an older version names TLS 1.2
/// as well, so each of them still negotiates the highest version it publishes.
fn negotiated_versions(ssl_policy: Option<&str>) -> Vec<&'static ::rustls::SupportedProtocolVersion> {
    let policy = ssl_policy
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(crate::ops::describe_ssl_policies::DEFAULT_SSL_POLICY);
    let published = crate::ops::describe_ssl_policies::policy_ssl_protocols(policy).or_else(|| {
        crate::ops::describe_ssl_policies::policy_ssl_protocols(
            crate::ops::describe_ssl_policies::DEFAULT_SSL_POLICY,
        )
    });
    let published = published.unwrap_or_default();
    let mut versions: Vec<&'static ::rustls::SupportedProtocolVersion> = Vec::new();
    if published.contains(&TLS_1_2) {
        versions.push(&::rustls::version::TLS12);
    }
    if published.contains(&TLS_1_3) {
        versions.push(&::rustls::version::TLS13);
    }
    versions
}

/// The `SslProtocol` spellings of the two versions this emulator negotiates, as the
/// published catalogue writes them.
const TLS_1_2: &str = "TLSv1.2";
const TLS_1_3: &str = "TLSv1.3";

/// How long one request may take, from its first header byte to the answer: the core's own
/// HTTP request deadline, so this address bounds a request exactly as the core's HTTP
/// listeners do.
fn request_deadline() -> Duration {
    Duration::from_secs(listener_caps::HTTP_REQUEST_DEADLINE_SECS)
}

/// How long a TLS handshake on this address may take, matching the deadline the core puts
/// on a handshake its own TLS server runs: a client that completes TCP and then stalls the
/// handshake cannot hold the connection permit the core issued for it.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// [`HANDSHAKE_DEADLINE`], as the timeout this address applies.
fn handshake_deadline() -> Duration {
    HANDSHAKE_DEADLINE
}

/// How many bytes of one request this address reads: the core's per-connection ceiling.
fn body_ceiling() -> usize {
    usize::try_from(listener_caps::MAX_BYTES_PER_CONN).unwrap_or(usize::MAX)
}

/// Answer one request from the data plane of the load balancer this socket was opened for.
///
/// The FRAMING the client wrote the request with is judged first ([`Framing`]): a framing this
/// load balancer's `routing.http.desync_mitigation_mode` refuses is answered by the LOAD
/// BALANCER here, with nothing forwarded, so no rule of its listener is evaluated and no
/// target is reached — and the CONNECTION it arrived on is not continued either
/// ([`CLOSE_CONNECTION`]): the bytes after a request whose end could not be agreed on are not
/// bytes this address can go on reading requests from.
async fn answer(
    callback: EmbeddedEmulator,
    framing: Framing,
    load_balancer_name: String,
    listener_port: Option<i32>,
    negotiated: NegotiatedTls,
    request: ::http::Request<::hyper::body::Incoming>,
) -> ::http::Response<Full<Bytes>> {
    let (parts, body) = request.into_parts();
    if framing.refuses() {
        let mut refused = generated(BAD_REQUEST, REFUSED_FRAMING);
        refused.headers_mut().insert(
            ::http::header::CONNECTION,
            ::http::HeaderValue::from_static(CLOSE_CONNECTION),
        );
        return refused;
    }
    // One permit per request IN FLIGHT, taken BEFORE a byte of the body is read: what the
    // bodies being read hold at once is then the in-flight cap times the ceiling below,
    // rather than the connection cap (or the number of load balancers) times it.
    let Ok(permit) = in_flight().try_acquire_owned() else {
        return generated(SERVICE_UNAVAILABLE, SERVER_BUSY);
    };
    let limited = Limited::new(body, body_ceiling());
    let served = ::tokio::time::timeout(request_deadline(), async move {
        // Held for the whole read and dispatch, and released with it.
        let _permit: OwnedSemaphorePermit = permit;
        let Ok(collected) = limited.collect().await else {
            return generated(PAYLOAD_TOO_LARGE, BODY_TOO_LARGE);
        };
        let Some(forwarded) = forwarded(
            parts,
            collected.to_bytes(),
            &load_balancer_name,
            listener_port,
            &negotiated,
        ) else {
            return generated(BAD_REQUEST, MALFORMED_REQUEST_TARGET);
        };
        match callback.http_connector().call(forwarded).await {
            Ok(response) => {
                let status = response.status().as_u16();
                let mut answer = ::http::Response::builder().status(status);
                for (name, value) in response.headers().iter() {
                    answer = answer.header(name, value);
                }
                let body = Bytes::from(
                    response
                        .into_body()
                        .bytes()
                        .map(<[u8]>::to_vec)
                        .unwrap_or_default(),
                );
                answer
                    .body(Full::new(body.clone()))
                    .unwrap_or_else(|_| ::http::Response::new(Full::new(body)))
            }
            // The data plane is this same process, so this is no answer about a load
            // balancer; it is reported the way the plane reports an answer it could not
            // produce.
            Err(_) => generated(BAD_GATEWAY, NO_LOAD_BALANCER),
        }
    })
    .await;
    served.unwrap_or_else(|_| generated(GATEWAY_TIMEOUT, REQUEST_TIMED_OUT))
}

/// The request the data plane is handed for one client request: the same method, body and
/// headers, addressed to this service's own endpoint under the reserved data-plane path of
/// `load_balancer_name`, and carrying the PORT of the listener the connection reached — so
/// the plane evaluates that listener's rules rather than the load balancer's lowest-numbered
/// one.
///
/// The host the CLIENT dialled travels beside it, in
/// [`crate::service::data_plane::FORWARDED_HOST_HEADER`]: re-addressing the leg is what lets
/// the plane resolve this model, and it must not cost the request the one thing a `#{host}`
/// substitution and a `host-header` condition are about. It is SET here, over anything the
/// client sent under that name ([`HEADERS_NOT_FORWARDED`]), so what the plane reads is the
/// address this socket was dialled at and nothing a client wrote.
///
/// The reserved prefix is what tells the plane WHICH load balancer must answer, and the
/// client's own path is appended to it, so a path that could take the request out from under
/// that prefix is not forwarded at all: a request target carrying a dot segment
/// ([`carries_a_dot_segment`]) renders as no forwarded request, and this address answers it
/// itself.
///
/// `None` when the client's own request does not render as one, which addresses nothing.
fn forwarded(
    parts: ::http::request::Parts,
    body: Bytes,
    load_balancer_name: &str,
    listener_port: Option<i32>,
    negotiated: &NegotiatedTls,
) -> Option<RtRequest> {
    let request_path = parts.uri.path();
    if carries_a_dot_segment(request_path) {
        return None;
    }
    let query = parts
        .uri
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let host = match listener_port {
        Some(port) => format!("{SERVICE_ENDPOINT}:{port}"),
        None => SERVICE_ENDPOINT.to_string(),
    };
    let prefix = crate::service::data_plane::data_plane_path(load_balancer_name);
    let uri = format!("https://{host}{prefix}{request_path}{query}");
    // The leg has to address the reserved path of THIS load balancer: a target that renders
    // to anything else addresses another plane, and is not forwarded.
    let addressed = uri.parse::<::http::Uri>().ok()?;
    if !addressed.path().starts_with(prefix.as_str()) {
        return None;
    }
    let mut builder = ::http::Request::builder()
        .method(parts.method.clone())
        .uri(uri)
        .header(::http::header::HOST, &host);
    if let Some(dialled) = parts
        .headers
        .get(::http::header::HOST)
        .or_else(|| parts.headers.get(crate::service::data_plane::FORWARDED_HOST_HEADER))
        .filter(|dialled| !dialled.is_empty())
    {
        builder = builder.header(crate::service::data_plane::FORWARDED_HOST_HEADER, dialled);
    }
    // What the handshake this connection ran negotiated, which only this end observed
    // ([`crate::service::data_plane::TLS_HEADERS`]): stated for a connection that ran one,
    // and for nothing else. The ADDRESS the connection was opened from travels on no header
    // at all: it belongs to the connection, and the plane reads it there
    // (`crate::service::data_plane`), so nothing a client writes can become it.
    for (name, value) in [
        (
            crate::service::data_plane::TLS_PROTOCOL_HEADER,
            negotiated.protocol.map(str::to_string),
        ),
        (
            crate::service::data_plane::TLS_CIPHER_HEADER,
            negotiated.cipher.map(str::to_string),
        ),
        (
            crate::service::data_plane::TLS_SERVER_NAME_HEADER,
            negotiated.server_name.clone(),
        ),
        (
            crate::service::data_plane::TLS_CERTIFICATE_ARN_HEADER,
            negotiated.certificate_arn.clone(),
        ),
        (
            crate::service::data_plane::TLS_CLIENT_CERTIFICATE_HEADER,
            negotiated.client_certificate.clone(),
        ),
    ] {
        if let Some(value) = value {
            builder = builder.header(name, value);
        }
    }
    for (name, value) in parts.headers.iter() {
        if HEADERS_NOT_FORWARDED.contains(&name.as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    let built = builder.body(SdkBody::from(body)).ok()?;
    RtRequest::try_from(built).ok()
}

/// Whether a request target carries a DOT SEGMENT — `.` or `..`, in the clear or
/// percent-encoded, read through the one decoding of a request path
/// ([`::smithy_server_runtime::percent_decode`]) so an encoded separator cannot hide one.
///
/// Such a target resolves, for anything that resolves it, to a path that need not stay under
/// the reserved prefix the forwarded leg addresses this load balancer's plane by — and that
/// prefix is the only thing that says whose listeners, rules and targets must answer. So it
/// is answered by this address rather than forwarded.
fn carries_a_dot_segment(request_path: &str) -> bool {
    ::smithy_server_runtime::percent_decode(request_path)
        .split('/')
        .any(|segment| segment == "." || segment == "..")
}

/// What this address answers a request it could not hand to the data plane at all: the
/// status a load balancer answers with when it produced the answer itself instead of a
/// target.
const BAD_GATEWAY: u16 = 502;

/// What it answers a request it will not forward at all: one carrying a dot segment, which
/// does not render as a request under this load balancer's own reserved path, and one whose
/// framing this load balancer's own desync mitigation mode refuses.
const BAD_REQUEST: u16 = 400;

/// What it answers a request that arrived while this service was already reading as many
/// request bodies as it reads at once.
const SERVICE_UNAVAILABLE: u16 = 503;

/// What it answers a request that did not finish within one request deadline.
const GATEWAY_TIMEOUT: u16 = 504;

/// What it answers a request carrying more than one connection may.
const PAYLOAD_TOO_LARGE: u16 = 413;

/// The body of an answer for a request shed at the in-flight cap.
const SERVER_BUSY: &str = "This address is serving as many requests as it can. Try again.\n";

/// The body of an answer for a request that reached no load balancer.
const NO_LOAD_BALANCER: &str = "This address serves no load balancer.\n";

/// The body of an answer for a request whose target this address will not forward.
const MALFORMED_REQUEST_TARGET: &str = "This request target cannot be served.\n";

/// The body of an answer for a request whose FRAMING the load balancer's own desync
/// mitigation mode refuses ([`crate::service::request_framing`]).
const REFUSED_FRAMING: &str = "This request is not framed as HTTP/1.1 states it.\n";

/// What such an answer states about the connection it is sent on: the request's own end was
/// not agreed on, so where the next one would begin is not either, and the load balancer does
/// not read further requests from it.
const CLOSE_CONNECTION: &str = "close";

/// The body of an answer for a request larger than this address accepts.
const BODY_TOO_LARGE: &str = "This request is larger than this address accepts.\n";

/// The body of an answer for a request that outlasted one request deadline.
const REQUEST_TIMED_OUT: &str = "This request took longer than this address allows.\n";

/// One answer of this address's own, for a request the data plane never saw. Plain text, as
/// a load balancer's own generated answers are.
fn generated(status: u16, message: &str) -> ::http::Response<Full<Bytes>> {
    let body = Bytes::from(message.to_string());
    ::http::Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Full::new(body.clone()))
        // Every argument is this function's own, so the builder cannot fail; answer a bare
        // status rather than panicking if it ever does.
        .unwrap_or_else(|_| {
            let mut response = ::http::Response::new(Full::new(body));
            *response.status_mut() = ::http::StatusCode::BAD_GATEWAY;
            response
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Material covering `names`, with the PEM text standing for the material: the
    /// selection below is about which entry is chosen, not about loading a key.
    fn material(names: &[&str]) -> Arc<CertificateKeyMaterial> {
        Arc::new(CertificateKeyMaterial {
            certificate_pem: String::new(),
            certificate_chain_pem: None,
            private_key_pem: String::new(),
            domain_names: names.iter().map(|name| (*name).to_string()).collect(),
        })
    }

    #[test]
    fn a_listener_with_no_loadable_certificate_has_no_configuration_of_its_own() {
        // Material whose PEM carries no certificate at all cannot be presented, so the
        // caller falls back to the core's own TLS server rather than failing the handshake.
        let certificates = vec![material(&["a.example.com"])];
        assert!(
            tls_server_config(&certificates, None, false).is_none(),
            "case material that loads no key: expected no configuration of this service's own"
        );
        assert!(
            tls_server_config(&[], None, false).is_none(),
            "case a listener with no certificate: expected no configuration of this service's own"
        );
    }

    #[test]
    fn a_listener_negotiates_the_versions_its_security_policy_publishes() {
        // The rows span what the reading generalises over: a listener that names no policy
        // (the default one), the default policy named, a policy that stops at TLS 1.2, a
        // policy that adds TLS 1.3, a TLS-1.3-only policy, a blank name, and a name no
        // published policy carries. The expectations are the published `SslProtocols` of
        // each policy, narrowed to the versions this emulator negotiates.
        let cases: &[(Option<&str>, &[&str])] = &[
            (None, &["TLSv1.2"]),
            (Some("ELBSecurityPolicy-2016-08"), &["TLSv1.2"]),
            (Some("ELBSecurityPolicy-TLS-1-2-2017-01"), &["TLSv1.2"]),
            (
                Some("ELBSecurityPolicy-TLS13-1-2-2021-06"),
                &["TLSv1.2", "TLSv1.3"],
            ),
            (Some("ELBSecurityPolicy-TLS13-1-3-2021-06"), &["TLSv1.3"]),
            (Some("   "), &["TLSv1.2"]),
            (Some("ELBSecurityPolicy-NoSuchPolicy"), &["TLSv1.2"]),
        ];
        for (policy, expected) in cases {
            let negotiated: Vec<::rustls::ProtocolVersion> = negotiated_versions(*policy)
                .iter()
                .map(|version| version.version)
                .collect();
            let expected_versions: Vec<::rustls::ProtocolVersion> = expected
                .iter()
                .map(|name| match *name {
                    TLS_1_3 => ::rustls::ProtocolVersion::TLSv1_3,
                    _ => ::rustls::ProtocolVersion::TLSv1_2,
                })
                .collect();
            assert_eq!(
                negotiated, expected_versions,
                "case {policy:?}: expected {expected_versions:?}, got {negotiated:?}"
            );
        }
    }

    #[test]
    fn the_forwarded_leg_addresses_the_plane_under_the_reserved_path() {
        // The rows span what the forwarding generalises over: a listener whose port is
        // known and one whose is not, a path with and without a query, and the root path.
        let cases: &[(&str, Option<i32>, &str, &str)] = &[
            (
                "/index.html",
                Some(443),
                "https://elasticloadbalancingv2.amazonaws.com:443/_aws/elasticloadbalancing/my-alb/index.html",
                "elasticloadbalancingv2.amazonaws.com:443",
            ),
            (
                "/a/b?q=1",
                Some(80),
                "https://elasticloadbalancingv2.amazonaws.com:80/_aws/elasticloadbalancing/my-alb/a/b?q=1",
                "elasticloadbalancingv2.amazonaws.com:80",
            ),
            (
                "/",
                None,
                "https://elasticloadbalancingv2.amazonaws.com/_aws/elasticloadbalancing/my-alb/",
                "elasticloadbalancingv2.amazonaws.com",
            ),
        ];
        for (requested, port, expected_uri, expected_host) in cases {
            let request = ::http::Request::builder()
                .method("GET")
                .uri(*requested)
                .header("host", "127.0.0.1:52811")
                .header("x-client", "kept")
                .body(())
                .expect("the case's own request");
            let (parts, ()) = request.into_parts();
            let forwarded = forwarded(
                parts,
                Bytes::new(),
                "my-alb",
                *port,
                &NegotiatedTls::default(),
            )
            .unwrap_or_else(|| panic!("case {requested:?}: the request must forward"));
            assert_eq!(
                forwarded.uri().to_string(),
                *expected_uri,
                "case {requested:?}: expected {expected_uri}, got {}",
                forwarded.uri()
            );
            assert_eq!(
                forwarded.headers().get("host").map(|host| host.to_string()),
                Some((*expected_host).to_string()),
                "case {requested:?}: expected the host {expected_host}, got {:?}",
                forwarded.headers().get("host")
            );
            assert_eq!(
                forwarded.headers().get("x-client").map(ToString::to_string),
                Some("kept".to_string()),
                "case {requested:?}: expected the client's own headers forwarded"
            );
            // The host the CLIENT dialled travels beside the re-addressed one, so the plane
            // reads `#{host}` and a `host-header` condition off the name the client used.
            assert_eq!(
                forwarded
                    .headers()
                    .get(crate::service::data_plane::FORWARDED_HOST_HEADER)
                    .map(ToString::to_string),
                Some("127.0.0.1:52811".to_string()),
                "case {requested:?}: expected the client's own host carried, got {:?}",
                forwarded
                    .headers()
                    .get(crate::service::data_plane::FORWARDED_HOST_HEADER)
            );
        }
    }

    /// A request TARGET whose resolved form need not stay under the reserved prefix the leg
    /// addresses this load balancer's plane by is not forwarded at all: the prefix is what
    /// says whose listeners, rules and targets must answer.
    #[test]
    fn a_target_carrying_a_dot_segment_renders_as_no_forwarded_request() {
        // The rows span the spellings of a dot segment the wire carries — `..` and `.` at the
        // front, in the middle and at the end, percent-encoded in either case — beside the
        // ordinary paths a client sends, which are forwarded, including one whose segments
        // merely CONTAIN dots.
        let cases: &[(&str, bool)] = &[
            ("/index.html", true),
            ("/a/b?q=1", true),
            ("/", true),
            ("/a.b.c/d", true),
            ("/..%2f_aws/execute-api/abc", false),
            ("/../other", false),
            ("/a/../b", false),
            ("/a/..", false),
            ("/./a", false),
            ("/a/%2E%2E/b", false),
            ("/a/%2e/b", false),
        ];
        for (requested, forwards) in cases {
            let request = ::http::Request::builder()
                .method("GET")
                .uri(*requested)
                .header("host", "127.0.0.1:52811")
                .body(())
                .expect("the case's own request");
            let (parts, ()) = request.into_parts();
            let rendered = forwarded(
                parts,
                Bytes::new(),
                "my-alb",
                Some(80),
                &NegotiatedTls::default(),
            );
            assert_eq!(
                rendered.is_some(),
                *forwards,
                "case {requested:?}: expected it to forward {forwards}, got {:?}",
                rendered.map(|request| request.uri().to_string())
            );
        }
    }

    /// RULE: the facts of the CONNECTION travel on the leg as this end observed them, and a
    /// value a client wrote under one of those names never does.
    #[test]
    fn the_leg_states_what_the_handshake_negotiated_and_nothing_a_client_wrote() {
        // The rows span what the observation generalises over: a connection that negotiated
        // everything (protocol, cipher, a server name, a certificate and a certificate the
        // CLIENT presented), one whose client asked for no name and presented none, and a
        // connection in the clear, which negotiated nothing.
        //
        // The presented chain is URL-encoded PEM, which is what the handshake hands this leg
        // ([`crate::service::client_certificates::presented_chain`]).
        const PRESENTED: &str = "-----BEGIN%20CERTIFICATE-----%0AMIIBkTCB%2Bw%3D%3D%0A\
                                 -----END%20CERTIFICATE-----%0A";
        let cases: &[(&str, NegotiatedTls, [Option<&str>; 5])] = &[
            (
                "a client that asked for a name",
                NegotiatedTls {
                    protocol: Some(TLS_1_3),
                    cipher: Some("TLS_AES_128_GCM_SHA256"),
                    server_name: Some("api.example.com".to_string()),
                    certificate_arn: Some("arn:aws:acm:us-west-2:111122223333:certificate/c1"
                        .to_string()),
                    client_certificate: Some(PRESENTED.to_string()),
                },
                [
                    Some(TLS_1_3),
                    Some("TLS_AES_128_GCM_SHA256"),
                    Some("api.example.com"),
                    Some("arn:aws:acm:us-west-2:111122223333:certificate/c1"),
                    Some(PRESENTED),
                ],
            ),
            (
                "a client that asked for no name",
                NegotiatedTls {
                    protocol: Some(TLS_1_2),
                    cipher: Some("ECDHE-RSA-AES128-GCM-SHA256"),
                    server_name: None,
                    certificate_arn: Some("arn:aws:acm:us-west-2:111122223333:certificate/c2"
                        .to_string()),
                    client_certificate: None,
                },
                [
                    Some(TLS_1_2),
                    Some("ECDHE-RSA-AES128-GCM-SHA256"),
                    None,
                    Some("arn:aws:acm:us-west-2:111122223333:certificate/c2"),
                    None,
                ],
            ),
            (
                "a connection in the clear",
                NegotiatedTls::default(),
                [None, None, None, None, None],
            ),
        ];
        for (case, negotiated, expected) in cases {
            // Every one of those names, forged by the client: none of them may travel.
            let mut builder = ::http::Request::builder()
                .method("GET")
                .uri("/")
                .header("host", "127.0.0.1:52811");
            for name in crate::service::data_plane::TLS_HEADERS {
                builder = builder.header(name, "forged-by-the-client");
            }
            let (parts, ()) = builder.body(()).expect("the case's own request").into_parts();
            let forwarded = forwarded(parts, Bytes::new(), "my-alb", Some(443), negotiated)
                .unwrap_or_else(|| panic!("case {case:?}: the request must forward"));
            for (name, expected) in crate::service::data_plane::TLS_HEADERS
                .iter()
                .zip(expected.iter())
            {
                let carried = forwarded.headers().get(*name).map(ToString::to_string);
                assert_eq!(
                    carried.as_deref(),
                    *expected,
                    "case {case:?}: expected {name} to carry {expected:?}, got {carried:?}"
                );
            }
        }
    }

    /// RULE: every protocol version and cipher suite this emulator's TLS implementation can
    /// negotiate has a name to report, and that name is the one the published security-policy
    /// catalogue spells it with.
    #[test]
    fn every_negotiable_protocol_and_cipher_has_the_published_name() {
        let versions: &[(::rustls::ProtocolVersion, Option<&str>)] = &[
            (::rustls::ProtocolVersion::TLSv1_2, Some(TLS_1_2)),
            (::rustls::ProtocolVersion::TLSv1_3, Some(TLS_1_3)),
            // A version no listener of this emulator completes a handshake under is named
            // by no published policy, so nothing is reported for it.
            (::rustls::ProtocolVersion::TLSv1_0, None),
            (::rustls::ProtocolVersion::SSLv3, None),
        ];
        for (version, expected) in versions {
            let named = protocol_name(*version);
            assert_eq!(
                named, *expected,
                "case {version:?}: expected {expected:?}, got {named:?}"
            );
        }
        // Every suite the provider this tree terminates handshakes with can negotiate: each
        // has a name, and each name is one `DescribeSSLPolicies` publishes — except the two
        // TLS 1.2 ChaCha20 suites, which no predefined policy of this service offers and
        // which are therefore named by the same OpenSSL convention without appearing in it.
        let unpublished = [
            "ECDHE-ECDSA-CHACHA20-POLY1305",
            "ECDHE-RSA-CHACHA20-POLY1305",
        ];
        for suite in ::rustls::crypto::aws_lc_rs::ALL_CIPHER_SUITES {
            let suite = suite.suite();
            let named = cipher_name(suite);
            let Some(named) = named else {
                panic!("case {suite:?}: expected a name for a suite this emulator negotiates");
            };
            assert!(
                crate::ops::describe_ssl_policies::publishes_cipher(named)
                    || unpublished.contains(&named),
                "case {suite:?}: expected {named} to be spelled as the published catalogue \
                 spells a cipher"
            );
        }
    }

    /// RULE: the certificate reported for a handshake is the one the selection presented —
    /// the first entry whose own names cover the name asked for, and the listener's default
    /// certificate for a name none of them covers or for no name at all.
    #[test]
    fn the_reported_certificate_is_the_one_the_selection_presents() {
        let held: Vec<(String, Arc<CertificateKeyMaterial>)> = vec![
            ("arn:default".to_string(), material(&["default.example.com"])),
            ("arn:listed".to_string(), material(&["listed.example.net"])),
            ("arn:wildcard".to_string(), material(&["*.apps.example.org"])),
        ];
        // The rows span what the selection generalises over: the default certificate's own
        // name, a certificate-list entry's name, a label under a wildcard entry, a name no
        // entry covers, no name at all, and a listener presenting nothing.
        type Presented = [(String, Arc<CertificateKeyMaterial>)];
        let nothing: &Presented = &[];
        let cases: &[(&str, Option<&str>, &Presented, Option<&str>)] = &[
            ("the default certificate's name", Some("default.example.com"), &held, Some("arn:default")),
            ("a certificate-list entry", Some("listed.example.net"), &held, Some("arn:listed")),
            ("a label under a wildcard", Some("api.apps.example.org"), &held, Some("arn:wildcard")),
            ("a name no entry covers", Some("other.example.com"), &held, Some("arn:default")),
            ("no name at all", None, &held, Some("arn:default")),
            ("a listener presenting nothing", Some("default.example.com"), nothing, None),
        ];
        for (case, asked_for, certificates, expected) in cases {
            let chosen = chosen_certificate(certificates, *asked_for);
            assert_eq!(
                chosen.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {chosen:?}"
            );
        }
    }
}
