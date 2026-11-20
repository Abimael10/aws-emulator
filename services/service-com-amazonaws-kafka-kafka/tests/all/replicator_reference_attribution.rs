// SPDX-License-Identifier: CC0-1.0

//! WHICH input a replicator operation blames when it refuses a call, driven end
//! to end over the same router the emulator serves from (the shared harness in
//! `tests/all/kafka_harness.rs`).
//!
//! The RULE this suite asserts, over a TABLE rather than over one recorded
//! request: every operation of the replicator surface resolves the replicator the
//! path binds BEFORE it reads any other input, so a refusal caused by that
//! path-bound reference names the member that carried it (`replicatorArn`) as the
//! parameter at fault — never a parameter the call had not reached. An optional or
//! secondary input is blamed only when it is ITSELF the input at fault, which is
//! decided against the replicator the reference resolved to.
//!
//! The wire names and the bindings are the model's
//! (`smithy_query(query="operation",
//! operation="DescribeReplicator,DeleteReplicator,UpdateReplicationInfo")`):
//! `ReplicatorArn` is the `@httpLabel` of all three, `DeleteReplicator` takes its
//! `CurrentVersion` on the `currentVersion` query parameter and does not require
//! it, `UpdateReplicationInfo` takes it in the body as `currentVersion`,
//! `DescribeReplicator` takes no version at all, and `BadRequestException` /
//! `NotFoundException` both carry the blamed member in `invalidParameter`.
//!
//! The rows span what the rule generalises over: which operation answers (the
//! reader, the delete and the replication update, three methods and three version
//! bindings), what makes the reference unusable (six forms, from "not an ARN at
//! all" to an ARN of another resource kind of this service), and which version the
//! request quotes beside it (none, a well-formed version token, a lower-cased one
//! and a value that is not shaped like a version at all) — the attribution is the
//! reference's, not one version string's.
//!
//! The controls hold the rest of the surface fixed: a well-formed replicator ARN
//! this endpoint holds nothing for is the 404 naming the same member, and a
//! version quoted against a replicator that DID resolve is blamed on
//! `currentVersion`, so the rule above is an ordering of the guards rather than a
//! member name hard-coded into one refusal.
//!
//! Every expected sentence is WRITTEN OUT here rather than imported from the
//! implementation, so an assertion cannot agree with a constant that is wrong.

use serde_json::{json, Value};

use crate::kafka_harness::{pct, Emulator, Reply, ACCOUNT, REGION};

/// The wire name of the `@httpLabel` every replicator operation addresses its
/// replicator by, as the model spells it on the wire.
const REPLICATOR_ARN: &str = "replicatorArn";

/// The wire name of the version member, as `CurrentVersion`'s `httpQuery` on the
/// delete and its `jsonName` on the update both spell it.
const CURRENT_VERSION: &str = "currentVersion";

/// What Amazon MSK answers a replicator reference it cannot read: fixed text end
/// to end, with no part of the submitted value in it.
const VALUE_NOT_VALID: &str = "The provided value is not valid.";

/// What a replicator call answers a `currentVersion` that is not the version the
/// replicator it resolved is actually at. The member is part of the sentence and
/// neither version value is.
const VERSION_NOT_CURRENT: &str = "Invalid parameter currentVersion: The version of the \
                                   connector isn\u{2019}t current. Check the current version \
                                   and try again.";

/// An id segment shaped like the ones this service mints, for the well-formed
/// ARNs the rows build.
const GHOST_ID: &str = "11111111-2222-4333-8444-555555555555-1";

/// A version token of the shape this service mints, for the rows that quote a
/// well-formed one.
const A_VERSION_TOKEN: &str = "K3AEGXETSR30VB";

/// The IAM role the control below hands the replicator it creates: a role of the
/// caller's own account, so the pass-role decision admits it and the rule under
/// test is what answers.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/MskReplicatorRole";

/// The subnets that create places the replicator's client interfaces in, in the
/// Amazon EC2 id form.
const SUBNETS: [&str; 2] = ["subnet-0a1b2c3d4e5f60718", "subnet-0a1b2c3d4e5f60719"];

/// Where an operation of this surface carries the replicator's version.
#[derive(Clone, Copy, Debug)]
enum VersionBinding {
    /// The operation takes no version (`DescribeReplicator`).
    Absent,
    /// On the `currentVersion` query parameter (`DeleteReplicator`).
    Query,
    /// In the request body (`UpdateReplicationInfo`).
    Body,
}

