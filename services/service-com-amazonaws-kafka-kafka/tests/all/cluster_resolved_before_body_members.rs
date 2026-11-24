// SPDX-License-Identifier: CC0-1.0

//! The ORDER in which an MSK operation that names the cluster it acts on as the
//! `{ClusterArn}` path label decides its refusals, driven end to end over the
//! same `MultiServiceRouter` the emulator serves from (the shared
//! `kafka_harness`).
//!
//! The RULE this suite asserts, over a table rather than one recorded request:
//! such an operation RESOLVES that cluster before it judges anything the request
//! BODY carries — so a request whose `{ClusterArn}` resolves to no cluster of
//! this endpoint is refused on the absent resource whatever the body contains,
//! and the very same request against a cluster the endpoint DOES hold is refused
//! (or served) about the body member instead. Only the SHAPE of the path
//! reference itself (a value that is not a cluster ARN at all,
//! `cluster_arn_refusal.rs`) and WHICH members the request carries are decided
//! earlier, because neither names a cluster to resolve.
//!
//! An operation whose model binds `NotFoundException` answers the absence as
//! HTTP 404 with the service's one fixed not-found sentence; one whose model
//! binds none answers the service's generic parameter sentence as HTTP 400,
//! still about the member that named the cluster. Both are refusals of the
//! cluster rather than of the body, which is what the rule is about.
//!
//! The rows span what the rule generalises over: which operation answers
//! (`BatchAssociateScramSecret`, `BatchDisassociateScramSecret`, `RebootBroker`,
//! `UpdateBrokerStorage`, `CreateTopic` and `PutClusterPolicy` — every HTTP
//! method of the set: POST, PATCH, PUT), which body member is at fault
//! (`secretArnList`, `brokerIds`, `targetBrokerEBSVolumeInfo`, `topicName`,
//! `policy`), and what makes that member's value refusable (an entry that is not
//! an ARN at all, another service's ARN, an empty string, a list carrying no
//! entry, a list carrying one entry too many, an illegal character, a reserved
//! name, and a document past the bound this emulator stores) — crossed with the
//! three ways a well-formed cluster ARN resolves to nothing here: a cluster that
//! was never created, one that was deleted, and one that lives in another
//! region.
//!
//! Where a refusal names the member at fault, the name is asserted
//! case-insensitively: WHICH member a refusal blames is this rule's business,
//! while the casing each operation spells it with is another issue's.

use serde_json::{json, Value};

// The service's own fixed refusal wordings and the bound a row is written
// against, so the suite and the handlers cannot drift apart on either.
use service_com_amazonaws_kafka_kafka::state::{
    BAD_REQUEST_MESSAGE, MAX_CLUSTER_POLICY_BYTES, RESOURCE_NOT_FOUND_MESSAGE,
    SECRET_ARN_INVALID_MESSAGE,
};

use super::kafka_harness::{
    pct, poll_until_active, ClusterRequest, Emulator, Reply, ACCOUNT, REGION,
};

/// A Secrets Manager secret ARN of the form `SecretArnList` carries, so a batch
/// can hold a usable entry beside an unusable one.
fn secret_arn(name: &str) -> String {
    format!("arn:aws:secretsmanager:{REGION}:{ACCOUNT}:secret:AmazonMSK_{name}")
}

/// A well-formed cluster ARN of this account and region that names no cluster.
fn ghost_cluster(name: &str) -> String {
    format!(
        "arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/{name}/00000000-0000-4000-8000-000000000000-1"
    )
}

