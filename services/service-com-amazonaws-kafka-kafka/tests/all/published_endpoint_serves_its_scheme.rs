// SPDX-License-Identifier: CC0-1.0

//! Every bootstrap member the control plane publishes is served by the socket behind
//! it, speaking the scheme that member is named for — including a member published
//! only because an `UpdateConnectivity` turned its access path on, and a member whose
//! scheme the cluster turns on for its MULTI-VPC connectivity rather than for its own
//! brokers.
//!
//! RULE under test: an address a service publishes for a caller to connect to is one
//! the service answers on with the protocol the published member is for, so a SASL
//! member's socket offers that member's mechanism and the exchange reaches an
//! authentication VERDICT — completed for a credential the cluster currently holds,
//! refused for any other — and a record produced to a partition over that address is
//! read back from it in produce order.
//!
//! The rows span what the rule generalises over: WHICH access path the published
//! member names (public, multi-VPC and the cluster's own), WHERE the cluster turns that
//! path's client authentication on (`ClientAuthentication`, or the
//! `ClientAuthentication` of its `VpcConnectivity`), WHICH mechanism a client offers on
//! it, WHICH credential it offers, and HOW MANY records cross it.
//!
//! Everything here goes over a real socket the core opened for the cluster, dialled at
//! the address `GetBootstrapBrokers` published; the client is the shared one in
//! `tests/all/broker_data_plane.rs`.

use serde_json::{json, Value};

use crate::broker_data_plane::{
    epoch_millis, given_secret_associated_as, given_topic, Client, AWS_MSK_IAM, OAUTHBEARER,
    SCRAM_SHA_512,
};
use crate::kafka_harness::{pct, poll_until_active, ClusterRequest, Emulator};

/// The SASL/SCRAM user a cluster below keeps associated, and the one it disassociates.
/// Both live in secrets of their own, which is how MSK holds a cluster's SCRAM
/// credentials.
const KEPT_USER: &str = "kept-user";
const KEPT_PASSWORD: &str = "kept-Secret-11";
const REVOKED_USER: &str = "revoked-user";
const REVOKED_PASSWORD: &str = "revoked-Secret-22";

/// The protocol's own codes: an exchange that did not authenticate the connection, and
/// a mechanism the port a client dialled does not take.
fn authentication_failed() -> i16 {
    ::kafka_protocol::ResponseError::SaslAuthenticationFailed.code()
}

/// See [`authentication_failed`].
fn unsupported_mechanism() -> i16 {
    ::kafka_protocol::ResponseError::UnsupportedSaslMechanism.code()
}

/// One expectation of a row: the published member to dial, the mechanism and credential
/// a client offers on it, and the code the exchange must answer.
struct Offered {
    member: &'static str,
    mechanism: &'static str,
    user: &'static str,
    credential: &'static str,
    expected: i16,
}

/// One row of the table: the cluster an update starts from, the connectivity it
/// submits, and what every published member it names must then answer.
struct Row {
    case: &'static str,
    created: ClusterRequest,
    connectivity: Value,
    offered: Vec<Offered>,
}

/// `PUT /v1/clusters/{ClusterArn}/connectivity` at the version the cluster is at.
async fn update_connectivity(emulator: &Emulator, arn: &str, version: &str, body: &Value) {
    let reply = emulator
        .call(
            "PUT",
            &format!("/v1/clusters/{}/connectivity", pct(arn)),
            Some(json!({ "currentVersion": version, "connectivityInfo": body })),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "UpdateConnectivity({body}) must be accepted, got {} {}",
        reply.status, reply.body
    );
}

/// A cluster that authenticates clients with the SASL schemes the arguments turn on,
/// over TLS — the shape every row below starts from.
fn sasl_cluster(name: &str, scram: bool, iam: bool) -> ClusterRequest {
    ClusterRequest {
        client_broker: Some("TLS"),
        sasl_scram: scram.then_some(true),
        sasl_iam: iam.then_some(true),
        ..ClusterRequest::named(name)
    }
}

/// Multi-VPC connectivity turning the named SASL schemes on, as `UpdateConnectivity`
/// takes it.
fn vpc_connectivity(scram: bool, iam: bool) -> Value {
    let mut sasl = serde_json::Map::new();
    if scram {
        sasl.insert("scram".to_string(), json!({ "enabled": true }));
    }
    if iam {
        sasl.insert("iam".to_string(), json!({ "enabled": true }));
    }
    json!({
        "vpcConnectivity": {
            "clientAuthentication": { "sasl": Value::Object(sasl) }
        }
    })
}

