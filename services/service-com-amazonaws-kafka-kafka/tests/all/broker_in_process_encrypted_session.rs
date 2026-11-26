// SPDX-License-Identifier: CC0-1.0

//! The ENCRYPTED bootstrap addresses, reached by a client running INSIDE this process — the
//! one vantage point an Amazon MSK data plane is reachable from.
//!
//! RULE: "Amazon MSK brokers are only accessible from within the cluster's VPC", so the client
//! that reads a bootstrap string and dials it is code this emulator itself runs (a Lambda
//! function attached to that VPC, which opens no host socket and reaches a native plane through
//! the register of the planes this emulator serves in process). Every access method a cluster
//! publishes must therefore be fully usable over THAT connection and not only over a host
//! socket: an address published for an encrypted method completes the TLS handshake the method
//! is served over, and the session on it is served the frames a client sends before it
//! authenticates — `ApiVersions`, and the `SaslHandshake` that answers which mechanisms the
//! method offers.
//!
//! This is the half of the in-process reachability `broker_plane_in_process_dial` leaves open:
//! that suite asserts an encrypted address RESOLVES, and stops at the handshake. A method whose
//! address resolves but whose TLS never completes over an in-process connection leaves every
//! in-VPC client — the only kind there is — hanging in bring-up with no error to report.
//!
//! The rows differ along what the rule generalises over: which access method the published
//! member names (the TLS path of a provisioned cluster, the SASL/SCRAM path beside it, and the
//! SASL/IAM path of a serverless cluster, which is the only method such a cluster has), and
//! therefore which mechanisms the handshake must offer.

use std::collections::BTreeMap;

use ::bytes::{Bytes, BytesMut};
use ::kafka_protocol::messages::{
    ApiKey, ApiVersionsRequest, ApiVersionsResponse, RequestHeader, ResponseHeader,
    SaslHandshakeRequest, SaslHandshakeResponse,
};
use ::kafka_protocol::protocol::{Decodable, Encodable, StrBytes};
use ::service_common::dialable_data_planes::DialablePlanes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::broker_data_plane::{emulator_tls_connector, AWS_MSK_IAM, OAUTHBEARER, SCRAM_SHA_512};
use crate::kafka_harness::{ClusterRequest, Emulator};

/// The client id the sessions below identify themselves with.
const CLIENT_ID: &str = "in-process-encrypted-client";

/// The server name the TLS client asks for: the core's listener certificate names loopback,
/// and the address published for a plane this emulator opened is an address of this machine.
const SERVER_NAME: &str = "localhost";

/// One row: the case, the cluster to stand up, and the mechanisms each published member's
/// handshake must offer — keyed by the member of `GetBootstrapBrokers` that names it.
struct Row {
    /// The case, named in every assertion of the row.
    case: &'static str,
    /// Whether the cluster is a SERVERLESS one, whose only method is SASL/IAM.
    serverless: bool,
    /// The provisioned cluster to create, for a row that is not serverless.
    request: Option<ClusterRequest>,
    /// The members this row drives, and the SASL mechanisms each one must offer: a method
    /// that authenticates no client offers none, which is what an empty list is.
    members: &'static [(&'static str, &'static [&'static str])],
}

/// The rows: the encrypted methods a cluster publishes, over both cluster types.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a provisioned cluster reached over TLS",
            serverless: false,
            request: Some(ClusterRequest {
                client_broker: Some("TLS"),
                ..ClusterRequest::named("in-process-tls")
            }),
            members: &[("bootstrapBrokerStringTls", &[])],
        },
        Row {
            case: "a provisioned cluster authenticating clients with SASL/SCRAM and IAM",
            serverless: false,
            request: Some(ClusterRequest {
                client_broker: Some("TLS"),
                sasl_scram: Some(true),
                sasl_iam: Some(true),
                ..ClusterRequest::named("in-process-sasl")
            }),
            members: &[
                ("bootstrapBrokerStringSaslScram", &[SCRAM_SHA_512]),
                (
                    "bootstrapBrokerStringSaslIam",
                    &[AWS_MSK_IAM, OAUTHBEARER],
                ),
            ],
        },
        Row {
            case: "a serverless cluster, whose clients are admitted through SASL/IAM alone",
            serverless: true,
            request: None,
            members: &[(
                "bootstrapBrokerStringSaslIam",
                &[AWS_MSK_IAM, OAUTHBEARER],
            )],
        },
    ]
}

