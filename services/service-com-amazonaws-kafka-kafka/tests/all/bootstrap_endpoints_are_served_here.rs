// SPDX-License-Identifier: CC0-1.0

//! Whether EVERY pair of EVERY bootstrap member a cluster publishes answers, including the
//! member families a cluster's public access, multi-VPC connectivity and dual-stack
//! networking add, driven end to end over the same `MultiServiceRouter` the emulator serves
//! from, through the shared harness in `tests/all/kafka_harness.rs`.
//!
//! RULE under test: every `hostname:port` pair of every bootstrap string a cluster
//! publishes is an address THIS emulator accepts an Apache Kafka connection on — the
//! interface the core bound its socket on, never a name of AWS's own and never a name
//! composed under a zone this process does not serve — and the broker behind it agrees with
//! the control plane about where the cluster's brokers are reached. A client does the one
//! thing these members exist for: it takes the string verbatim as `bootstrap.servers` and
//! connects, so an announced address that leads anywhere but back to this process is
//! unusable wherever it is returned.
//!
//! `bootstrap_broker_entitlement` decides WHICH members a cluster publishes and
//! `bootstrap_addresses_dialled_as_published` reaches the plaintext, TLS and SASL ones;
//! what this suite adds is the pairs of the `Public*`, `VpcConnectivity*` and `*Ipv6`
//! families dialled over the wire they are served on, the agreement between a published
//! string and the broker list the broker behind it reports, and that no address is ever
//! announced for two clusters at once.
//!
//! The rows differ along what the rule generalises over: the cluster kind (provisioned and
//! serverless), the number of broker nodes and client subnets (one, two, three), and which
//! access methods the cluster entitles — the plaintext path alone, the plaintext and TLS
//! pair with both SASL mechanisms, and a cluster published publicly and over multi-VPC
//! connectivity on both network layers.

use std::collections::BTreeSet;

use crate::bootstrap_broker_cardinality::{published, split};
use crate::broker_data_plane::Client;
use crate::kafka_harness::{ClusterRequest, Emulator};

/// Whether a member names one of the ENCRYPTED access methods, which is the wire a client
/// dials it over: every SASL method of Amazon MSK runs over TLS, as does the TLS method
/// itself, while the plaintext member is reached in the clear.
fn encrypted(member: &str) -> bool {
    member.contains("Tls") || member.contains("Sasl")
}

/// Assert every pair of every member `arn` publishes is an address this emulator answers
/// the Apache Kafka protocol on, and hand back the addresses that were dialled.
async fn assert_every_published_pair_answers(
    emulator: &Emulator,
    arn: &str,
    expected_members: &[&str],
    case: &str,
) -> BTreeSet<String> {
    let members = published(emulator, arn, case).await;
    // Exactly the members the cluster's encryption, client authentication and connectivity
    // entitle — so a family that stopped being published is a failure here rather than a
    // row that quietly asserts nothing. Compared as SETS: which members are populated is
    // the modelled property, the order a JSON object lists them in is not.
    let listed: BTreeSet<&str> = members.iter().map(|(member, _)| member.as_str()).collect();
    let wanted: BTreeSet<&str> = expected_members.iter().copied().collect();
    assert_eq!(
        listed, wanted,
        "case {case:?}: expected exactly the members this cluster entitles, got {listed:?}"
    );
    let mut dialled: BTreeSet<String> = BTreeSet::new();
    for (member, pairs) in &members {
        let probe = format!("{case} / {member}");
        assert!(
            !pairs.is_empty(),
            "case {probe:?}: a published member must carry a hostname:port pair"
        );
        for pair in pairs {
            let (host, port) = split(pair, &probe);
            // The host is this emulator's own interface — which a name of AWS's own, or one
            // composed under any other zone, is not: such a name resolves to nothing a
            // client on this machine can reach.
            assert!(
                ::service_common::data_plane_addressing::names_the_loopback_interface(&host)
                    && port > 0,
                "case {probe:?}: expected an address of this emulator, got {pair:?}"
            );
            // And it ANSWERS: the pair is dialled over the wire its member names, and the
            // broker behind it completes the first call every Kafka client makes.
            let mut client = match encrypted(member) {
                true => Client::tls(&probe, pair).await,
                false => Client::plaintext(&probe, pair).await,
            };
            let negotiated = client.api_versions(&probe).await;
            assert_eq!(
                negotiated.error_code, 0,
                "case {probe:?}: every published pair must answer ApiVersions, got error {}",
                negotiated.error_code
            );
            assert!(
                !negotiated.api_keys.is_empty(),
                "case {probe:?}: the broker behind a published pair must advertise its APIs"
            );
            // The broker there agrees with the control plane about where this cluster is
            // reached: every broker it reports is an address the SAME member publishes, so
            // a client that re-resolves the cluster from its metadata stays on the method
            // it bootstrapped over. Asked only where the method carries no SASL exchange:
            // a connection to a SASL path is served nothing but the frames that negotiate
            // that exchange until it has finished, as an unauthenticated connection to a
            // broker is.
            if !member.contains("Sasl") {
                let reported: BTreeSet<String> = client
                    .metadata(&probe)
                    .await
                    .brokers
                    .iter()
                    .map(|broker| format!("{}:{}", broker.host.as_str(), broker.port))
                    .collect();
                assert!(
                    !reported.is_empty(),
                    "case {probe:?}: the broker must report the cluster's broker list"
                );
                assert!(
                    reported.iter().all(|address| pairs.contains(address)),
                    "case {probe:?}: expected every broker reported to be one the member \
                     publishes {pairs:?}, got {reported:?}"
                );
            }
            dialled.insert(pair.clone());
        }
    }
    dialled
}