/// One operation of the replicator surface, as the model binds it.
#[derive(Clone, Copy, Debug)]
struct Call {
    name: &'static str,
    method: &'static str,
    /// What follows the percent-encoded ARN in the operation's URI.
    suffix: &'static str,
    version: VersionBinding,
}

impl Call {
    /// The request one row sends: the URI the model binds, with the quoted
    /// version placed where this operation carries it.
    fn request(&self, arn: &str, quoted: Option<&str>) -> (String, Option<Value>) {
        let base = format!("/replication/v1/replicators/{}{}", pct(arn), self.suffix);
        match self.version {
            VersionBinding::Absent => (base, None),
            VersionBinding::Query => match quoted {
                Some(version) => (
                    format!("{base}?{CURRENT_VERSION}={}", pct(version)),
                    None,
                ),
                None => (base, None),
            },
            // The update's version is a required body member, so a row that
            // quotes nothing still sends a well-formed token: what varies is the
            // value, not whether the request is complete.
            VersionBinding::Body => (
                base,
                Some(update_body(quoted.unwrap_or(A_VERSION_TOKEN))),
            ),
        }
    }

    /// The versions a row of the 400 table quotes. Every value is answered the
    /// same way, because the reference is read before any of them.
    fn unreadable_probes(&self) -> Vec<Option<&'static str>> {
        match self.version {
            VersionBinding::Absent => vec![None],
            _ => vec![
                None,
                Some(A_VERSION_TOKEN),
                Some("k3aegxetsr30vb"),
                Some("not-a-version"),
            ],
        }
    }

    /// The versions a row of the 404 table quotes: tokens of the shape this
    /// service mints, so what the row tests is the resolution of the reference
    /// rather than a version this surface will not read at all.
    fn resolvable_probes(&self) -> Vec<Option<&'static str>> {
        match self.version {
            VersionBinding::Absent => vec![None],
            _ => vec![None, Some(A_VERSION_TOKEN), Some("K0000000000000")],
        }
    }
}

/// The three operations that address a replicator by the ARN in their path.
fn calls() -> Vec<Call> {
    vec![
        Call {
            name: "DescribeReplicator",
            method: "GET",
            suffix: "",
            version: VersionBinding::Absent,
        },
        Call {
            name: "DeleteReplicator",
            method: "DELETE",
            suffix: "",
            version: VersionBinding::Query,
        },
        Call {
            name: "UpdateReplicationInfo",
            method: "PUT",
            suffix: "/replication-info",
            version: VersionBinding::Body,
        },
    ]
}

/// An `UpdateReplicationInfo` body that is complete on its own terms: the version
/// the model requires, each end of the flow named exactly once, and a topic
/// selection whose every member the model requires
/// (`smithy_query(query="shape",shape="TopicReplicationUpdate")`) — so nothing but
/// the path's reference is at fault.
fn update_body(current_version: &str) -> Value {
    json!({
        "currentVersion": current_version,
        "sourceKafkaClusterArn": ghost_cluster_arn("update-source"),
        "targetKafkaClusterArn": ghost_cluster_arn("update-target"),
        "topicReplication": {
            "topicsToReplicate": [".*"],
            "topicsToExclude": ["excluded.*"],
            "copyAccessControlListsForTopics": false,
            "copyTopicConfigurations": false,
            "detectAndCopyNewTopics": false,
        },
    })
}

/// A well-formed Amazon MSK cluster ARN of this endpoint's region and account
/// that was never issued — a reference another resource of this service owns, and
/// one this operation does not reach on any row below.
fn ghost_cluster_arn(name: &str) -> String {
    format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/{name}/{GHOST_ID}")
}

/// Every way a submitted value can fail to be a replicator ARN this service could
/// have minted.
fn unreadable_references() -> Vec<(&'static str, String)> {
    vec![
        ("not-an-arn-at-all", "not-a-replicator-arn".to_string()),
        ("free-text-with-spaces", "not an arn at all".to_string()),
        ("truncated-arn", "arn:aws:kafka".to_string()),
        ("a-non-ascii-value", "ニセのレプリケーター".to_string()),
        (
            "an-arn-of-another-service",
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:instance/i-0a1b2c3d"),
        ),
        (
            "an-arn-of-another-resource-kind-of-this-service",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/a-cluster/{GHOST_ID}"),
        ),
        (
            "a-replicator-arn-missing-its-id-segment",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:replicator/only-a-name"),
        ),
    ]
}