/// The table.
fn rows() -> Vec<Row> {
    vec![
        // Public access turned on by the UPDATE: the cluster's own client
        // authentication is what the public ports require, so each public SASL member
        // answers its own mechanism and refuses the other port's.
        Row {
            case: "public access turned on by an update, on a SCRAM and IAM cluster",
            created: sasl_cluster("scheme-public", true, true),
            connectivity: json!({ "publicAccess": { "type": "SERVICE_PROVIDED_EIPS" } }),
            offered: vec![
                Offered {
                    member: "bootstrapBrokerStringPublicSaslScram",
                    mechanism: SCRAM_SHA_512,
                    user: KEPT_USER,
                    credential: KEPT_PASSWORD,
                    expected: 0,
                },
                Offered {
                    member: "bootstrapBrokerStringPublicSaslScram",
                    mechanism: SCRAM_SHA_512,
                    user: KEPT_USER,
                    credential: "not-the-password",
                    expected: authentication_failed(),
                },
                Offered {
                    member: "bootstrapBrokerStringPublicSaslIam",
                    mechanism: AWS_MSK_IAM,
                    user: KEPT_USER,
                    credential: "a-signature",
                    expected: 0,
                },
                Offered {
                    member: "bootstrapBrokerStringPublicSaslIam",
                    mechanism: OAUTHBEARER,
                    user: KEPT_USER,
                    credential: "a-signature",
                    expected: 0,
                },
                // The port a member names still takes only its own mechanism.
                Offered {
                    member: "bootstrapBrokerStringPublicSaslIam",
                    mechanism: SCRAM_SHA_512,
                    user: KEPT_USER,
                    credential: KEPT_PASSWORD,
                    expected: unsupported_mechanism(),
                },
                Offered {
                    member: "bootstrapBrokerStringSaslScram",
                    mechanism: SCRAM_SHA_512,
                    user: KEPT_USER,
                    credential: KEPT_PASSWORD,
                    expected: 0,
                },
            ],
        },
        // IAM turned on for MULTI-VPC connectivity alone: the cluster's own client
        // authentication names no IAM access control at all, yet the control plane
        // publishes the multi-VPC SASL/IAM member — so that socket completes the IAM
        // exchange.
        Row {
            case: "multi-VPC SASL/IAM turned on by an update, on a SCRAM-only cluster",
            created: sasl_cluster("scheme-vpc-iam", true, false),
            connectivity: vpc_connectivity(false, true),
            offered: vec![
                Offered {
                    member: "bootstrapBrokerStringVpcConnectivitySaslIam",
                    mechanism: AWS_MSK_IAM,
                    user: KEPT_USER,
                    credential: "a-signature",
                    expected: 0,
                },
                Offered {
                    member: "bootstrapBrokerStringVpcConnectivitySaslIam",
                    mechanism: OAUTHBEARER,
                    user: KEPT_USER,
                    credential: "a-signature",
                    expected: 0,
                },
                // An IAM payload that is not a signed `Connect` request is refused on
                // it, which is the verdict and not a mechanism refusal.
                Offered {
                    member: "bootstrapBrokerStringVpcConnectivitySaslIam",
                    mechanism: AWS_MSK_IAM,
                    user: KEPT_USER,
                    credential: "",
                    expected: authentication_failed(),
                },
                Offered {
                    member: "bootstrapBrokerStringSaslScram",
                    mechanism: SCRAM_SHA_512,
                    user: KEPT_USER,
                    credential: KEPT_PASSWORD,
                    expected: 0,
                },
            ],
        },
        // SCRAM turned on for MULTI-VPC connectivity alone, on a cluster whose own
        // client authentication names only IAM: the published multi-VPC SASL/SCRAM
        // member's socket runs the SCRAM exchange and answers its verdict — a failure,
        // since a cluster whose own authentication names no SCRAM holds no SCRAM
        // secret to authenticate against.
        Row {
            case: "multi-VPC SASL/SCRAM turned on by an update, on an IAM-only cluster",
            created: sasl_cluster("scheme-vpc-scram", false, true),
            connectivity: vpc_connectivity(true, true),
            offered: vec![
                Offered {
                    member: "bootstrapBrokerStringVpcConnectivitySaslScram",
                    mechanism: SCRAM_SHA_512,
                    user: KEPT_USER,
                    credential: KEPT_PASSWORD,
                    expected: authentication_failed(),
                },
                Offered {
                    member: "bootstrapBrokerStringVpcConnectivitySaslIam",
                    mechanism: AWS_MSK_IAM,
                    user: KEPT_USER,
                    credential: "a-signature",
                    expected: 0,
                },
                Offered {
                    member: "bootstrapBrokerStringSaslIam",
                    mechanism: OAUTHBEARER,
                    user: KEPT_USER,
                    credential: "a-signature",
                    expected: 0,
                },
            ],
        },
        // Both halves at once: the public ports and the multi-VPC ones, on a cluster
        // that holds a SCRAM credential, so every published SASL member authenticates
        // the credential its scheme is for.
        Row {
            case: "public access and multi-VPC SASL turned on by one update",
            created: sasl_cluster("scheme-public-vpc", true, true),
            connectivity: json!({
                "publicAccess": { "type": "SERVICE_PROVIDED_EIPS" },
                "vpcConnectivity": {
                    "clientAuthentication": {
                        "sasl": { "scram": { "enabled": true }, "iam": { "enabled": true } }
                    }
                }
            }),
            offered: vec![
                Offered {
                    member: "bootstrapBrokerStringVpcConnectivitySaslScram",
                    mechanism: SCRAM_SHA_512,
                    user: KEPT_USER,
                    credential: KEPT_PASSWORD,
                    expected: 0,
                },
                Offered {
                    member: "bootstrapBrokerStringVpcConnectivitySaslIam",
                    mechanism: AWS_MSK_IAM,
                    user: KEPT_USER,
                    credential: "a-signature",
                    expected: 0,
                },
                Offered {
                    member: "bootstrapBrokerStringPublicSaslScram",
                    mechanism: SCRAM_SHA_512,
                    user: KEPT_USER,
                    credential: KEPT_PASSWORD,
                    expected: 0,
                },
                Offered {
                    member: "bootstrapBrokerStringPublicSaslIam",
                    mechanism: OAUTHBEARER,
                    user: KEPT_USER,
                    credential: "a-signature",
                    expected: 0,
                },
            ],
        },
    ]
}