/// One row: the provisioned cluster to create, and the bootstrap members its encryption,
/// client authentication and connectivity entitle.
struct Row {
    case: &'static str,
    created: ClusterRequest,
    members: &'static [&'static str],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The published minimum: one broker node in one subnet, reached in the clear.
            // A cluster that requires no client authentication accepts unauthenticated
            // traffic, so the plaintext path is the one it offers.
            case: "one broker node, plaintext only",
            created: ClusterRequest {
                client_broker: Some("PLAINTEXT"),
                brokers: 1,
                subnets: vec!["subnet-a"],
                ..ClusterRequest::named("served-one-plaintext")
            },
            members: &["bootstrapBrokerString"],
        },
        Row {
            // Four access methods at once over two broker nodes: the plaintext and TLS
            // pair, which an unauthenticated cluster offers, plus both SASL mechanisms.
            case: "two broker nodes, unauthenticated plus both SASL mechanisms",
            created: ClusterRequest {
                client_broker: Some("TLS_PLAINTEXT"),
                unauthenticated: Some(true),
                sasl_scram: Some(true),
                sasl_iam: Some(true),
                brokers: 2,
                subnets: vec!["subnet-a", "subnet-b"],
                ..ClusterRequest::named("served-two-four-methods")
            },
            members: &[
                "bootstrapBrokerString",
                "bootstrapBrokerStringSaslIam",
                "bootstrapBrokerStringSaslScram",
                "bootstrapBrokerStringTls",
            ],
        },
        Row {
            // Every remaining member family: the public and multi-VPC siblings of a scheme
            // are access methods of their own, and a dual-stack cluster reaches each
            // method it offers over both network layers. Client certificates authenticate
            // the TLS path, which is what keeps it open on a cluster that admits no
            // unauthenticated traffic.
            case: "three broker nodes published publicly and over multi-VPC connectivity",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                tls_auth: Some(true),
                sasl_scram: Some(true),
                sasl_iam: Some(true),
                public_access: Some("SERVICE_PROVIDED_EIPS"),
                vpc_tls: Some(true),
                vpc_scram: Some(true),
                vpc_iam: Some(true),
                network_type: Some("DUAL"),
                brokers: 3,
                subnets: vec!["subnet-a", "subnet-b", "subnet-c"],
                ..ClusterRequest::named("served-three-public-dual")
            },
            members: &[
                "bootstrapBrokerStringPublicSaslIam",
                "bootstrapBrokerStringPublicSaslScram",
                "bootstrapBrokerStringPublicTls",
                "bootstrapBrokerStringSaslIam",
                "bootstrapBrokerStringSaslIamIpv6",
                "bootstrapBrokerStringSaslScram",
                "bootstrapBrokerStringSaslScramIpv6",
                "bootstrapBrokerStringTls",
                "bootstrapBrokerStringTlsIpv6",
                "bootstrapBrokerStringVpcConnectivitySaslIam",
                "bootstrapBrokerStringVpcConnectivitySaslScram",
                "bootstrapBrokerStringVpcConnectivityTls",
            ],
        },
    ]
}

#[tokio::test]
async fn every_published_bootstrap_pair_is_an_address_this_emulator_answers() {
    let emulator = Emulator::new();
    let rows = rows();
    let mut arns = Vec::new();
    for row in &rows {
        arns.push(emulator.create_ok(&row.created).await);
    }

    let mut announced: BTreeSet<String> = BTreeSet::new();
    for (row, arn) in rows.iter().zip(&arns) {
        let case = row.case;
        // A cluster still `CREATING` has no brokers to bootstrap against, and that move is
        // one the CLOCK owes it, so this LOOKS AGAIN until it publishes an address rather
        // than waiting out a span.
        let first = row
            .members
            .first()
            .expect("every row names the members it entitles");
        emulator.published_bootstrap(arn, first, case).await;
        for pair in assert_every_published_pair_answers(&emulator, arn, row.members, case).await {
            // No address is announced for two clusters: each cluster's brokers are its
            // own, so a client dialling a pair reaches the cluster it read it from.
            assert!(
                announced.insert(pair.clone()),
                "case {case:?}: the address {pair:?} is already another cluster's"
            );
        }
    }
}

#[tokio::test]
async fn the_serverless_bootstrap_string_is_an_address_this_emulator_answers() {
    let case = "an MSK Serverless cluster authenticating with IAM";
    // MSK Serverless admits its clients through one access path — SASL/IAM over TLS — so
    // the one string it publishes is the one asserted here.
    let members = ["bootstrapBrokerStringSaslIam"];
    let emulator = Emulator::new();
    let cluster = emulator
        .create_serverless_ok("served-serverless", &[vec!["subnet-0a1b2c3d"]], Some(true))
        .await;
    emulator.published_bootstrap(&cluster, members[0], case).await;
    let dialled = assert_every_published_pair_answers(&emulator, &cluster, &members, case).await;
    assert!(
        !dialled.is_empty(),
        "case {case:?}: a serverless cluster must publish an address to bootstrap at"
    );
}
