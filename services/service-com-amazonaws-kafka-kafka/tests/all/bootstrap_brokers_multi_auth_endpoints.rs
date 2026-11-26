// SPDX-License-Identifier: CC0-1.0

//! What a cluster that admits SEVERAL access methods publishes, driven end to end over the
//! same `MultiServiceRouter` the emulator serves from, through the shared harness in
//! `tests/all/kafka_harness.rs`.
//!
//! RULE under test: Amazon MSK answers one ACCESS METHOD on ONE PORT, shared by every
//! broker node of the cluster, and tells one node from another by the HOST it publishes it
//! under — so the port a client reads off a bootstrap string names the METHOD it reaches.
//! `GetBootstrapBrokers` therefore answers, for each method the cluster enables, a string
//! carrying one `hostname:port` pair per broker node where every pair repeats that one
//! port; two methods of a cluster never share a port, so their strings are two different
//! values; a member the cluster does not enable is absent from the payload rather than
//! present and empty; and each of those ports is a socket this emulator really answers the
//! Apache Kafka wire protocol on, where a client is reported the cluster's whole broker
//! membership.
//!
//! Why the port and not the host: security-group rules, VPC endpoint policies, firewall
//! and monitoring templates are written per port, and a client derives a sibling endpoint
//! by swapping one method's port for another's.
//!
//! The rows differ along what the rule generalises over: how many broker nodes the cluster
//! holds (one, two, three, and more nodes than client subnets), which methods its
//! client-broker encryption and client authentication admit (plaintext, unauthenticated
//! TLS, mutual TLS, each SASL mechanism, both together), and which of the public,
//! multi-VPC and IPv6 siblings its connectivity adds.

use std::collections::{BTreeMap, BTreeSet};

use crate::bootstrap_broker_cardinality::{published, split};
use crate::bootstrap_broker_ports::method_of;
use crate::broker_data_plane::Client;
// Every bootstrap member the model declares, as the one catalog this crate keeps of them —
// so a row's expectation that a member is ABSENT is checked against the whole output shape
// and not only against the members the cluster happens to publish.
use crate::cluster_lifecycle::BOOTSTRAP_MEMBERS;
use crate::kafka_harness::{poll_until_active, ClusterRequest, Emulator};

/// One row: the cluster to create, the broker nodes it holds, and the bootstrap members it
/// must publish — every other member of the output shape has to be absent.
struct Row {
    case: &'static str,
    created: ClusterRequest,
    brokers: usize,
    published: &'static [&'static str],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The shape the replayed multi-auth cluster carries: unauthenticated clients
            // plus both SASL mechanisms over TLS_PLAINTEXT, so all four of the cluster's
            // own methods are offered and the public and multi-VPC siblings are not.
            case: "two broker nodes, unauthenticated plus both SASL mechanisms over \
                   TLS_PLAINTEXT",
            created: ClusterRequest {
                client_broker: Some("TLS_PLAINTEXT"),
                unauthenticated: Some(true),
                sasl_scram: Some(true),
                sasl_iam: Some(true),
                brokers: 2,
                subnets: vec!["subnet-a", "subnet-b"],
                ..ClusterRequest::named("multi-auth-two")
            },
            brokers: 2,
            published: &[
                "bootstrapBrokerString",
                "bootstrapBrokerStringTls",
                "bootstrapBrokerStringSaslScram",
                "bootstrapBrokerStringSaslIam",
            ],
        },
        Row {
            // `BrokerNodeGroupInfo$NumberOfBrokerNodes` is `__integerMin1Max15`, so one is
            // the published minimum: one node still publishes one pair on the method's
            // port, which is what tells a shared port from an artefact of several nodes.
            case: "one broker node, plaintext only",
            created: ClusterRequest {
                client_broker: Some("PLAINTEXT"),
                unauthenticated: Some(true),
                brokers: 1,
                subnets: vec!["subnet-a"],
                ..ClusterRequest::named("multi-auth-one")
            },
            brokers: 1,
            published: &["bootstrapBrokerString"],
        },
        Row {
            // Mutual TLS plus SASL/SCRAM over TLS: the plaintext method is not offered at
            // all, so its member is absent while two methods are published.
            case: "three broker nodes, mutual TLS and SASL/SCRAM over TLS",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                tls_auth: Some(true),
                sasl_scram: Some(true),
                brokers: 3,
                subnets: vec!["subnet-a", "subnet-b", "subnet-c"],
                ..ClusterRequest::named("multi-auth-three")
            },
            brokers: 3,
            published: &["bootstrapBrokerStringTls", "bootstrapBrokerStringSaslScram"],
        },
        Row {
            // More nodes than subnets, with the PUBLIC siblings of two methods and the
            // IPv6 ones of a dual-stack cluster: the siblings are methods of their own,
            // except an `Ipv6` member, which is its neighbour's method over another
            // network layer and therefore repeats its string.
            case: "five broker nodes over three subnets, published publicly and dual-stack",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                tls_auth: Some(true),
                sasl_iam: Some(true),
                public_access: Some("SERVICE_PROVIDED_EIPS"),
                network_type: Some("DUAL"),
                brokers: 5,
                subnets: vec!["subnet-a", "subnet-b", "subnet-c"],
                ..ClusterRequest::named("multi-auth-public")
            },
            brokers: 5,
            published: &[
                "bootstrapBrokerStringTls",
                "bootstrapBrokerStringSaslIam",
                "bootstrapBrokerStringPublicTls",
                "bootstrapBrokerStringPublicSaslIam",
                "bootstrapBrokerStringTlsIpv6",
                "bootstrapBrokerStringSaslIamIpv6",
            ],
        },
        Row {
            // The multi-VPC siblings, which a cluster's own client VPC connectivity adds.
            case: "two broker nodes with multi-VPC TLS, SCRAM and IAM connectivity",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                sasl_scram: Some(true),
                sasl_iam: Some(true),
                vpc_tls: Some(true),
                vpc_scram: Some(true),
                vpc_iam: Some(true),
                brokers: 2,
                subnets: vec!["subnet-a", "subnet-b"],
                ..ClusterRequest::named("multi-auth-vpc")
            },
            brokers: 2,
            published: &[
                "bootstrapBrokerStringSaslScram",
                "bootstrapBrokerStringSaslIam",
                "bootstrapBrokerStringVpcConnectivityTls",
                "bootstrapBrokerStringVpcConnectivitySaslScram",
                "bootstrapBrokerStringVpcConnectivitySaslIam",
            ],
        },
    ]
}

