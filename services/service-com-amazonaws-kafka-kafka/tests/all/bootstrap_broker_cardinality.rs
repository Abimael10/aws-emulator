// SPDX-License-Identifier: CC0-1.0

//! How MANY `hostname:port` pairs a bootstrap string carries, driven end to end over the
//! same `MultiServiceRouter` the emulator serves from, through the shared harness in
//! `tests/all/kafka_harness.rs`.
//!
//! RULE under test: every bootstrap string a cluster publishes is "a string containing one
//! or more hostname:port pairs" (`GetBootstrapBrokers` output members), carrying ONE pair
//! per broker node the cluster was provisioned with — so the pairs number exactly the
//! cluster's broker count, each pair addresses a DISTINCT broker, and the same holds for
//! every variant the cluster entitles. A cluster that publishes no broker nodes of its own
//! (MSK Serverless) stands behind one endpoint, so its single string carries one pair.
//!
//! Every pair is an address this emulator really answers the Apache Kafka wire protocol
//! on, and the brokers a client learns from `Metadata` over any one of them are the same
//! list in the same order — so a client that splits the string to size its connection
//! pool, to pin a replication factor to the broker count or to fail over to another broker
//! reaches a broker for each pair it took.
//!
//! The rows differ along what the rule generalises over: the number of broker nodes (the
//! published minimum, two, three, and a count above the subnets it is spread over), the
//! number of client subnets they are placed in, and which access paths the cluster
//! entitles — so the count is asserted on the plaintext, TLS and both SASL variants, not
//! only on the one a single row happens to publish.

use std::collections::BTreeSet;

use crate::broker_data_plane::Client;
use crate::kafka_harness::{sleep_past_activation, ClusterRequest, Emulator};

/// Every `host:port` pair of every bootstrap string the cluster `arn` publishes, member by
/// member, in the order each string lists them.
async fn published(emulator: &Emulator, arn: &str, case: &str) -> Vec<(String, Vec<String>)> {
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

/// The host and port one published pair spells, or a failure naming the pair that did not.
fn split(pair: &str, case: &str) -> (String, u16) {
    let (host, port) = pair.rsplit_once(':').unwrap_or_else(|| {
        panic!("case {case:?}: {pair:?} is not a hostname:port pair")
    });
    let port = port.parse::<u16>().unwrap_or_else(|_| {
        panic!("case {case:?}: {pair:?} does not carry a port number")
    });
    (host.to_string(), port)
}

/// One row: the cluster to create, and the broker nodes it is provisioned with — which is
/// the number of pairs every string it publishes must then carry.
struct Row {
    case: &'static str,
    created: ClusterRequest,
    brokers: usize,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // `BrokerNodeGroupInfo$NumberOfBrokerNodes` is `__integerMin1Max15`, so one is
            // the published minimum: a one-broker cluster publishes one pair.
            case: "one broker node, in one subnet",
            created: ClusterRequest {
                brokers: 1,
                subnets: vec!["subnet-a"],
                ..ClusterRequest::named("pairs-one")
            },
            brokers: 1,
        },
        Row {
            case: "two broker nodes, spread over two subnets",
            created: ClusterRequest {
                brokers: 2,
                subnets: vec!["subnet-a", "subnet-b"],
                ..ClusterRequest::named("pairs-two")
            },
            brokers: 2,
        },
        Row {
            case: "three broker nodes over three subnets, TLS only",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                brokers: 3,
                subnets: vec!["subnet-a", "subnet-b", "subnet-c"],
                ..ClusterRequest::named("pairs-three-tls")
            },
            brokers: 3,
        },
        Row {
            // More nodes than subnets: the nodes are spread round-robin over the subnets,
            // and the string still carries one pair per NODE.
            case: "six broker nodes over three subnets, both SASL schemes",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                sasl_scram: Some(true),
                sasl_iam: Some(true),
                brokers: 6,
                subnets: vec!["subnet-a", "subnet-b", "subnet-c"],
                ..ClusterRequest::named("pairs-six-sasl")
            },
            brokers: 6,
        },
        Row {
            // The public and multi-VPC siblings of a path are the same brokers reached over
            // another network, so they carry the same number of pairs.
            case: "two broker nodes published publicly and over multi-VPC connectivity",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                sasl_iam: Some(true),
                public_access: Some("SERVICE_PROVIDED_EIPS"),
                vpc_tls: Some(true),
                vpc_iam: Some(true),
                network_type: Some("DUAL"),
                brokers: 2,
                subnets: vec!["subnet-a", "subnet-b"],
                ..ClusterRequest::named("pairs-two-public")
            },
            brokers: 2,
        },
    ]
}

