// SPDX-License-Identifier: CC0-1.0

//! Whether a published bootstrap address is one a CLIENT can reach: an address of this
//! emulator's own, taken exactly as the control plane wrote it.
//!
//! RULE under test: the `host:port` a service publishes for a caller to connect to names
//! an address THIS emulator answers that protocol at — one of the hosts this process is
//! reached at, never a name of AWS's own naming and never a name this emulator invents
//! that nothing outside it resolves. A client hands the pair it was given to its resolver
//! and its socket, so a host only this emulator's own tests could resolve is an address
//! nothing serves.
//!
//! What this asserts, for every pair of every bootstrap string a cluster publishes:
//!
//! * the host is one of THIS emulator's — the interface the core binds its listeners on
//!   (`service_common::data_plane_addressing::names_the_loopback_interface`) — and the port
//!   is one the core assigned;
//! * the pair resolves, in the register of the planes this emulator serves in process
//!   (`service_common::dialable_data_planes`), to a plane of this service, and a Kafka
//!   session on it is answered — the vantage point of the client that reads a bootstrap
//!   string, since "Amazon MSK brokers are only accessible from within the cluster's VPC";
//! * that address answers the Apache Kafka protocol on the wire the access method is
//!   served over.
//!
//! The same holds for the addresses the BROKER itself reports once a client is connected:
//! `Metadata`'s broker list and the coordinator `FindCoordinator` names, since those are
//! the addresses a client then dials.
//!
//! The rows differ along what the rule generalises over: the kind of cluster (MSK
//! Serverless and provisioned), the number of broker nodes behind the published address
//! (one, two, three), and which access method the published member names (in the clear,
//! over TLS, over SASL/SCRAM and over SASL/IAM) — so the address is asserted on every
//! member a cluster publishes rather than on the one a single row happens to carry.

use std::collections::BTreeSet;

use ::kafka_protocol::messages::{
    ApiKey, FindCoordinatorRequest, FindCoordinatorResponse, MetadataRequest, MetadataResponse,
};
use ::kafka_protocol::protocol::StrBytes;
use ::service_common::dialable_data_planes::DialablePlanes;

use crate::bootstrap_broker_cardinality::{published, split};
use crate::broker_data_plane::Client;
use crate::kafka_harness::{ClusterRequest, Emulator};

/// The service the register must name as answering at every published address: this
/// service's own SigV4 signing name (`sigv4Name` `kafka`).
const SERVED_BY: &str = "kafka";

/// Assert `address` is an address of this emulator's own — a host on the interface the
/// core binds its listeners on and a port the core assigned — and that the client that
/// reads it reaches a plane of this service there.
///
/// The address goes to the register unchanged, so what is asserted is the authority the
/// control plane published and not one this suite rewrote. Whether a host is this
/// emulator's own is the workspace's one reading of that question
/// (`service_common::data_plane_addressing::names_the_loopback_interface`).
async fn assert_reachable(planes: &DialablePlanes, address: &str, case: &str) {
    let (host, port) = split(address, case);
    assert!(
        ::service_common::data_plane_addressing::names_the_loopback_interface(&host),
        "case {case:?}: expected a host this emulator answers at, got {address:?}"
    );
    assert!(
        port > 0,
        "case {case:?}: expected the port the broker answers on, got {address:?}"
    );
    let dialed = planes
        .dialable(host.trim_start_matches('[').trim_end_matches(']'), port)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the address {address} the control plane published must be one a \
                 client that reads it can reach"
            )
        });
    assert_eq!(
        dialed.served_by, SERVED_BY,
        "case {case:?}: expected {address} to be served by {SERVED_BY}, got {}",
        dialed.served_by
    );
}

/// One row: the cluster to create and the bootstrap members it must publish.
struct Row {
    case: &'static str,
    /// A serverless cluster is created through its own request shape; `created` is the
    /// provisioned request the other rows send.
    serverless: bool,
    created: ClusterRequest,
    members: &'static [&'static str],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded case: an MSK Serverless cluster, whose clients authenticate
            // with IAM access control and which publishes exactly one endpoint.
            case: "an MSK Serverless cluster with SASL/IAM",
            serverless: true,
            created: ClusterRequest::named("dialled-serverless"),
            members: &["bootstrapBrokerStringSaslIam"],
        },
        Row {
            case: "one broker node reached in the clear and over TLS",
            serverless: false,
            created: ClusterRequest {
                client_broker: Some("TLS_PLAINTEXT"),
                unauthenticated: Some(true),
                brokers: 1,
                subnets: vec!["subnet-a"],
                ..ClusterRequest::named("dialled-one-plaintext")
            },
            members: &["bootstrapBrokerString", "bootstrapBrokerStringTls"],
        },
        Row {
            case: "two broker nodes reached over SASL/SCRAM",
            serverless: false,
            created: ClusterRequest {
                client_broker: Some("TLS"),
                sasl_scram: Some(true),
                brokers: 2,
                subnets: vec!["subnet-a", "subnet-b"],
                ..ClusterRequest::named("dialled-two-scram")
            },
            members: &["bootstrapBrokerStringSaslScram"],
        },
        Row {
            case: "three broker nodes reached over SASL/IAM",
            serverless: false,
            created: ClusterRequest {
                client_broker: Some("TLS"),
                sasl_iam: Some(true),
                brokers: 3,
                subnets: vec!["subnet-a", "subnet-b", "subnet-c"],
                ..ClusterRequest::named("dialled-three-iam")
            },
            members: &["bootstrapBrokerStringSaslIam"],
        },
    ]
}