/// RULE: one port per access method, shared by every broker node of the cluster — so each
/// published string repeats one port over its pairs, distinct methods carry distinct ports
/// and therefore distinct strings, members of one method repeat its string, and a member
/// the cluster does not enable is absent.
#[tokio::test]
async fn one_port_per_access_method_shared_by_every_broker_node() {
    let emulator = Emulator::new();
    let rows = rows();
    let mut arns = Vec::new();
    for row in &rows {
        arns.push(emulator.create_ok(&row.created).await);
    }

    for (row, arn) in rows.iter().zip(&arns) {
        let case = row.case;
        // A cluster still `CREATING` has no brokers to bootstrap against, so this LOOKS
        // AGAIN until the cluster has settled rather than waiting out a span.
        poll_until_active(&emulator, arn, case).await;
        let members = published(&emulator, arn, case).await;
        let listed: BTreeSet<&str> = members.iter().map(|(member, _)| member.as_str()).collect();
        assert_eq!(
            listed,
            row.published.iter().copied().collect::<BTreeSet<&str>>(),
            "case {case:?}: expected exactly the members this cluster's access methods \
             entitle, got {listed:?}"
        );
        // And every OTHER member of the output shape is absent from the payload rather
        // than present and empty.
        let reply = emulator.bootstrap_brokers(arn).await;
        for member in BOOTSTRAP_MEMBERS {
            if row.published.contains(member) {
                continue;
            }
            assert!(
                reply.body.get(*member).is_none(),
                "case {case:?}: {member} must be absent for a cluster that does not enable \
                 it, got {:?}",
                reply.body.get(*member)
            );
        }

        let mut port_of_method: BTreeMap<&str, u16> = BTreeMap::new();
        let mut string_of_method: BTreeMap<&str, String> = BTreeMap::new();
        for (member, pairs) in &members {
            let probe = format!("{case} / {member}");
            // ONE PAIR PER BROKER NODE, which is what a client that splits the string to
            // size its connection pool or fan out per broker counts.
            assert_eq!(
                pairs.len(),
                row.brokers,
                "case {probe:?}: expected one hostname:port pair per broker node, got \
                 {pairs:?}"
            );
            let mut ports: BTreeSet<u16> = BTreeSet::new();
            for pair in pairs {
                let (host, port) = split(pair, &probe);
                assert!(
                    ::service_common::data_plane_addressing::names_the_loopback_interface(&host),
                    "case {probe:?}: every published host must name an address this \
                     emulator answers on, got {pair:?}"
                );
                ports.insert(port);
            }
            // THE INVARIANT: all pairs of one string share one port, so the port a client
            // admits to its brokers reaches every node of that method.
            assert_eq!(
                ports.len(),
                1,
                "case {probe:?}: expected every pair of one string to carry the access \
                 method's one port, got {ports:?} in {pairs:?}"
            );
            let port = ports.into_iter().next().unwrap_or_default();
            let method = method_of(member, &probe);
            // Two members of ONE method publish one string: an `Ipv6` member is that
            // method reached over another network layer.
            if let Some(held) = port_of_method.get(method) {
                assert_eq!(
                    *held, port,
                    "case {probe:?}: two members of the {method} method must publish one \
                     port, got {port} beside {held}"
                );
            }
            if let Some(held) = string_of_method.get(method) {
                assert_eq!(
                    held,
                    &pairs.join(","),
                    "case {probe:?}: two members of the {method} method must publish the \
                     same string, got {pairs:?} beside {held:?}"
                );
            }
            port_of_method.insert(method, port);
            string_of_method.insert(method, pairs.join(","));
        }

        // Across the methods: as many distinct ports as methods, so a port identifies the
        // method, and as many distinct strings as methods, since their ports differ.
        let distinct_ports: BTreeSet<u16> = port_of_method.values().copied().collect();
        assert_eq!(
            distinct_ports.len(),
            port_of_method.len(),
            "case {case:?}: expected one distinct port per access method, got \
             {port_of_method:?}"
        );
        let distinct_strings: BTreeSet<&String> = string_of_method.values().collect();
        assert_eq!(
            distinct_strings.len(),
            string_of_method.len(),
            "case {case:?}: expected one distinct string per access method, got \
             {string_of_method:?}"
        );

        // Reporting the endpoints changes nothing a later caller sees: a second read
        // answers the same strings on the same ports.
        let again = published(&emulator, arn, case).await;
        assert_eq!(
            again, members,
            "case {case:?}: a second read must answer the same strings, got {again:?} after \
             {members:?}"
        );
    }
}

