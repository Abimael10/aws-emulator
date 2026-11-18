// SPDX-License-Identifier: CC0-1.0

//! The bootstrap address of a cluster, reached by a client running INSIDE this process.
//!
//! RULE: "Amazon MSK brokers are only accessible from within the cluster's VPC", so the client
//! that reads a bootstrap string and dials it is often code this emulator itself runs — the
//! code of a Lambda function attached to that VPC, which opens no host socket and reaches a
//! native data plane through the register of the planes this emulator serves in process
//! (`service_common::dialable_data_planes`). An address `GetBootstrapBrokers` publishes has to
//! be one such a client can reach: every published `host:port` resolves in that register to
//! this service, and a connection opened on it is served by the SAME broker, over the same log,
//! that a client dialling the socket is served by. An address this emulator opened no plane at
//! resolves to nothing.
//!
//! The rows differ along what the rule generalises over: which access path the published member
//! names (in the clear, over TLS, over each SASL path), and the addresses that answer nothing
//! (another port of the same host, a host nothing was opened at).
//!
//! The in-process connection below carries a real Apache Kafka session — `ApiVersions` and
//! `Metadata`, framed by the `kafka-protocol` crate — so what is asserted is the broker's own
//! answer rather than the register's bookkeeping.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ::bytes::{Bytes, BytesMut};
use ::kafka_protocol::messages::{
    ApiKey, ApiVersionsRequest, ApiVersionsResponse, MetadataRequest, MetadataResponse,
    RequestHeader, ResponseHeader,
};
use ::kafka_protocol::protocol::{Decodable, Encodable, StrBytes};
use ::service_common::dialable_data_planes::DialablePlanes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::kafka_harness::{poll_until_active, ClusterRequest, Emulator};

/// The client id the in-process session below identifies itself with.
const CLIENT_ID: &str = "in-process-client";

/// The service the register must name as answering at every published address: this service's
/// own SigV4 signing name (`sigv4Name` `kafka`).
const SERVED_BY: &str = "kafka";

/// The bootstrap strings a cluster publishes, member by member.
async fn published(emulator: &Emulator, cluster: &str, case: &str) -> BTreeMap<String, String> {
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
        .filter_map(|(member, value)| value.as_str().map(|address| (member, address.to_string())))
        .collect()
}