/// RULE: every bootstrap member a cluster publishes after a connectivity update names a
/// socket this emulator answers the Apache Kafka protocol on, which offers that
/// member's own SASL mechanism and answers an authentication verdict for the credential
/// a client offers.
#[tokio::test]
async fn every_member_published_after_a_connectivity_update_answers_its_own_scheme() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::with_secrets_manager();
        let cluster = emulator.create_ok(&row.created).await;
        let version = poll_until_active(&emulator, &cluster, case).await;
        // A SCRAM credential lives in a Secrets Manager secret the cluster associates,
        // which MSK admits only for a cluster whose OWN client authentication names
        // SASL/SCRAM — so a row whose cluster does not holds none.
        if row.created.sasl_scram == Some(true) {
            given_secret_associated_as(
                &emulator,
                &cluster,
                &format!("{}-kept", row.created.name),
                KEPT_USER,
                KEPT_PASSWORD,
                case,
            )
            .await;
        }
        update_connectivity(&emulator, &cluster, &version, &row.connectivity).await;
        // The update is applied asynchronously, so the members it turns on are
        // published once the cluster is back to ACTIVE.
        poll_until_active(&emulator, &cluster, case).await;

        for offered in &row.offered {
            let probe = format!("{case} / {} / {}", offered.member, offered.mechanism);
            let authority = emulator
                .published_bootstrap(&cluster, offered.member, &probe)
                .await;
            let mut client = Client::tls(&probe, &authority).await;
            let negotiated = client.api_versions(&probe).await;
            assert_eq!(
                negotiated.error_code, 0,
                "case {probe:?}: the published address {authority} must answer ApiVersions, \
                 got error {}",
                negotiated.error_code
            );
            assert!(
                !negotiated.api_keys.is_empty(),
                "case {probe:?}: the broker at {authority} must advertise the APIs it serves"
            );
            let code = client
                .authenticate_as(&probe, offered.mechanism, offered.user, offered.credential)
                .await;
            assert_eq!(
                code, offered.expected,
                "case {probe:?}: expected the exchange at {authority} to answer code {}, got \
                 {code}",
                offered.expected
            );
            if code != 0 {
                continue;
            }
            // An authenticated connection on a published address is SERVED, and the
            // brokers it reports are reached at the address the client dialled.
            let metadata = client.metadata(&probe).await;
            let brokers: Vec<String> = metadata
                .brokers
                .iter()
                .map(|broker| format!("{}:{}", broker.host.as_str(), broker.port))
                .collect();
            assert!(
                brokers.contains(&authority),
                "case {probe:?}: Metadata must report the address the client dialled \
                 ({authority}), got {brokers:?}"
            );
        }
    }
}