/// What the SAME request is answered with once its cluster resolves.
enum Resolved {
    /// HTTP 400 `BadRequestException` about the body member this names, by its
    /// wire name (compared case-insensitively).
    Refused(&'static str),
    /// HTTP 200: this operation reports what it could not do per item rather
    /// than refusing the batch, so the body member is read and answered there.
    Served,
}

/// How the operation refuses a path reference it resolves no cluster for, which
/// is decided by whether its model binds `NotFoundException`.
enum Absent {
    /// HTTP 404 `NotFoundException`, the service's one fixed not-found sentence.
    NotFound,
    /// HTTP 400 `BadRequestException`, the service's generic parameter sentence:
    /// the operation models no `NotFoundException`.
    Parameter,
}

/// One row: an operation that names its cluster in the `{ClusterArn}` path
/// label, called with ONE body member value the service will not act on.
struct Row {
    /// What the row is, named in every assertion it makes.
    case: String,
    method: &'static str,
    /// The path under `/v1/clusters/{ClusterArn}` the model binds the operation
    /// to.
    suffix: &'static str,
    body: Value,
    absent: Absent,
    resolved: Resolved,
}

/// Every row, built against `current_version` — the version an accepted
/// `UpdateBrokerStorage` has to quote, so no row is refused on a stale one.
fn rows(current_version: &str) -> Vec<Row> {
    let scram = |operation: &str, method: &'static str, form: &str, list: Value| Row {
        case: format!("{operation} with a {form} secretArnList"),
        method,
        suffix: "/scram-secrets",
        body: json!({ "secretArnList": list }),
        absent: Absent::NotFound,
        // The association refuses a batch carrying an entry that is not a
        // Secrets Manager secret ARN; the disassociation answers every entry
        // that names no association of the cluster per item, in its one 200.
        resolved: if method == "POST" {
            Resolved::Refused("secretArnList")
        } else {
            Resolved::Served
        },
    };
    let reboot = |form: &str, ids: Value| Row {
        case: format!("RebootBroker with {form} brokerIds"),
        method: "PUT",
        suffix: "/reboot-broker",
        body: json!({ "brokerIds": ids }),
        absent: Absent::NotFound,
        resolved: Resolved::Refused("brokerIds"),
    };
    let storage = |form: &str, entries: Value| Row {
        case: format!("UpdateBrokerStorage with {form} targetBrokerEBSVolumeInfo"),
        method: "PUT",
        suffix: "/nodes/storage",
        body: json!({
            "currentVersion": current_version,
            "targetBrokerEBSVolumeInfo": entries,
        }),
        absent: Absent::NotFound,
        resolved: Resolved::Refused("targetBrokerEBSVolumeInfo"),
    };
    // `TopicName` is a BODY member of `CreateTopic` (`jsonName` `topicName`),
    // unlike the topic operations that carry it in the path.
    let create_topic = |form: &str, name: &str| Row {
        case: format!("CreateTopic with {form} topicName"),
        method: "POST",
        suffix: "/topics",
        body: json!({
            "topicName": name,
            "partitionCount": 1,
            "replicationFactor": 1,
        }),
        absent: Absent::NotFound,
        resolved: Resolved::Refused("topicName"),
    };

    let volume = |entries: usize| -> Value {
        Value::Array(
            (0..entries)
                .map(|_| json!({ "kafkaBrokerNodeId": "ALL", "volumeSizeGB": 1100 }))
                .collect(),
        )
    };

    let mut rows = Vec::new();
    for (operation, method) in [
        ("BatchAssociateScramSecret", "POST"),
        ("BatchDisassociateScramSecret", "PATCH"),
    ] {
        rows.push(scram(operation, method, "not-an-arn", json!(["not-a-secret-arn"])));
        rows.push(scram(
            operation,
            method,
            "another-service",
            json!(["arn:aws:s3:::a-bucket/a-key"]),
        ));
        rows.push(scram(operation, method, "empty-entry", json!([""])));
        rows.push(scram(
            operation,
            method,
            "usable-then-unusable",
            json!([secret_arn("order"), "not-a-secret-arn"]),
        ));
    }
    rows.push(reboot("no", json!([])));
    rows.push(reboot("two", json!(["1", "2"])));
    rows.push(storage("no", volume(0)));
    rows.push(storage("two", volume(2)));
    rows.push(create_topic("an empty", ""));
    rows.push(create_topic("an illegal-character", "order topic"));
    rows.push(create_topic("a reserved", ".."));
    // The bound this emulator stores one cluster's policy document within, one
    // byte past it.
    rows.push(Row {
        case: format!("PutClusterPolicy with a policy of {} bytes", MAX_CLUSTER_POLICY_BYTES + 1),
        method: "PUT",
        suffix: "/policy",
        body: json!({ "policy": "x".repeat(MAX_CLUSTER_POLICY_BYTES + 1) }),
        // `PutClusterPolicy` models BadRequest, Forbidden and
        // InternalServerError alone (`smithy_query(query="operation",
        // operation="PutClusterPolicy")`), so the absence is the parameter
        // refusal rather than the modelled miss.
        absent: Absent::Parameter,
        resolved: Resolved::Refused("policy"),
    });
    rows
}