/// Every `host:port` pair of every bootstrap string a cluster publishes, member by member.
async fn published(
    emulator: &Emulator,
    cluster: &str,
    case: &str,
) -> BTreeMap<String, Vec<String>> {
    let reply = emulator.bootstrap_brokers(cluster).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: GetBootstrapBrokers must answer, got {} {}",
        reply.status, reply.body
    );
    reply
        .body
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(member, value)| {
            value.as_str().map(|published| {
                (
                    member,
                    published
                        .split(',')
                        .map(|pair| pair.trim().to_string())
                        .collect(),
                )
            })
        })
        .collect()
}

/// The host and port one published authority spells.
fn split(authority: &str, case: &str) -> (String, u16) {
    let (host, port) = authority.rsplit_once(':').unwrap_or_else(|| {
        panic!("case {case:?}: a published address carries a port: {authority}")
    });
    let port = port
        .parse::<u16>()
        .unwrap_or_else(|_| panic!("case {case:?}: a published port is a number: {authority}"));
    (
        host.trim_start_matches('[').trim_end_matches(']').to_string(),
        port,
    )
}

/// One Kafka session on a byte stream, framed as the protocol frames them.
struct Session<Stream> {
    wire: Stream,
    correlation: i32,
}

impl<Stream: AsyncRead + AsyncWrite + Unpin> Session<Stream> {
    /// Send one request and read its response back, decoded in the same version.
    async fn call<Request: Encodable, Response: Decodable>(
        &mut self,
        case: &str,
        key: ApiKey,
        version: i16,
        request: &Request,
    ) -> Response {
        self.correlation += 1;
        let mut frame = BytesMut::new();
        RequestHeader::default()
            .with_request_api_key(key as i16)
            .with_request_api_version(version)
            .with_correlation_id(self.correlation)
            .with_client_id(Some(StrBytes::from_static_str(CLIENT_ID)))
            .encode(&mut frame, key.request_header_version(version))
            .unwrap_or_else(|error| panic!("case {case:?}: the header must encode: {error}"));
        request
            .encode(&mut frame, version)
            .unwrap_or_else(|error| panic!("case {case:?}: the request must encode: {error}"));
        let length = u32::try_from(frame.len()).expect("a request frame fits in a u32");
        self.wire
            .write_all(&length.to_be_bytes())
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the frame must be written: {error}"));
        self.wire
            .write_all(&frame)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the frame must be written: {error}"));
        self.wire
            .flush()
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the frame must be flushed: {error}"));
        let mut answered = self.read_frame(case).await;
        ResponseHeader::decode(&mut answered, key.response_header_version(version))
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the response header must decode: {error}")
            });
        Response::decode(&mut answered, version)
            .unwrap_or_else(|error| panic!("case {case:?}: the response must decode: {error}"))
    }

    /// The next response frame the broker wrote.
    async fn read_frame(&mut self, case: &str) -> Bytes {
        let mut length = [0_u8; 4];
        self.wire
            .read_exact(&mut length)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the broker must answer: {error}"));
        let length = u32::from_be_bytes(length) as usize;
        assert!(
            length > 0 && length < 8 * 1024 * 1024,
            "case {case:?}: the broker answered a frame of {length} bytes"
        );
        let mut frame = vec![0_u8; length];
        self.wire
            .read_exact(&mut frame)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the frame must be read: {error}"));
        Bytes::from(frame)
    }
}