/// RULE: a SASL/SCRAM endpoint authenticates exactly the credentials the cluster's
/// CURRENTLY associated secrets hold — a disassociated secret's credential stops
/// authenticating at once while every other secret's keeps working, and the refusal is
/// the protocol's own authentication failure rather than a dropped connection.
#[tokio::test]
async fn a_disassociated_secret_stops_authenticating_on_the_published_endpoint() {
    let case = "two associated secrets, one of them disassociated";
    let emulator = Emulator::with_secrets_manager();
    let cluster = emulator
        .create_ok(&sasl_cluster("scheme-revocation", true, true))
        .await;
    let version = poll_until_active(&emulator, &cluster, case).await;
    given_secret_associated_as(
        &emulator,
        &cluster,
        "revocation-kept",
        KEPT_USER,
        KEPT_PASSWORD,
        case,
    )
    .await;
    let revoked = given_secret_associated_as(
        &emulator,
        &cluster,
        "revocation-removed",
        REVOKED_USER,
        REVOKED_PASSWORD,
        case,
    )
    .await;
    update_connectivity(
        &emulator,
        &cluster,
        &version,
        &json!({ "publicAccess": { "type": "SERVICE_PROVIDED_EIPS" } }),
    )
    .await;
    poll_until_active(&emulator, &cluster, case).await;
    let authority = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerStringPublicSaslScram", case)
        .await;

    // While both secrets are associated, both credentials authenticate and nothing
    // else does. The rows differ along which credential is offered.
    let before: &[(&str, &str, &str, i16)] = &[
        ("the kept secret's user", KEPT_USER, KEPT_PASSWORD, 0),
        (
            "the secret about to be removed",
            REVOKED_USER,
            REVOKED_PASSWORD,
            0,
        ),
        (
            "a user's own password against the other user",
            KEPT_USER,
            REVOKED_PASSWORD,
            authentication_failed(),
        ),
        (
            "a user no secret of the cluster names",
            "never-associated-user",
            KEPT_PASSWORD,
            authentication_failed(),
        ),
    ];
    for (row, user, password, expected) in before {
        let probe = format!("{case} / before the disassociation / {row}");
        let code = scram_outcome(&probe, &authority, user, password).await;
        assert_eq!(
            code, *expected,
            "case {probe:?}: expected the exchange to answer code {expected}, got {code}"
        );
    }

    let removed = emulator
        .call(
            "PATCH",
            &format!("/v1/clusters/{}/scram-secrets", pct(&cluster)),
            Some(json!({ "secretArnList": [revoked.clone()] })),
        )
        .await;
    assert_eq!(
        removed.status, 200,
        "case {case:?}: BatchDisassociateScramSecret must be accepted, got {} {}",
        removed.status, removed.body
    );
    assert_eq!(
        removed.at("/unprocessedScramSecrets").as_array().map(Vec::len),
        Some(0),
        "case {case:?}: the disassociation must process the secret, got {}",
        removed.body
    );
    let listed = emulator
        .call(
            "GET",
            &format!("/v1/clusters/{}/scram-secrets", pct(&cluster)),
            None,
        )
        .await;
    let held: Vec<String> = listed
        .at("/secretArnList")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|arn| arn.as_str().map(str::to_string))
        .collect();
    assert!(
        !held.contains(&revoked),
        "case {case:?}: the disassociated secret must leave ListScramSecrets, got {held:?}"
    );
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: the cluster must still hold the other secret, got {held:?}"
    );

    // And the credential it held stops authenticating, while the one still associated
    // does not.
    let after: &[(&str, &str, &str, i16)] = &[
        (
            "the disassociated secret's user",
            REVOKED_USER,
            REVOKED_PASSWORD,
            authentication_failed(),
        ),
        ("the still-associated user", KEPT_USER, KEPT_PASSWORD, 0),
    ];
    for (row, user, password, expected) in after {
        let probe = format!("{case} / after the disassociation / {row}");
        let code = scram_outcome(&probe, &authority, user, password).await;
        assert_eq!(
            code, *expected,
            "case {probe:?}: expected the exchange to answer code {expected}, got {code}"
        );
    }
}

