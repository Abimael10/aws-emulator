// SPDX-License-Identifier: CC0-1.0

//! `GetBootstrapBrokers`, driven end to end over the same `MultiServiceRouter` the
//! emulator serves from, through the shared harness in
//! `tests/all/kafka_harness.rs`.
//!
//! RULE under test: every member of the answer is a DIFFERENT connection scheme, and
//! a member is populated exactly when the cluster accepts a client arriving on that
//! scheme — the client-broker encryption setting has to admit the scheme's transport
//! AND the cluster's client authentication has to admit the credential (or lack of
//! one) that scheme carries, with the connectivity configuration deciding the
//! public, multi-VPC and IPv6 siblings. So the plaintext and TLS strings, which
//! carry no SASL exchange, belong to a cluster that accepts unauthenticated traffic
//! (or, for TLS, authenticates clients by certificate), and each SASL string belongs
//! to the mechanism it is named for. A member the cluster does not entitle is ABSENT
//! from the body rather than present and empty.
//!
//! The rows differ along what the rule generalises over: which authentication
//! mechanisms the cluster turns on (none, SASL/SCRAM, SASL/IAM, both, mutual TLS),
//! whether unauthenticated traffic is named explicitly or left to its default, which
//! transports the encryption setting admits (`TLS_PLAINTEXT` by default, `TLS`,
//! `PLAINTEXT`), and which extra connectivity paths are on (public access, multi-VPC,
//! dual-stack).

use std::collections::BTreeSet;

use serde_json::Value;

use crate::cluster_lifecycle::BOOTSTRAP_MEMBERS;
use crate::kafka_harness::{sleep_past_activation, ClusterRequest, Emulator};

/// One row: the cluster to create, and exactly the bootstrap members its settings
/// entitle. Every other member of `BOOTSTRAP_MEMBERS` — the catalog of the output
/// members this service's model declares — must be absent.
struct Row {
    case: &'static str,
    created: ClusterRequest,
    entitled: &'static [&'static str],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no client authentication at all: unauthenticated traffic is on, and \
                   `ClientBroker` defaults to TLS_PLAINTEXT",
            created: ClusterRequest::named("entitle-defaulted"),
            entitled: &["bootstrapBrokerString", "bootstrapBrokerStringTls"],
        },
        Row {
            case: "unauthenticated traffic named explicitly, nothing else: the same two \
                   anonymous paths",
            created: ClusterRequest {
                unauthenticated: Some(true),
                ..ClusterRequest::named("entitle-unauthenticated")
            },
            entitled: &["bootstrapBrokerString", "bootstrapBrokerStringTls"],
        },
        Row {
            case: "SASL/SCRAM only, TLS encryption: a credential is required, so only the \
                   SCRAM string",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                sasl_scram: Some(true),
                ..ClusterRequest::named("entitle-scram")
            },
            entitled: &["bootstrapBrokerStringSaslScram"],
        },
        Row {
            case: "SASL/IAM only, TLS encryption: only the IAM string",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                sasl_iam: Some(true),
                ..ClusterRequest::named("entitle-iam")
            },
            entitled: &["bootstrapBrokerStringSaslIam"],
        },
        Row {
            case: "SASL/IAM only, left on the defaulted TLS_PLAINTEXT encryption: the \
                   transport admits plaintext but no client may arrive without a credential",
            created: ClusterRequest {
                sasl_iam: Some(true),
                ..ClusterRequest::named("entitle-iam-defaulted-encryption")
            },
            entitled: &["bootstrapBrokerStringSaslIam"],
        },
        Row {
            case: "both SASL mechanisms: one string each, and nothing anonymous",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                sasl_scram: Some(true),
                sasl_iam: Some(true),
                ..ClusterRequest::named("entitle-both-sasl")
            },
            entitled: &[
                "bootstrapBrokerStringSaslScram",
                "bootstrapBrokerStringSaslIam",
            ],
        },
        Row {
            case: "SASL mechanisms turned explicitly OFF: no mechanism is required, so the \
                   anonymous paths are the cluster's",
            created: ClusterRequest {
                sasl_scram: Some(false),
                sasl_iam: Some(false),
                ..ClusterRequest::named("entitle-sasl-off")
            },
            entitled: &["bootstrapBrokerString", "bootstrapBrokerStringTls"],
        },
        Row {
            case: "SASL/SCRAM beside unauthenticated traffic turned ON: the SCRAM string and \
                   the anonymous ones together",
            created: ClusterRequest {
                sasl_scram: Some(true),
                unauthenticated: Some(true),
                ..ClusterRequest::named("entitle-scram-and-unauthenticated")
            },
            entitled: &[
                "bootstrapBrokerString",
                "bootstrapBrokerStringTls",
                "bootstrapBrokerStringSaslScram",
            ],
        },
        Row {
            case: "mutual TLS only: the client is authenticated by its certificate on the \
                   TLS path, and the plaintext path carries no certificate exchange",
            created: ClusterRequest {
                client_broker: Some("TLS_PLAINTEXT"),
                tls_auth: Some(true),
                ..ClusterRequest::named("entitle-mutual-tls")
            },
            entitled: &["bootstrapBrokerStringTls"],
        },
        Row {
            case: "mutual TLS with unauthenticated traffic turned OFF explicitly: the TLS \
                   path stays, the plaintext one does not",
            created: ClusterRequest {
                tls_auth: Some(true),
                unauthenticated: Some(false),
                ..ClusterRequest::named("entitle-mutual-tls-no-anonymous")
            },
            entitled: &["bootstrapBrokerStringTls"],
        },
        Row {
            case: "PLAINTEXT-only encryption with no authentication: the TLS transport is \
                   not admitted at all",
            created: ClusterRequest {
                client_broker: Some("PLAINTEXT"),
                ..ClusterRequest::named("entitle-plaintext-only")
            },
            entitled: &["bootstrapBrokerString"],
        },
        Row {
            case: "PLAINTEXT-only encryption with SASL/SCRAM: the transport admits plaintext \
                   but the cluster admits no anonymous client",
            created: ClusterRequest {
                client_broker: Some("PLAINTEXT"),
                sasl_scram: Some(true),
                ..ClusterRequest::named("entitle-plaintext-scram")
            },
            entitled: &["bootstrapBrokerStringSaslScram"],
        },
        Row {
            case: "public access on a SASL/IAM, dual-stack cluster: the IAM string, its \
                   public sibling and its IPv6 sibling",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                sasl_iam: Some(true),
                public_access: Some("SERVICE_PROVIDED_EIPS"),
                network_type: Some("DUAL"),
                ..ClusterRequest::named("entitle-public-iam-dual")
            },
            entitled: &[
                "bootstrapBrokerStringSaslIam",
                "bootstrapBrokerStringPublicSaslIam",
                "bootstrapBrokerStringSaslIamIpv6",
            ],
        },
        Row {
            case: "public access on a mutual-TLS cluster: the public TLS sibling follows the \
                   TLS path the certificate authenticates",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                tls_auth: Some(true),
                public_access: Some("SERVICE_PROVIDED_EIPS"),
                ..ClusterRequest::named("entitle-public-mutual-tls")
            },
            entitled: &[
                "bootstrapBrokerStringTls",
                "bootstrapBrokerStringPublicTls",
            ],
        },
        Row {
            case: "multi-VPC connectivity on a SASL/SCRAM cluster: the multi-VPC members are \
                   the ones its own client authentication turns on",
            created: ClusterRequest {
                client_broker: Some("TLS"),
                sasl_scram: Some(true),
                vpc_tls: Some(true),
                vpc_scram: Some(true),
                vpc_iam: Some(false),
                ..ClusterRequest::named("entitle-multi-vpc-scram")
            },
            entitled: &[
                "bootstrapBrokerStringSaslScram",
                "bootstrapBrokerStringVpcConnectivityTls",
                "bootstrapBrokerStringVpcConnectivitySaslScram",
            ],
        },
        Row {
            case: "a dual-stack cluster with no authentication: both anonymous paths and \
                   both of their IPv6 siblings",
            created: ClusterRequest {
                network_type: Some("DUAL"),
                ..ClusterRequest::named("entitle-dual-anonymous")
            },
            entitled: &[
                "bootstrapBrokerString",
                "bootstrapBrokerStringTls",
                "bootstrapBrokerStringIpv6",
                "bootstrapBrokerStringTlsIpv6",
            ],
        },
    ]
}