/// RULE: a replicator operation handed a reference it cannot read refuses it with
/// HTTP 400 `BadRequestException` naming the member that carried the reference,
/// whatever makes the reference unusable, whichever operation of the surface was
/// called and whatever version the request quoted beside it — and with the one
/// fixed sentence, which echoes neither the reference nor the version.
#[tokio::test]
async fn an_unreadable_replicator_reference_is_blamed_on_the_member_that_carried_it() {
    let emulator = Emulator::new();

    for call in calls() {
        for (form, reference) in unreadable_references() {
            for quoted in call.unreadable_probes() {
                let case = format!("{}/{form}/{CURRENT_VERSION}={quoted:?}", call.name);
                let (uri, body) = call.request(&reference, quoted);
                let reply = emulator.call(call.method, &uri, body).await;
                assert_eq!(
                    (reply.status, reply.error_code()),
                    (400, "BadRequestException"),
                    "case {case}: expected 400 BadRequestException, got {} {}",
                    reply.status,
                    reply.body
                );
                assert_eq!(
                    reply.at("/invalidParameter").as_str(),
                    Some(REPLICATOR_ARN),
                    "case {case}: expected the refusal to blame {REPLICATOR_ARN}, got {}",
                    reply.body
                );
                assert_eq!(
                    reply.error_message(),
                    VALUE_NOT_VALID,
                    "case {case}: expected the fixed sentence {VALUE_NOT_VALID:?}, got {:?}",
                    reply.error_message()
                );
                assert!(
                    !reply.error_message().contains(&reference),
                    "case {case}: the refusal must not echo the submitted reference, got {:?}",
                    reply.error_message()
                );
                if let Some(version) = quoted {
                    assert!(
                        !reply.error_message().contains(version),
                        "case {case}: the refusal must not echo the submitted version, got {:?}",
                        reply.error_message()
                    );
                }
            }
        }
    }
}

/// RULE: a reference this surface CAN read but holds no replicator for is
/// answered on the RESOURCE — HTTP 404 `NotFoundException` naming the same
/// member — whichever operation was called and whatever version the request
/// quoted: the version is never what a reference that resolves to nothing is
/// blamed on.
#[tokio::test]
async fn a_readable_reference_that_holds_no_replicator_is_the_not_found_naming_it() {
    let emulator = Emulator::new();

    // Well-formed replicator ARNs of this endpoint's own region and account, and
    // one of another region, which this registry holds no record of just the same.
    let absent = [
        (
            "this-region",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:replicator/absent/{GHOST_ID}"),
        ),
        (
            "another-region",
            format!("arn:aws:kafka:eu-west-1:{ACCOUNT}:replicator/absent/{GHOST_ID}"),
        ),
        (
            "another-account",
            format!("arn:aws:kafka:{REGION}:111122223333:replicator/absent/{GHOST_ID}"),
        ),
    ];

    for call in calls() {
        for (where_from, arn) in &absent {
            for quoted in call.resolvable_probes() {
                let case = format!("{}/{where_from}/{CURRENT_VERSION}={quoted:?}", call.name);
                let (uri, body) = call.request(arn, quoted);
                let reply = emulator.call(call.method, &uri, body).await;
                assert_eq!(
                    (reply.status, reply.error_code()),
                    (404, "NotFoundException"),
                    "case {case}: expected 404 NotFoundException, got {} {}",
                    reply.status,
                    reply.body
                );
                assert_eq!(
                    reply.at("/invalidParameter").as_str(),
                    Some(REPLICATOR_ARN),
                    "case {case}: expected the refusal to blame {REPLICATOR_ARN}, got {}",
                    reply.body
                );
            }
        }
    }
}