/// RULE: an address published for an ENCRYPTED access method completes the TLS handshake over
/// a connection an in-process client opened through the register of this emulator's planes, and
/// the session on it is served the frames a client sends before it authenticates — so an in-VPC
/// client reaches the method it was told to use rather than hanging on its handshake.
#[tokio::test]
async fn an_in_process_client_completes_a_session_on_every_encrypted_published_address() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();
        let cluster = match (&row.request, row.serverless) {
            (Some(request), _) => emulator.create_ok(request).await,
            (None, _) => {
                emulator
                    .create_serverless_ok("in-process-serverless", &[vec!["subnet-a", "subnet-b"]], None)
                    .await
            }
        };
        // LOOKS AGAIN until the method's address is published rather than waiting out a span:
        // the move out of `CREATING` is a transition the clock owes the cluster, and
        // `GetBootstrapBrokers` refuses while it is still under way. Read for the first member
        // the row drives, which is one every row's cluster publishes.
        let first_member = row
            .members
            .first()
            .map(|(member, _mechanisms)| *member)
            .unwrap_or_default();
        emulator
            .published_bootstrap(&cluster, first_member, case)
            .await;
        let addresses = published(&emulator, &cluster, case).await;
        let planes = DialablePlanes::of(&*emulator.engine)
            .expect("the engine vends the register of the planes it serves");
        for (member, mechanisms) in row.members {
            let pairs = addresses.get(*member).cloned().unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the cluster to publish {member}, got {:?}",
                    addresses.keys().collect::<Vec<_>>()
                )
            });
            for authority in &pairs {
                let probe = format!("{case} / {member} / {authority}");
                let (host, port) = split(authority, &probe);
                let dialed = planes.dialable(&host, port).unwrap_or_else(|| {
                    panic!(
                        "case {probe:?}: the published address must be one an in-process client \
                         can reach"
                    )
                });
                let wire = dialed.plane.dial(None).await.unwrap_or_else(|error| {
                    panic!("case {probe:?}: the plane must open a connection: {error}")
                });
                // The TLS the method is served over, run as the CLIENT over that same
                // in-process connection: the plane reads the handshake's first byte off it
                // exactly as it does off a socket.
                let server_name = ::rustls::pki_types::ServerName::try_from(SERVER_NAME)
                    .expect("loopback is a server name");
                let stream = emulator_tls_connector()
                    .connect(server_name, wire)
                    .await
                    .unwrap_or_else(|error| {
                        panic!(
                            "case {probe:?}: the published address must complete the TLS \
                             handshake its method is served over, got {error}"
                        )
                    });
                let mut session = Session {
                    wire: stream,
                    correlation: 0,
                };
                let versions: ApiVersionsResponse = session
                    .call(
                        &probe,
                        ApiKey::ApiVersions,
                        3,
                        &ApiVersionsRequest::default()
                            .with_client_software_name(StrBytes::from_static_str(CLIENT_ID))
                            .with_client_software_version(StrBytes::from_static_str("1")),
                    )
                    .await;
                assert_eq!(
                    versions.error_code, 0,
                    "case {probe:?}: expected ApiVersions to be answered over the encrypted \
                     session, got error {}",
                    versions.error_code
                );
                assert!(
                    !versions.api_keys.is_empty(),
                    "case {probe:?}: expected the broker to report the APIs it serves"
                );
                // And the mechanisms the method offers, which is what a client reads before
                // it authenticates: the handshake answers the method's own list, and names
                // the mechanism a client of it must use.
                let handshake: SaslHandshakeResponse = session
                    .call(
                        &probe,
                        ApiKey::SaslHandshake,
                        1,
                        &SaslHandshakeRequest::default()
                            .with_mechanism(StrBytes::from_string(
                                mechanisms.first().unwrap_or(&"PLAIN").to_string(),
                            )),
                    )
                    .await;
                let offered: Vec<String> = handshake
                    .mechanisms
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                let expected: Vec<String> =
                    mechanisms.iter().map(ToString::to_string).collect();
                assert_eq!(
                    offered, expected,
                    "case {probe:?}: expected the handshake to offer {expected:?}, got \
                     {offered:?}"
                );
                match mechanisms.is_empty() {
                    // A method that authenticates no client offers no mechanism, so the
                    // handshake refuses the one this client asked for.
                    true => assert_ne!(
                        handshake.error_code, 0,
                        "case {probe:?}: expected a method that offers no mechanism to refuse \
                         the handshake, got error {}",
                        handshake.error_code
                    ),
                    false => assert_eq!(
                        handshake.error_code, 0,
                        "case {probe:?}: expected the method's own mechanism to be accepted, \
                         got error {}",
                        handshake.error_code
                    ),
                }
            }
        }
    }
}