/// The members the answer populated, as a set.
fn populated(case: &str, body: &Value) -> BTreeSet<String> {
    let object = body
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: GetBootstrapBrokers answered {body}"));
    object
        .iter()
        .filter(|(member, _)| BOOTSTRAP_MEMBERS.contains(&member.as_str()))
        .map(|(member, _)| member.clone())
        .collect()
}

#[tokio::test]
async fn every_bootstrap_member_follows_the_clusters_encryption_and_authentication() {
    let emulator = Emulator::new();
    let rows = rows();

    // Every row's cluster first, then ONE activation span: a cluster still
    // `CREATING` has no brokers to bootstrap against, so the reader refuses it, and
    // these rows are about which schemes a SETTLED cluster offers.
    let mut arns = Vec::new();
    for row in &rows {
        arns.push(emulator.create_ok(&row.created).await);
    }
    sleep_past_activation();

    for (row, arn) in rows.iter().zip(&arns) {
        let case = row.case;
        let reply = emulator.bootstrap_brokers(arn).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected GetBootstrapBrokers to return 200, got {} {}",
            reply.status, reply.body
        );

        let expected: BTreeSet<String> =
            row.entitled.iter().map(|member| (*member).to_string()).collect();
        let actual = populated(case, &reply.body);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected exactly {expected:?} populated, got {actual:?} in {}",
            reply.body
        );

        // A member the cluster does not entitle is ABSENT, not present and empty:
        // asserted over the whole catalog of modelled members, so a member no row
        // names is checked too.
        for member in BOOTSTRAP_MEMBERS {
            if expected.contains(*member) {
                let value = reply
                    .body
                    .get(*member)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected {member}, got {}", reply.body)
                    });
                for endpoint in value.split(',') {
                    let (host, port) = endpoint.rsplit_once(':').unwrap_or_else(|| {
                        panic!("case {case:?}: {member} entry {endpoint:?} is not a host:port pair")
                    });
                    assert!(
                        !host.is_empty() && port.parse::<u16>().is_ok_and(|port| port > 0),
                        "case {case:?}: {member} must name an address this emulator serves, got \
                         {endpoint:?}"
                    );
                }
                continue;
            }
            assert!(
                reply.body.get(*member).is_none(),
                "case {case:?}: {member} must be absent for a cluster whose configuration does \
                 not offer it, got {:?}",
                reply.body.get(*member)
            );
        }

        // Reporting the endpoints changes nothing: a second read answers the same
        // members with the same addresses.
        let again = emulator.bootstrap_brokers(arn).await;
        assert_eq!(
            again.body, reply.body,
            "case {case:?}: a second read must answer the same endpoints, got {} after {}",
            again.body, reply.body
        );
    }
}