/// Send one row against `cluster_arn`.
async fn send(emulator: &Emulator, cluster_arn: &str, row: &Row) -> Reply {
    emulator
        .call(
            row.method,
            &format!("/v1/clusters/{}{}", pct(cluster_arn), row.suffix),
            Some(row.body.clone()),
        )
        .await
}

/// Assert `reply` blames `member`, by the name the wire uses for it — compared
/// without regard to ASCII case, because which member is at fault is this rule's
/// business and the casing each operation spells it with is not.
fn assert_blames(reply: &Reply, case: &str, member: &str) {
    let named = reply.at("/invalidParameter").as_str().unwrap_or_default();
    assert!(
        named.eq_ignore_ascii_case(member),
        "case {case}: expected the refusal to blame {member}, got {}",
        reply.body
    );
}

/// RULE: the cluster the `{ClusterArn}` path label names is resolved FIRST, so a
/// well-formed cluster ARN this endpoint resolves no cluster for is answered
/// about that absent cluster whatever the request body carries — and the
/// identical request against a cluster the endpoint DOES hold is answered about
/// the body member instead.
#[tokio::test]
async fn a_cluster_the_endpoint_does_not_hold_is_answered_before_any_body_member() {
    let emulator = Emulator::new();

    // The cluster every row's "resolves" half runs against: live, SASL/SCRAM
    // enabled (so the SCRAM rows reach their secret lists) and provisioned (so
    // the topic row reaches its name), with the only thing wrong with those
    // requests being the body member under test.
    let live = emulator
        .create_ok(&ClusterRequest {
            sasl_scram: Some(true),
            ..ClusterRequest::named("body-order-live")
        })
        .await;
    // A second cluster, created now so both settle over one wait, which this
    // suite then DELETES: a deleted cluster is one of the three ways a
    // well-formed ARN resolves to nothing here.
    let doomed = emulator
        .create_ok(&ClusterRequest::named("body-order-deleted"))
        .await;
    // And a cluster of another region's endpoint, which this one holds no
    // record of.
    let other_region = emulator
        .create_ok_in("eu-central-1", &ClusterRequest::named("body-order-elsewhere"))
        .await;

    let live_version = poll_until_active(&emulator, &live, "body-order-live").await;
    poll_until_active(&emulator, &doomed, "body-order-deleted").await;
    let deleted = emulator
        .call("DELETE", &format!("/v1/clusters/{}", pct(&doomed)), None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "expected DeleteCluster to answer 200, got {} {}",
        deleted.status, deleted.body
    );

    let never_created = ghost_cluster("body-order-ghost");
    let unresolvable: [(&str, &str); 3] = [
        ("never-created", never_created.as_str()),
        ("deleted", doomed.as_str()),
        ("another-region", other_region.as_str()),
    ];

    for (target, cluster_arn) in unresolvable {
        // The version a request against an unresolvable cluster quotes cannot be
        // that cluster's, since there is none; it only has to be a version token.
        for row in rows("K3AEGXETSR30VB") {
            let case = format!("{target}: {}", row.case);
            let reply = send(&emulator, cluster_arn, &row).await;
            match row.absent {
                Absent::NotFound => {
                    assert_eq!(
                        (reply.status, reply.error_code()),
                        (404, "NotFoundException"),
                        "case {case}: expected 404 NotFoundException for a cluster this endpoint \
                         does not hold, got {} {}",
                        reply.status,
                        reply.body
                    );
                    assert_eq!(
                        reply.error_message(),
                        RESOURCE_NOT_FOUND_MESSAGE,
                        "case {case}: expected the service's fixed not-found sentence, got {:?}",
                        reply.error_message()
                    );
                }
                Absent::Parameter => {
                    assert_eq!(
                        (reply.status, reply.error_code()),
                        (400, "BadRequestException"),
                        "case {case}: expected the parameter refusal of an operation that models \
                         no NotFoundException, got {} {}",
                        reply.status,
                        reply.body
                    );
                    assert_eq!(
                        reply.error_message(),
                        BAD_REQUEST_MESSAGE,
                        "case {case}: expected the service's generic parameter sentence, got {:?}",
                        reply.error_message()
                    );
                }
            }
            // Whatever the class, the refusal is about the member that named the
            // cluster rather than about the body.
            assert_blames(&reply, &case, "clusterArn");
            // And it carries nothing of the request: the sentences above are
            // fixed end to end.
            for leaked in [cluster_arn, REGION, ACCOUNT] {
                assert!(
                    !reply.error_message().contains(leaked),
                    "case {case}: the message must carry no part of the request ({leaked:?}), got \
                     {:?}",
                    reply.error_message()
                );
            }
        }
    }

    // The SAME requests against the cluster this endpoint DOES hold are answered
    // about the body member instead — so the member values the rows carry really
    // are ones the service will not act on, and the answers above were the
    // cluster gate rather than a refusal that never reaches the body.
    for row in rows(&live_version) {
        let case = format!("live: {}", row.case);
        let reply = send(&emulator, &live, &row).await;
        match row.resolved {
            Resolved::Refused(member) => {
                assert_eq!(
                    (reply.status, reply.error_code()),
                    (400, "BadRequestException"),
                    "case {case}: expected 400 BadRequestException once the cluster resolves, got \
                     {} {}",
                    reply.status,
                    reply.body
                );
                assert_blames(&reply, &case, member);
            }
            Resolved::Served => {
                assert_eq!(
                    reply.status, 200,
                    "case {case}: expected the per-item outcomes to be served once the cluster \
                     resolves, got {} {}",
                    reply.status, reply.body
                );
                let unprocessed = reply
                    .at("/unprocessedScramSecrets")
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                assert!(
                    !unprocessed.is_empty(),
                    "case {case}: expected every entry that names no association to be reported \
                     per item, got {}",
                    reply.body
                );
            }
        }
    }

    // The association's own fixed sentence for a batch carrying an entry that is
    // not a Secrets Manager secret ARN, pinned once: the rows above assert WHICH
    // member it blames, and this asserts the wording it blames it in.
    let refused = emulator
        .call(
            "POST",
            &format!("/v1/clusters/{}/scram-secrets", pct(&live)),
            Some(json!({ "secretArnList": ["not-a-secret-arn"] })),
        )
        .await;
    assert_eq!(
        refused.error_message(),
        SECRET_ARN_INVALID_MESSAGE,
        "expected the association's fixed secret-ARN sentence, got {:?}",
        refused.error_message()
    );

    // None of those refusals touched the cluster: it is still ACTIVE, at the
    // version it was, with no operation in flight and no secret associated.
    let after = emulator.describe(&live).await;
    assert_eq!(
        after.at("/clusterInfo/state").as_str(),
        Some("ACTIVE"),
        "a refused request must leave the cluster ACTIVE, got {}",
        after.body
    );
    assert_eq!(
        after.at("/clusterInfo/currentVersion").as_str(),
        Some(live_version.as_str()),
        "a refused request must not move the cluster's version, got {}",
        after.at("/clusterInfo/currentVersion")
    );
    assert_eq!(
        after.at("/clusterInfo/activeOperationArn"),
        &Value::Null,
        "a refused request must start no cluster operation, got {}",
        after.at("/clusterInfo/activeOperationArn")
    );
    let secrets = emulator
        .call("GET", &format!("/v1/clusters/{}/scram-secrets", pct(&live)), None)
        .await;
    assert_eq!(
        secrets.at("/secretArnList").as_array().map(Vec::len),
        Some(0),
        "a refused batch must associate nothing, got {}",
        secrets.body
    );

    // And a request that is wrong in NOTHING is accepted on that same cluster,
    // which is what makes every refusal above about what the row carried.
    let accepted = emulator
        .call(
            "POST",
            &format!("/v1/clusters/{}/scram-secrets", pct(&live)),
            Some(json!({ "secretArnList": [secret_arn("order")] })),
        )
        .await;
    assert_eq!(
        accepted.status, 200,
        "expected a well-formed batch to be accepted, got {} {}",
        accepted.status, accepted.body
    );
}