#[tokio::test]
async fn every_published_bootstrap_address_is_one_a_client_can_reach() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();
        let cluster = match row.serverless {
            true => {
                emulator
                    .create_serverless_ok(
                        "dialled-serverless",
                        &[vec!["subnet-0a1b2c3d", "subnet-1b2c3d4e"]],
                        Some(true),
                    )
                    .await
            }
            false => emulator.create_ok(&row.created).await,
        };
        // The cluster has no brokers to bootstrap against until the clock owes it its move
        // out of `CREATING`, so this LOOKS AGAIN until it publishes its endpoints rather
        // than waiting out a span.
        for member in row.members {
            emulator.published_bootstrap(&cluster, member, case).await;
        }
        let members = published(&emulator, &cluster, case).await;
        for member in row.members {
            assert!(
                members.iter().any(|(name, _)| name == member),
                "case {case:?}: expected {member} to be published, got {members:?}"
            );
        }
        let planes = DialablePlanes::of(&*emulator.engine)
            .expect("the engine vends the register of the planes it serves");

        for (member, pairs) in &members {
            assert!(
                !pairs.is_empty(),
                "case {case:?}: expected {member} to carry a hostname:port pair, got {pairs:?}"
            );
            // Every pair is an own address a client that reads the string reaches.
            for address in pairs {
                assert_reachable(&planes, address, &format!("{case} / {member} / {address}")).await;
            }
            // And that address answers the protocol the member is for, on the wire that
            // access method is served over. The connection goes to the published pair
            // itself (`broker_data_plane::dial` checks the host is this emulator's own
            // interface and dials the port it names).
            let bootstrap = pairs
                .first()
                .unwrap_or_else(|| panic!("case {case:?}: {member} carries no pair"));
            let probe = format!("{case} / {member} / {bootstrap}");
            let mut client = match member.contains("Tls") || member.contains("Sasl") {
                true => Client::tls(&probe, bootstrap).await,
                false => Client::plaintext(&probe, bootstrap).await,
            };
            let negotiated = client.api_versions(&probe).await;
            assert_eq!(
                negotiated.error_code, 0,
                "case {probe:?}: the published address must answer ApiVersions, got error {}",
                negotiated.error_code
            );
        }
        // No two access methods are published at one address: the port a pair carries is
        // what says which method a client reached.
        let every_address: BTreeSet<&String> =
            members.iter().flat_map(|(_, pairs)| pairs).collect();
        let published_count: usize = members.iter().map(|(_, pairs)| pairs.len()).sum();
        assert_eq!(
            every_address.len(),
            published_count,
            "case {case:?}: every published address must be distinct, got {members:?}"
        );
    }
}

#[tokio::test]
async fn the_addresses_the_broker_itself_reports_are_reachable_too() {
    // The rows differ along the broker count, which is what decides how long the broker
    // list a client reads from `Metadata` is.
    let rows: &[(&str, i64, &[&str])] = &[
        ("one broker", 1, &["subnet-a"]),
        ("two brokers", 2, &["subnet-a", "subnet-b"]),
        ("three brokers", 3, &["subnet-a", "subnet-b", "subnet-c"]),
    ];
    for (case, brokers, subnets) in rows {
        let emulator = Emulator::new();
        let cluster = emulator
            .create_ok(&ClusterRequest {
                client_broker: Some("PLAINTEXT"),
                brokers: *brokers,
                subnets: subnets.to_vec(),
                ..ClusterRequest::named(&format!("reported-dialled-{brokers}"))
            })
            .await;
        let bootstrap = emulator
            .published_bootstrap(&cluster, "bootstrapBrokerString", case)
            .await;
        let planes = DialablePlanes::of(&*emulator.engine)
            .expect("the engine vends the register of the planes it serves");
        let mut client = Client::plaintext(case, &bootstrap).await;

        // `Metadata`: every broker it reports is an address a client may now dial.
        let metadata: MetadataResponse = client
            .call(
                case,
                ApiKey::Metadata,
                9,
                &MetadataRequest::default().with_topics(None),
            )
            .await;
        let reported: Vec<String> = metadata
            .brokers
            .iter()
            .map(|broker| format!("{}:{}", broker.host.as_str(), broker.port))
            .collect();
        assert_eq!(
            reported.len(),
            usize::try_from(*brokers).unwrap_or_default(),
            "case {case:?}: expected Metadata to report the cluster's {brokers} brokers, got \
             {reported:?}"
        );
        for address in &reported {
            assert_reachable(&planes, address, &format!("{case} / Metadata / {address}")).await;
        }

        // `FindCoordinator`: the coordinator of a group is a broker of the cluster, at an
        // address the client may dial — which is what lets a client follow the answer.
        let coordinator: FindCoordinatorResponse = client
            .call(
                case,
                ApiKey::FindCoordinator,
                2,
                &FindCoordinatorRequest::default()
                    .with_key(StrBytes::from_static_str("dialled-readers"))
                    .with_key_type(0),
            )
            .await;
        let named = format!("{}:{}", coordinator.host.as_str(), coordinator.port);
        assert!(
            reported.contains(&named),
            "case {case:?}: the coordinator must be one of the cluster's brokers {reported:?}, \
             got {named:?}"
        );
        assert_reachable(&planes, &named, &format!("{case} / FindCoordinator / {named}")).await;
    }
}