#[tokio::test]
async fn every_bootstrap_string_carries_one_distinct_pair_per_broker_node() {
    let emulator = Emulator::new();
    let rows = rows();

    // Every row's cluster first, then ONE activation span: a cluster still `CREATING` has
    // no brokers to bootstrap against, so the reader refuses it, and these rows are about
    // how many endpoints a SETTLED cluster publishes.
    let mut arns = Vec::new();
    for row in &rows {
        arns.push(emulator.create_ok(&row.created).await);
    }
    sleep_past_activation();

    for (row, arn) in rows.iter().zip(&arns) {
        let case = row.case;
        let members = published(&emulator, arn, case).await;
        assert!(
            !members.is_empty(),
            "case {case:?}: a cluster with brokers must publish a bootstrap string"
        );
        for (member, pairs) in &members {
            let probe = format!("{case} / {member}");
            assert_eq!(
                pairs.len(),
                row.brokers,
                "case {probe:?}: expected one hostname:port pair per broker node \
                 ({} nodes), got {pairs:?}",
                row.brokers
            );
            // Each pair addresses a DISTINCT broker: a string that repeats one address
            // names one broker however many times it names it.
            let distinct: BTreeSet<&String> = pairs.iter().collect();
            assert_eq!(
                distinct.len(),
                pairs.len(),
                "case {probe:?}: every pair must address a distinct broker, got {pairs:?}"
            );
            for pair in pairs {
                let (host, port) = split(pair, &probe);
                assert!(
                    !host.is_empty() && port > 0,
                    "case {probe:?}: {pair:?} must name an address this emulator serves"
                );
            }
        }
        // And the strings of one cluster agree with each other: every variant lists the
        // same number of brokers, whichever access path it is for.
        let counts: BTreeSet<usize> = members.iter().map(|(_, pairs)| pairs.len()).collect();
        assert_eq!(
            counts,
            BTreeSet::from([row.brokers]),
            "case {case:?}: every variant must list the cluster's {} brokers, got {counts:?}",
            row.brokers
        );
    }
}

#[tokio::test]
async fn a_serverless_cluster_publishes_the_one_endpoint_it_stands_behind() {
    let case = "an MSK Serverless cluster";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_serverless_ok("pairs-serverless", &[vec!["subnet-0a1b2c3d"]], Some(true))
        .await;
    sleep_past_activation();
    let members = published(&emulator, &cluster, case).await;
    let listed: Vec<&String> = members.iter().map(|(member, _)| member).collect();
    assert_eq!(
        listed,
        vec!["bootstrapBrokerStringSaslIam"],
        "case {case:?}: a serverless cluster publishes exactly the SASL/IAM string, got \
         {listed:?}"
    );
    for (member, pairs) in &members {
        assert_eq!(
            pairs.len(),
            1,
            "case {case:?}: {member} of a cluster that publishes no broker nodes carries one \
             pair, got {pairs:?}"
        );
    }
}