/// The host and port one published authority spells.
fn split(authority: &str, case: &str) -> (String, u16) {
    let (host, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: a published address carries a port: {authority}"));
    let port = port
        .parse::<u16>()
        .unwrap_or_else(|_| panic!("case {case:?}: a published port is a number: {authority}"));
    (
        host.trim_start_matches('[').trim_end_matches(']').to_string(),
        port,
    )
}

/// One in-process session on one published address: the client end of a pipe the plane's own
/// handler is serving.
struct Session {
    wire: ::emulator_server::ListenerConn,
    correlation: i32,
}

impl Session {
    /// Open a connection to `authority` through the register, the one way a client that opens
    /// no host socket reaches a native plane.
    async fn opened(planes: &DialablePlanes, authority: &str, case: &str) -> Self {
        let (host, port) = split(authority, case);
        let dialed = planes.dialable(&host, port).unwrap_or_else(|| {
            panic!(
                "case {case:?}: the address {authority} the control plane published must be one \
                 an in-process client can reach"
            )
        });
        assert_eq!(
            dialed.served_by, SERVED_BY,
            "case {case:?}: expected {authority} to be served by {SERVED_BY}, got {}",
            dialed.served_by
        );
        let wire = dialed
            .plane
            .dial(None)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the plane must open a connection: {error}"));
        Self {
            wire,
            correlation: 0,
        }
    }

    /// Send one request and read its response, framed as the protocol frames them: a length,
    /// the request header for this API and version, and the body.
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

/// RULE: every address `GetBootstrapBrokers` publishes resolves, in the register of the planes
/// this emulator serves in process, to a plane of this service — and a connection opened on the
/// one published for the plaintext path carries a real Kafka session, answered by the same
/// broker a client of the socket reaches, which reports the published address as its own.
#[tokio::test]
async fn a_published_bootstrap_address_is_reachable_by_an_in_process_client() {
    // The rows differ along the access paths the cluster enables, which is what decides WHICH
    // members are published and what each one requires of a client.
    let rows: Vec<(&str, ClusterRequest)> = vec![
        (
            "a cluster reached in the clear and over TLS",
            ClusterRequest::named("in-process-plaintext"),
        ),
        (
            "a cluster authenticating clients with IAM access control",
            ClusterRequest {
                client_broker: Some("TLS"),
                sasl_iam: Some(true),
                ..ClusterRequest::named("in-process-iam")
            },
        ),
    ];
    for (case, request) in &rows {
        let emulator = Emulator::new();
        let cluster = emulator.create_ok(request).await;
        poll_until_active(&emulator, &cluster, case).await;
        let addresses = published(&emulator, &cluster, case).await;
        assert!(
            !addresses.is_empty(),
            "case {case:?}: a cluster with brokers must publish a bootstrap string"
        );
        let planes = DialablePlanes::of(&*emulator.engine)
            .expect("the engine vends the register of the planes it serves");
        for (member, authority) in &addresses {
            let probe = format!("{case} / {member}");
            // Reachable: the address resolves to a plane this service opened, whichever access
            // path the member names.
            let mut session = Session::opened(&planes, authority, &probe).await;
            if member.contains("Tls") || member.contains("Sasl") {
                // A client of an encrypted path opens a TLS handshake on the connection
                // instead of a Kafka frame, which is the wire that path is served on; what
                // this row asserts is that the address is reachable at all.
                continue;
            }
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
                "case {probe:?}: expected the broker to answer ApiVersions, got error code {}",
                versions.error_code
            );
            assert!(
                !versions.api_keys.is_empty(),
                "case {probe:?}: expected the broker to report the APIs it serves"
            );
            // And the broker it reports itself as is the address the control plane published,
            // so a client that re-resolves the cluster from its metadata keeps talking to this
            // same plane.
            let metadata: MetadataResponse = session
                .call(
                    &probe,
                    ApiKey::Metadata,
                    9,
                    &MetadataRequest::default().with_topics(None),
                )
                .await;
            let (host, port) = split(authority, &probe);
            let reported: Vec<String> = metadata
                .brokers
                .iter()
                .map(|broker| format!("{}:{}", broker.host.as_str(), broker.port))
                .collect();
            let expected = format!("{host}:{port}");
            assert!(
                !reported.is_empty() && reported.iter().all(|broker| *broker == expected),
                "case {probe:?}: expected every broker reported to be the published address \
                 {expected}, got {reported:?}"
            );
        }
    }
}

/// RULE: only the addresses this emulator opened a plane at resolve in that register; an
/// address it opened none at answers nothing, so an in-process client dialling one is answered
/// the way a host with nothing listening answers it.
#[tokio::test]
async fn an_address_no_cluster_was_opened_at_resolves_to_nothing() {
    let case = "a cluster reached in the clear";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest::named("in-process-unopened"))
        .await;
    poll_until_active(&emulator, &cluster, case).await;
    let addresses = published(&emulator, &cluster, case).await;
    let published_authority = addresses
        .values()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: a cluster with brokers publishes an address"));
    let (host, port) = split(published_authority, case);
    let planes = DialablePlanes::of(&*emulator.engine)
        .expect("the engine vends the register of the planes it serves");
    let rows: Vec<(&str, String, u16)> = vec![
        (
            "another port of the same host",
            host.clone(),
            port.checked_add(1).unwrap_or(port - 1),
        ),
        ("a host nothing was opened at", "10.0.7.9".to_string(), port),
        (
            "a name of AWS's own broker naming",
            format!("b-1.{}.kafka.us-west-2.amazonaws.com", "in-process-unopened"),
            port,
        ),
    ];
    for (row, host, port) in rows {
        let dialed = planes.dialable(&host, port);
        assert!(
            dialed.is_none(),
            "case {row:?}: expected {host}:{port} to answer nothing, got {dialed:?}"
        );
    }
    // The published address itself still answers, so the rows above failed on the address they
    // named and not on the register.
    assert!(
        planes.dialable(&host, port).is_some(),
        "case {case:?}: the published address {published_authority} must stay reachable"
    );
}

/// RULE: the record goes with the listener the cluster held, so once the cluster is gone
/// nothing resolves to the address it published — an in-process client dialling it is then
/// answered the way a host with nothing listening answers one.
#[tokio::test]
async fn the_published_address_is_withdrawn_with_the_cluster() {
    let case = "a deleted cluster";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest::named("in-process-withdrawn"))
        .await;
    poll_until_active(&emulator, &cluster, case).await;
    let addresses = published(&emulator, &cluster, case).await;
    let authority = addresses
        .values()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: a cluster with brokers publishes an address"))
        .clone();
    let (host, port) = split(&authority, case);
    let planes = DialablePlanes::of(&*emulator.engine)
        .expect("the engine vends the register of the planes it serves");
    assert!(
        planes.dialable(&host, port).is_some(),
        "case {case:?}: the published address must be reachable while the cluster holds it"
    );
    let deleted = emulator
        .call(
            "DELETE",
            &format!("/v1/clusters/{}", crate::kafka_harness::pct(&cluster)),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: DeleteCluster must be accepted, got {} {}",
        deleted.status, deleted.body
    );
    // The deletion is a span the CLOCK owes the cluster, so the read is made again until the
    // cluster is gone rather than waited out; the deadline is far longer than that span, so it
    // says nothing about how fast the host runs.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let described = emulator.describe(&cluster).await;
        if described.status == 404 && planes.dialable(&host, port).is_none() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the address {authority} must stop answering once the cluster is \
             gone, last read {} {}",
            described.status,
            described.body
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
