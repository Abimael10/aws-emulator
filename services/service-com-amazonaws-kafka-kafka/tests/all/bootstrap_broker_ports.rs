// SPDX-License-Identifier: CC0-1.0

//! Which PORT a bootstrap string carries, driven end to end over the same
//! `MultiServiceRouter` the emulator serves from, through the shared harness in
//! `tests/all/kafka_harness.rs`.
//!
//! RULE under test: every BROKER NODE of every ACCESS METHOD a cluster publishes answers
//! on a socket of its OWN, and the pair published for it is that socket's address — so a
//! bootstrap string lists one `hostname:port` pair per broker node, no two published
//! addresses of a cluster name one endpoint, and the port of a published address is the
//! one the core assigned that node's socket. Amazon MSK tells one node of a method from
//! another by the HOST it publishes it under; this emulator is reached at ONE host, the
//! interface the core binds its listeners on, so what carries that distinction here is the
//! port, and a client that splits a string reaches a broker of its own at every pair.
//!
//! Two members of the SAME access method publish the SAME string (an `Ipv6` member is that
//! method reached over another network layer — one broker endpoint whose record set gains
//! AAAA entries), and two different methods publish strings that share no address: what a
//! client dials decides both which broker it reaches and over which scheme.
//!
//! Each of those hosts is an address of the interface this emulator answers on, since a
//! client dials the pair it was handed verbatim.
//!
//! The rows differ along what the rule generalises over: how many broker nodes each method
//! publishes (one, two, three), which access methods the cluster enables (the plaintext and
//! TLS pair, both SASL mechanisms, the public siblings, the multi-VPC siblings), whether
//! the cluster is dual-stack, and which encryption the client-broker setting admits.
//!
//! The ports are then DIALLED, so what the suite asserts is the port of a socket this
//! emulator answers the Apache Kafka protocol on and not a number in a string.

use std::collections::{BTreeMap, BTreeSet};

use crate::broker_data_plane::Client;
use crate::kafka_harness::{poll_until_active, ClusterRequest, Emulator};

/// Which ACCESS METHOD serves each bootstrap member: the catalog of this operation's
/// output members (`smithy_query(query="operation",operation="GetBootstrapBrokers")`),
/// each mapped to the method whose port it publishes.
///
/// The three `Public*` and three `VpcConnectivity*` members are methods of their own —
/// Amazon MSK publishes the public and multi-VPC siblings of a scheme separately —
/// while an `Ipv6` member is the same method as the string it sits beside, since
/// one broker endpoint answers on both network layers.
const METHOD_OF_MEMBER: &[(&str, &str)] = &[
    ("bootstrapBrokerString", "plaintext"),
    ("bootstrapBrokerStringIpv6", "plaintext"),
    ("bootstrapBrokerStringTls", "tls"),
    ("bootstrapBrokerStringTlsIpv6", "tls"),
    ("bootstrapBrokerStringSaslScram", "sasl-scram"),
    ("bootstrapBrokerStringSaslScramIpv6", "sasl-scram"),
    ("bootstrapBrokerStringSaslIam", "sasl-iam"),
    ("bootstrapBrokerStringSaslIamIpv6", "sasl-iam"),
    ("bootstrapBrokerStringPublicTls", "public-tls"),
    ("bootstrapBrokerStringPublicSaslScram", "public-sasl-scram"),
    ("bootstrapBrokerStringPublicSaslIam", "public-sasl-iam"),
    ("bootstrapBrokerStringVpcConnectivityTls", "vpc-tls"),
    ("bootstrapBrokerStringVpcConnectivitySaslScram", "vpc-sasl-scram"),
    ("bootstrapBrokerStringVpcConnectivitySaslIam", "vpc-sasl-iam"),
];

/// The access method the member `member` is served by, or a failure naming a member this
/// catalog does not know — which is a member of the model this suite would otherwise skip
/// silently.
fn method_of(member: &str, case: &str) -> &'static str {
    METHOD_OF_MEMBER
        .iter()
        .find(|(known, _)| *known == member)
        .map(|(_, method)| *method)
        .unwrap_or_else(|| panic!("case {case:?}: {member} names no known access method"))
}