/// One SASL/SCRAM exchange on a published address, from a connection of its own: the
/// code its last leg answered.
async fn scram_outcome(case: &str, authority: &str, user: &str, password: &str) -> i16 {
    let mut client = Client::tls(case, authority).await;
    client.api_versions(case).await;
    client
        .authenticate_as(case, SCRAM_SHA_512, user, password)
        .await
}

/// RULE: records produced to one partition over a published endpoint are read back from
/// that partition in the order they were produced, at consecutive offsets from the
/// start of the log.
#[tokio::test]
async fn records_produced_over_a_published_endpoint_are_read_back_in_order() {
    let case = "ordered records over the public SASL/SCRAM endpoint";
    let emulator = Emulator::with_secrets_manager();
    let cluster = emulator
        .create_ok(&sasl_cluster("scheme-ordering", true, true))
        .await;
    let version = poll_until_active(&emulator, &cluster, case).await;
    given_secret_associated_as(
        &emulator,
        &cluster,
        "ordering-secret",
        KEPT_USER,
        KEPT_PASSWORD,
        case,
    )
    .await;
    update_connectivity(
        &emulator,
        &cluster,
        &version,
        &json!({ "publicAccess": { "type": "SERVICE_PROVIDED_EIPS" } }),
    )
    .await;
    poll_until_active(&emulator, &cluster, case).await;
    let authority = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerStringPublicSaslScram", case)
        .await;

    // The rows differ along HOW MANY records cross the endpoint and whether they arrive
    // as one batch or as a request each — both of which the per-partition order holds
    // across.
    let rows: &[(&str, &str, usize, bool)] = &[
        ("one record", "order-single", 1, true),
        ("a batch of records", "order-batch", 25, true),
        ("a request per record", "order-per-request", 7, false),
    ];
    for (row, topic, count, one_batch) in rows {
        let probe = format!("{case} / {row}");
        given_topic(&emulator, &cluster, topic, 1, None).await;
        let mut client = Client::tls(&probe, &authority).await;
        client.api_versions(&probe).await;
        let code = client
            .authenticate_as(&probe, SCRAM_SHA_512, KEPT_USER, KEPT_PASSWORD)
            .await;
        assert_eq!(
            code, 0,
            "case {probe:?}: the SCRAM exchange must authenticate the connection, got code {code}"
        );
        let produced: Vec<String> = (0..*count).map(|index| format!("record-{index}")).collect();
        let timestamp = epoch_millis();
        if *one_batch {
            let batch: Vec<(i64, Option<&str>, &str)> = produced
                .iter()
                .map(|value| (timestamp, None, value.as_str()))
                .collect();
            let base = client.produce(&probe, topic, 0, &batch).await;
            assert_eq!(
                base, 0,
                "case {probe:?}: the first batch must land at the start of the log, got {base}"
            );
        } else {
            for (index, value) in produced.iter().enumerate() {
                let base = client
                    .produce(&probe, topic, 0, &[(timestamp, None, value.as_str())])
                    .await;
                assert_eq!(
                    base, index as i64,
                    "case {probe:?}: record {index} must land at offset {index}, got {base}"
                );
            }
        }
        let read = client.fetch(&probe, topic, 0, 0).await;
        let values: Vec<String> = read.iter().map(|(_, _, value)| value.clone()).collect();
        assert_eq!(
            values, produced,
            "case {probe:?}: expected the consumed sequence to equal the produced one \
             {produced:?}, got {values:?}"
        );
        let offsets: Vec<i64> = read.iter().map(|(offset, _, _)| *offset).collect();
        let expected: Vec<i64> = (0..*count as i64).collect();
        assert_eq!(
            offsets, expected,
            "case {probe:?}: expected consecutive offsets {expected:?}, got {offsets:?}"
        );
    }
}