/// RULE: the port a method publishes is a socket this emulator answers the Apache Kafka
/// wire protocol on, and a client that dials it is reported the cluster's WHOLE broker
/// membership there — so sharing one port across the nodes loses neither the cluster's
/// broker count nor its reachability.
#[tokio::test]
async fn the_shared_port_answers_and_reports_every_broker_node() {
    // The rows differ along the broker count, which is what decides how long the broker
    // list the shared port reports is.
    let rows: &[(&str, i64, &[&str])] = &[
        ("one broker node", 1, &["subnet-a"]),
        ("two broker nodes", 2, &["subnet-a", "subnet-b"]),
        (
            "three broker nodes",
            3,
            &["subnet-a", "subnet-b", "subnet-c"],
        ),
    ];
    for (case, brokers, subnets) in rows {
        let emulator = Emulator::new();
        let cluster = emulator
            .create_ok(&ClusterRequest {
                client_broker: Some("PLAINTEXT"),
                unauthenticated: Some(true),
                brokers: *brokers,
                subnets: subnets.to_vec(),
                ..ClusterRequest::named(&format!("multi-auth-dialled-{brokers}"))
            })
            .await;
        // LOOKS AGAIN until the cluster publishes its endpoints rather than waiting out a
        // span: the move out of `CREATING` is one the clock owes the cluster.
        let dialled = emulator
            .published_bootstrap(&cluster, "bootstrapBrokerString", case)
            .await;
        let pairs = published(&emulator, &cluster, case)
            .await
            .into_iter()
            .find(|(member, _)| member == "bootstrapBrokerString")
            .map(|(_, pairs)| pairs)
            .unwrap_or_else(|| panic!("case {case:?}: the cluster published no plaintext string"));
        assert!(
            pairs.iter().all(|pair| *pair == dialled),
            "case {case:?}: every pair of the plaintext string is the method's one address \
             {dialled:?}, got {pairs:?}"
        );

        let mut client = Client::plaintext(case, &dialled).await;
        let negotiated = client.api_versions(case).await;
        assert_eq!(
            negotiated.error_code, 0,
            "case {case:?}: the published port must answer ApiVersions, got error {}",
            negotiated.error_code
        );
        let metadata = client.metadata(case).await;
        let reported: Vec<i32> = metadata
            .brokers
            .iter()
            .map(|broker| broker.node_id.0)
            .collect();
        assert_eq!(
            reported,
            (1..=i32::try_from(*brokers).unwrap_or(1)).collect::<Vec<i32>>(),
            "case {case:?}: expected the cluster's {brokers} broker nodes to be reported, \
             got {reported:?}"
        );
        // And every one of them is reported AT the published address, which is where the
        // method's nodes are reached.
        let addresses: BTreeSet<String> = metadata
            .brokers
            .iter()
            .map(|broker| format!("{}:{}", broker.host.as_str(), broker.port))
            .collect();
        assert_eq!(
            addresses,
            BTreeSet::from([dialled.clone()]),
            "case {case:?}: expected every broker node to be reported at {dialled:?}, got \
             {addresses:?}"
        );
    }
}