/// Every bootstrap string the cluster `arn` publishes, member by member, split into its
/// `hostname:port` pairs in the order the string lists them.
async fn published(emulator: &Emulator, arn: &str, case: &str) -> BTreeMap<String, Vec<String>> {
    let reply = emulator.bootstrap_brokers(arn).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected GetBootstrapBrokers to return 200, got {} {}",
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

/// The host and port one published pair spells, or a failure naming the pair that did
/// not.
fn split(pair: &str, case: &str) -> (String, u16) {
    let (host, port) = pair
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: {pair:?} is not a hostname:port pair"));
    let port = port
        .parse::<u16>()
        .unwrap_or_else(|_| panic!("case {case:?}: {pair:?} carries no port number"));
    (host.to_string(), port)
}

/// One row of the table: the cluster to create and how many broker nodes each of its
/// methods publishes a pair for.
struct Row {
    case: &'static str,
    created: ClusterRequest,
    brokers: usize,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The four access methods a cluster that admits unauthenticated clients and
            // both SASL mechanisms over TLS_PLAINTEXT publishes.
            case: "two brokers, unauthenticated plus both SASL mechanisms over TLS_PLAINTEXT",
            created: ClusterRequest {
                client_broker: Some("TLS_PLAINTEXT"),
                unauthenticated: Some(true),
                sasl_scram: Some(true),
                sasl_iam: Some(true),
                brokers: 2,
                subnets: vec!["subnet-a", "subnet-b"],
                ..ClusterRequest::named("ports-two-four-methods")
            },
            brokers: 2,
        },
        Row {
            // One broker node publishes one pair, so what a pair carries is a node's own
            // socket and not an artefact of there being several nodes.
            case: "one broker, plaintext only",
            created: ClusterRequest {
                client_broker: Some("PLAINTEXT"),
                brokers: 1,
                subnets: vec!["subnet-a"],
                ..ClusterRequest::named("ports-one-plaintext")
            },
            brokers: 1,
        },
        Row {
            // The public siblings of a scheme are methods of their own, and a dual-stack
            // cluster reaches each method over both network layers at that method's own
            // addresses.
            case: "three brokers, TLS and SASL/IAM published publicly, dual-stack",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                tls_auth: Some(true),
                sasl_iam: Some(true),
                public_access: Some("SERVICE_PROVIDED_EIPS"),
                network_type: Some("DUAL"),
                brokers: 3,
                subnets: vec!["subnet-a", "subnet-b", "subnet-c"],
                ..ClusterRequest::named("ports-three-public-dual")
            },
            brokers: 3,
        },
        Row {
            // The multi-VPC siblings are methods of their own too.
            case: "two brokers, SASL/SCRAM with multi-VPC TLS, SCRAM and IAM",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                sasl_scram: Some(true),
                vpc_tls: Some(true),
                vpc_scram: Some(true),
                vpc_iam: Some(true),
                brokers: 2,
                subnets: vec!["subnet-a", "subnet-b"],
                ..ClusterRequest::named("ports-two-multi-vpc")
            },
            brokers: 2,
        },
    ]
}