/// RULE: once the reference RESOLVES, the version the request quoted is checked
/// against the replicator's own and is what a stale quote is blamed on — so the
/// member a replicator refusal names follows which input is at fault, and a
/// delete that states no version pre-condition, or quotes the version the
/// replicator is at, removes it.
#[tokio::test]
async fn a_resolved_replicator_blames_the_version_it_was_quoted() {
    let emulator = Emulator::new();

    // Two replicators, so the stale-version rows and the no-version control each
    // act on their own resource.
    let quoted_delete = create_replicator(&emulator, "attribution-quoted").await;
    let bare_delete = create_replicator(&emulator, "attribution-bare").await;

    let described = describe(&emulator, &quoted_delete).await;
    assert_eq!(
        described.status, 200,
        "expected the created replicator to be readable, got {} {}",
        described.status, described.body
    );
    let current_version = described
        .at("/currentVersion")
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "expected DescribeReplicator to report a currentVersion, got {}",
                described.body
            )
        })
        .to_string();

    // Each row is a version the replicator is NOT at: a token of the shape the
    // service mints, one differing only in case from its own, and the next
    // token's shape. Every one is the version's fault, so every one is blamed on
    // the version.
    let stale = [
        A_VERSION_TOKEN.to_string(),
        "K0000000000000".to_string(),
        current_version.to_lowercase(),
    ];
    for version in &stale {
        let case = format!("stale={version:?}");
        let reply = emulator
            .call(
                "DELETE",
                &format!(
                    "/replication/v1/replicators/{}?{CURRENT_VERSION}={}",
                    pct(&quoted_delete),
                    pct(version)
                ),
                None,
            )
            .await;
        assert_eq!(
            (
                reply.status,
                reply.error_code(),
                reply.at("/invalidParameter").as_str(),
                reply.error_message(),
            ),
            (
                400,
                "BadRequestException",
                Some(CURRENT_VERSION),
                VERSION_NOT_CURRENT,
            ),
            "case {case}: expected the stale-version refusal to blame {CURRENT_VERSION}, got {} {}",
            reply.status,
            reply.body
        );
        assert_eq!(
            describe(&emulator, &quoted_delete).await.status,
            200,
            "case {case}: a refused delete must leave the replicator in place"
        );
    }

    // The replicator's own version satisfies the pre-condition, so the delete is
    // accepted and every later read of that ARN is the not-found refusal.
    let reply = emulator
        .call(
            "DELETE",
            &format!(
                "/replication/v1/replicators/{}?{CURRENT_VERSION}={}",
                pct(&quoted_delete),
                pct(&current_version)
            ),
            None,
        )
        .await;
    assert_eq!(
        (reply.status, reply.at("/replicatorState").as_str()),
        (200, Some("DELETING")),
        "expected the delete quoting the replicator's own version to be accepted, got {} {}",
        reply.status,
        reply.body
    );
    let gone = describe(&emulator, &quoted_delete).await;
    assert_eq!(
        (gone.status, gone.error_code()),
        (404, "NotFoundException"),
        "expected the deleted replicator to be served to nobody, got {} {}",
        gone.status,
        gone.body
    );

    // A delete that states no pre-condition at all is accepted on the same terms:
    // the optional version is a pre-condition, not a requirement.
    let reply = emulator
        .call(
            "DELETE",
            &format!("/replication/v1/replicators/{}", pct(&bare_delete)),
            None,
        )
        .await;
    assert_eq!(
        (reply.status, reply.at("/replicatorState").as_str()),
        (200, Some("DELETING")),
        "expected a delete quoting no version to be accepted, got {} {}",
        reply.status,
        reply.body
    );
}

/// `DescribeReplicator`, which the model binds to
/// `GET /replication/v1/replicators/{ReplicatorArn}`.
async fn describe(emulator: &Emulator, arn: &str) -> Reply {
    emulator
        .call(
            "GET",
            &format!("/replication/v1/replicators/{}", pct(arn)),
            None,
        )
        .await
}

/// `CreateReplicator` with one flow between two Amazon MSK clusters, asserted
/// admitted, handing back the ARN it minted. The clusters it names are another
/// resource's to resolve, so the create is admitted on what the request says and
/// the replicator this suite acts on is a real record of the store.
async fn create_replicator(emulator: &Emulator, name: &str) -> String {
    let source = ghost_cluster_arn(&format!("{name}-source"));
    let target = ghost_cluster_arn(&format!("{name}-target"));
    let body = json!({
        "replicatorName": name,
        "serviceExecutionRoleArn": ROLE_ARN,
        "kafkaClusters": [
            {
                "amazonMskCluster": { "mskClusterArn": source },
                "vpcConfig": { "subnetIds": SUBNETS },
            },
            {
                "amazonMskCluster": { "mskClusterArn": target },
                "vpcConfig": { "subnetIds": SUBNETS },
            },
        ],
        "replicationInfoList": [{
            "sourceKafkaClusterArn": source,
            "targetKafkaClusterArn": target,
            "targetCompressionType": "NONE",
            "topicReplication": { "topicsToReplicate": [".*"] },
            "consumerGroupReplication": { "consumerGroupsToReplicate": [".*"] },
        }],
    });
    let reply = emulator
        .call("POST", "/replication/v1/replicators", Some(body))
        .await;
    assert_eq!(
        (reply.status, reply.error_code()),
        (200, ""),
        "case {name}: expected CreateReplicator to admit the request, got {} {}",
        reply.status,
        reply.body
    );
    reply
        .at("/replicatorArn")
        .as_str()
        .unwrap_or_else(|| panic!("case {name}: CreateReplicator returned no replicatorArn"))
        .to_string()
}