#[tokio::test]
async fn every_published_pair_answers_and_reports_the_whole_broker_list() {
    // The rows differ along the broker count, which is what decides how many pairs are
    // dialled and how long the list each of them reports is.
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
                ..ClusterRequest::named(&format!("pairs-dialled-{brokers}"))
            })
            .await;
        // LOOKS AGAIN until the cluster publishes its endpoints rather than waiting out a
        // span: the transition out of `CREATING` is one the clock owes the cluster.
        emulator
            .published_bootstrap(&cluster, "bootstrapBrokerString", case)
            .await;
        let pairs = published(&emulator, &cluster, case)
            .await
            .into_iter()
            .find(|(member, _)| member == "bootstrapBrokerString")
            .map(|(_, pairs)| pairs)
            .unwrap_or_else(|| panic!("case {case:?}: the cluster published no plaintext string"));
        assert_eq!(
            pairs.len(),
            usize::try_from(*brokers).unwrap_or_default(),
            "case {case:?}: expected one pair per broker node, got {pairs:?}"
        );
        for pair in &pairs {
            let probe = format!("{case} / {pair}");
            let mut client = Client::plaintext(&probe, pair).await;
            let negotiated = client.api_versions(&probe).await;
            assert_eq!(
                negotiated.error_code, 0,
                "case {probe:?}: every published pair must answer ApiVersions, got error {}",
                negotiated.error_code
            );
            // And the broker list it reports is the whole published list, in the same
            // order: the brokers are one cluster, whichever of them the client reached.
            let metadata = client.metadata(&probe).await;
            let reported: Vec<String> = metadata
                .brokers
                .iter()
                .map(|broker| format!("{}:{}", broker.host.as_str(), broker.port))
                .collect();
            assert_eq!(
                reported, pairs,
                "case {probe:?}: expected Metadata to report the published brokers {pairs:?}, \
                 got {reported:?}"
            );
            // The node this pair addresses reports ITSELF as the coordinator of a group a
            // client asks it for, so a client stays on the connection it already has.
            let coordinator: ::kafka_protocol::messages::FindCoordinatorResponse = client
                .call(
                    &probe,
                    ::kafka_protocol::messages::ApiKey::FindCoordinator,
                    2,
                    &::kafka_protocol::messages::FindCoordinatorRequest::default()
                        .with_key(::kafka_protocol::protocol::StrBytes::from_static_str("readers"))
                        .with_key_type(0),
                )
                .await;
            assert_eq!(
                format!("{}:{}", coordinator.host.as_str(), coordinator.port),
                *pair,
                "case {probe:?}: the coordinator must be the broker the client dialled"
            );
        }
    }
}

#[tokio::test]
async fn the_pairs_follow_the_cluster_when_its_broker_count_grows() {
    let case = "a cluster scaled from two brokers to four";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            brokers: 2,
            subnets: vec!["subnet-a", "subnet-b"],
            ..ClusterRequest::named("pairs-grown")
        })
        .await;
    let version = crate::kafka_harness::poll_until_active(&emulator, &cluster, case).await;
    let before = published(&emulator, &cluster, case).await;
    for (member, pairs) in &before {
        assert_eq!(
            pairs.len(),
            2,
            "case {case:?}: {member} must list the two brokers the cluster was created with, \
             got {pairs:?}"
        );
    }

    let accepted = emulator
        .call(
            "PUT",
            &format!(
                "/v1/clusters/{}/nodes/count",
                crate::kafka_harness::pct(&cluster)
            ),
            Some(serde_json::json!({
                "currentVersion": version,
                "targetNumberOfBrokerNodes": 4,
            })),
        )
        .await;
    assert_eq!(
        accepted.status, 200,
        "case {case:?}: UpdateBrokerCount must be accepted, got {} {}",
        accepted.status, accepted.body
    );
    // The new nodes are a transition the CLOCK owes the cluster, so this LOOKS AGAIN until
    // the published endpoints name them rather than waiting out a span; the deadline is far
    // longer than that span, so it says nothing about how fast the host runs.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let after = published(&emulator, &cluster, case).await;
        let settled = !after.is_empty()
            && after.iter().all(|(_, pairs)| pairs.len() == 4)
            && after
                .iter()
                .all(|(_, pairs)| pairs.iter().collect::<BTreeSet<_>>().len() == 4);
        if settled {
            // The addresses the cluster already published are kept, so a client holding the
            // old string still reaches the brokers it named.
            for (member, pairs) in &before {
                let grown = after
                    .iter()
                    .find(|(grown, _)| grown == member)
                    .map(|(_, pairs)| pairs.clone())
                    .unwrap_or_default();
                assert!(
                    pairs.iter().all(|pair| grown.contains(pair)),
                    "case {case:?}: {member} must keep the addresses it published {pairs:?}, \
                     got {grown:?}"
                );
            }
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the published endpoints must name the four brokers the cluster \
             holds, last read {after:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // And the control plane agrees about the count the endpoints now name.
    let described = emulator.describe(&cluster).await;
    assert_eq!(
        described.at("/clusterInfo/numberOfBrokerNodes").as_i64(),
        Some(4),
        "case {case:?}: expected the cluster to hold four brokers, got {}",
        described.body
    );
}