#[tokio::test]
async fn every_broker_node_of_every_access_method_answers_on_a_port_of_its_own() {
    let emulator = Emulator::new();
    let rows = rows();
    let mut arns = Vec::new();
    for row in &rows {
        arns.push(emulator.create_ok(&row.created).await);
    }

    for (row, arn) in rows.iter().zip(&arns) {
        let case = row.case;
        // The cluster has no brokers to bootstrap against until the clock owes it its
        // move out of `CREATING`, so this LOOKS AGAIN until it has settled.
        poll_until_active(&emulator, arn, case).await;
        let members = published(&emulator, arn, case).await;
        assert!(
            !members.is_empty(),
            "case {case:?}: a cluster with brokers must publish a bootstrap string"
        );
        // The broker nodes this row provisioned, which is how many pairs each method
        // publishes below: one per node, each on that node's own port.
        let described = emulator.describe(arn).await;
        assert_eq!(
            described.at("/clusterInfo/numberOfBrokerNodes").as_i64(),
            i64::try_from(row.brokers).ok(),
            "case {case:?}: expected the cluster to hold {} broker nodes, got {}",
            row.brokers,
            described.body
        );

        // Within ONE string: a pair per broker node, each on a port of its own.
        let mut ports_of_method: BTreeMap<&str, BTreeSet<u16>> = BTreeMap::new();
        let mut string_of_method: BTreeMap<&str, String> = BTreeMap::new();
        let mut hosts_of_method: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for (member, pairs) in &members {
            let probe = format!("{case} / {member}");
            assert_eq!(
                pairs.len(),
                row.brokers,
                "case {probe:?}: expected one pair per broker node, got {pairs:?}"
            );
            let mut ports: BTreeSet<u16> = BTreeSet::new();
            let mut hosts: BTreeSet<String> = BTreeSet::new();
            for pair in pairs {
                let (host, port) = split(pair, &probe);
                // A host a client is handed is a host it dials, so it names this
                // emulator's own interface.
                assert!(
                    ::service_common::data_plane_addressing::names_the_loopback_interface(&host),
                    "case {probe:?}: every published host must name this emulator's own \
                     interface, got {pairs:?}"
                );
                assert!(
                    ports.insert(port),
                    "case {probe:?}: a string must name each address it serves once, got \
                     {pairs:?}"
                );
                hosts.insert(host);
            }
            let method = method_of(member, &probe);
            // Two members of the SAME access method publish the same string: an IPv6
            // sibling is that method reached over another network layer.
            if let Some(held) = string_of_method.get(method) {
                assert_eq!(
                    held,
                    &pairs.join(","),
                    "case {probe:?}: two members of the {method} method must publish the same \
                     string, got {pairs:?} beside {held:?}"
                );
            }
            ports_of_method.insert(method, ports);
            string_of_method.insert(method, pairs.join(","));
            hosts_of_method.insert(method, hosts);
        }

        // Across the methods: no port is published twice, so no two of a cluster's
        // addresses name one endpoint and what a client dials decides both the broker it
        // reaches and the scheme it is served over.
        let published_ports: usize = ports_of_method.values().map(BTreeSet::len).sum();
        let distinct_ports: BTreeSet<u16> =
            ports_of_method.values().flatten().copied().collect();
        assert_eq!(
            distinct_ports.len(),
            published_ports,
            "case {case:?}: expected a port of its own for every broker node of every access \
             method, got {ports_of_method:?}"
        );
        // And distinct methods publish distinct strings, since their ports differ.
        let strings: BTreeSet<&String> = string_of_method.values().collect();
        assert_eq!(
            strings.len(),
            string_of_method.len(),
            "case {case:?}: expected one distinct string per access method, got \
             {string_of_method:?}"
        );
        // The HOSTS are the same under every method — this emulator answers on one
        // interface, whichever method a client reaches it over.
        let distinct_hosts: BTreeSet<&BTreeSet<String>> = hosts_of_method.values().collect();
        assert_eq!(
            distinct_hosts.len(),
            1,
            "case {case:?}: every access method must list the same broker hosts, got \
             {hosts_of_method:?}"
        );

        // Reporting the endpoints changes nothing: a second read answers the same ports.
        let again = published(&emulator, arn, case).await;
        assert_eq!(
            again, members,
            "case {case:?}: a second read must answer the same ports, got {again:?} after \
             {members:?}"
        );
    }
}

#[tokio::test]
async fn every_published_address_answers_on_a_socket_of_its_own() {
    let case = "a cluster publishing the plaintext, TLS and both SASL methods";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("TLS_PLAINTEXT"),
            unauthenticated: Some(true),
            sasl_scram: Some(true),
            sasl_iam: Some(true),
            brokers: 2,
            subnets: vec!["subnet-a", "subnet-b"],
            ..ClusterRequest::named("ports-dialled")
        })
        .await;
    poll_until_active(&emulator, &cluster, case).await;
    let members = published(&emulator, &cluster, case).await;
    assert_eq!(
        members.keys().cloned().collect::<Vec<String>>(),
        vec![
            "bootstrapBrokerString".to_string(),
            "bootstrapBrokerStringSaslIam".to_string(),
            "bootstrapBrokerStringSaslScram".to_string(),
            "bootstrapBrokerStringTls".to_string(),
        ],
        "case {case:?}: expected the four access methods of this cluster, got {members:?}"
    );

    // Every published pair is a socket of this emulator's own and answers the protocol on
    // the wire its access method is served over, so a port a cluster publishes is one the
    // core assigned that broker node for that method and never a number borrowed from
    // another endpoint. Each pair of every member is dialled, not only the first.
    let mut dialled: BTreeSet<u16> = BTreeSet::new();
    for (member, pairs) in &members {
        for pair in pairs {
            let probe = format!("{case} / {member} / {pair}");
            let (_host, port) = split(pair, &probe);
            assert!(
                dialled.insert(port),
                "case {probe:?}: the port {port} is already another endpoint's"
            );
            let mut client = match member.contains("Tls") || member.contains("Sasl") {
                true => Client::tls(&probe, pair).await,
                false => Client::plaintext(&probe, pair).await,
            };
            let negotiated = client.api_versions(&probe).await;
            assert_eq!(
                negotiated.error_code, 0,
                "case {probe:?}: the port {port} must answer ApiVersions, got error {}",
                negotiated.error_code
            );
        }
    }
}
