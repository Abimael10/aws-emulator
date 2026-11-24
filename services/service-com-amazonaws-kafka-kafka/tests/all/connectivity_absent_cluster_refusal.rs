// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a mutation of a cluster's CLIENT-CONNECTIVITY surface takes for a
//! cluster ARN this endpoint holds no cluster for, driven end to end over the same
//! `MultiServiceRouter` the emulator serves from (the shared `kafka_harness`).
//!
//! RULE under test: a call that CHANGES how clients reach a cluster — the multi-VPC
//! and public-access surface a cluster's own resource-based policy is attached for —
//! can be allowed only by a document attached to the cluster it applies to or by one
//! of the caller's own that names that cluster, so a well-formed MSK cluster ARN this
//! endpoint holds no cluster for is allowed by NOTHING: the call is refused as
//! unauthorized (HTTP 403, `AccessDeniedException`, IAM's sentence for a request no
//! resource-based policy allows, over the principal that signed, `kafka:<ApiName>`
//! and the ARN exactly as submitted) rather than told the cluster is absent — so "you
//! may not" and "it is not there" are the one answer and nothing of what the registry
//! holds is disclosed.
//!
//! The rows differ along what the rule generalises over: the OPERATION taking the
//! decision (both mutations of that surface — `UpdateConnectivity` and
//! `RejectClientVpcConnection`), the way this endpoint comes to hold no cluster under
//! the submitted ARN (a name never created, an ARN of the shape this service mints
//! with another name and id, a cluster id with a different revision suffix, a cluster
//! that was created and then deleted, and a live cluster's own ARN seen from another
//! region's endpoint), and — for `UpdateConnectivity` — the BODY the request carries
//! (each connectivity member the operation can change, a version token the cluster
//! would refuse, and a body naming nothing to change at all), since the decision
//! precedes every reading of the body.
//!
//! The boundaries are pinned too: a value that is no cluster ARN at all is left to the
//! operation's own parameter refusal (HTTP 400), the REPORTING side of the same ARN
//! still answers the absence as the absence it is (`DescribeCluster`, HTTP 404), and
//! the cluster this endpoint DOES hold has its connectivity update accepted — so the
//! rows above are refusals about the target rather than about the caller or the
//! request.

use serde_json::{json, Value};
use service_com_amazonaws_kafka_kafka::state::authorization::caller_principal;

use crate::kafka_harness::{
    pct, poll_until_active, ClusterRequest, Emulator, Reply, ACCOUNT, REGION,
};

/// The status and wire code an authorization refusal carries, spelled out here
/// rather than read from the service so a drift in its own constants fails this
/// suite.
const ACCESS_DENIED_STATUS: u16 = 403;
const ACCESS_DENIED_CODE: &str = "AccessDeniedException";

/// The status and wire code the service reports a resource it does not hold with,
/// which is what the REPORTING side of the same ARN still answers.
const NOT_FOUND_STATUS: u16 = 404;
const NOT_FOUND_CODE: &str = "NotFoundException";

/// A second region's endpoint, for the row that addresses a live cluster from an
/// endpoint that does not hold it.
const OTHER_REGION: &str = "eu-west-1";

/// IAM's sentence for a request nothing but a resource-based policy could have
/// allowed: fixed text around the principal that signed, the action asked for (which
/// it names twice, so the two must agree) and the resource the decision was taken
/// over.
fn no_resource_policy_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no resource-based policy allows the {action} action"
    )
}

/// `UpdateConnectivity` against `target`, as the endpoint of `region` serves it.
async fn update_connectivity_in(
    emulator: &Emulator,
    region: &str,
    target: &str,
    body: &Value,
) -> Reply {
    emulator
        .call_in(
            region,
            "PUT",
            &format!("/v1/clusters/{}/connectivity", pct(target)),
            Some(body.clone()),
        )
        .await
}

/// Assert `reply` is the authorization refusal for `action` on `target`.
fn assert_refused_as_unauthorized(reply: &Reply, case: &str, action: &str, target: &str) {
    let expected = no_resource_policy_message(&caller_principal(), action, target);
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (
            ACCESS_DENIED_STATUS,
            ACCESS_DENIED_CODE,
            expected.as_str()
        ),
        "case {case:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED_CODE} {expected:?}, \
         got {} {}",
        reply.status,
        reply.body
    );
    // The absence the registry would have reported is disclosed nowhere in the
    // refusal: one answer, whatever this endpoint holds under the ARN.
    assert_ne!(
        reply.error_code(),
        NOT_FOUND_CODE,
        "case {case:?}: an unauthorized mutation must disclose nothing about the registry, \
         got {}",
        reply.body
    );
}

/// Every body `UpdateConnectivity` can carry while the decision is taken before any
/// of it is read: each member the operation can change, a version token the cluster
/// would refuse, and a request naming nothing to change at all.
fn connectivity_bodies(version: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "public-access-on",
            json!({
                "currentVersion": version,
                "connectivityInfo": { "publicAccess": { "type": "SERVICE_PROVIDED_EIPS" } },
            }),
        ),
        (
            "public-access-off",
            json!({
                "currentVersion": version,
                "connectivityInfo": { "publicAccess": { "type": "DISABLED" } },
            }),
        ),
        (
            "vpc-connectivity-sasl-iam",
            json!({
                "currentVersion": version,
                "connectivityInfo": {
                    "vpcConnectivity": {
                        "clientAuthentication": { "sasl": { "iam": { "enabled": true } } },
                    },
                },
            }),
        ),
        (
            "network-type",
            json!({
                "currentVersion": version,
                "connectivityInfo": { "networkType": "DUAL" },
            }),
        ),
        (
            "zookeeper-access-only",
            json!({
                "currentVersion": version,
                "zookeeperAccess": { "enabled": true },
            }),
        ),
        (
            "a-version-the-cluster-would-refuse",
            json!({
                "currentVersion": "not-a-version-token",
                "connectivityInfo": { "publicAccess": { "type": "DISABLED" } },
            }),
        ),
        (
            "nothing-to-change",
            json!({ "currentVersion": version }),
        ),
    ]
}

/// RULE: a connectivity mutation handed a well-formed cluster ARN this endpoint holds
/// no cluster for is refused as unauthorized, whatever the body carries and however
/// the endpoint came to hold no cluster under that ARN.
#[tokio::test]
async fn an_absent_cluster_refuses_a_connectivity_mutation_as_unauthorized() {
    let emulator = Emulator::new();
    // A cluster this endpoint really holds, so the version the rows quote is one a
    // request could otherwise be accepted at, and the controls at the end have a
    // cluster to be served for.
    let live = emulator
        .create_ok(&ClusterRequest::named("connectivity-refusal"))
        .await;
    let version = poll_until_active(&emulator, &live, "the created cluster").await;

    // A cluster created and then DELETED: the registry holds no cluster under its
    // ARN any more, which the rule covers exactly as a name never created.
    let deleted = emulator
        .create_ok(&ClusterRequest::named("connectivity-refusal-deleted"))
        .await;
    let deleted_version = poll_until_active(&emulator, &deleted, "the cluster to delete").await;
    let removed = emulator
        .call(
            "DELETE",
            &format!(
                "/v1/clusters/{}?currentVersion={}",
                pct(&deleted),
                deleted_version
            ),
            None,
        )
        .await;
    assert_eq!(
        removed.status, 200,
        "case \"deleted\": expected DeleteCluster to be accepted, got {} {}",
        removed.status, removed.body
    );

    // Every way this endpoint holds no cluster under a well-formed cluster ARN of
    // the caller's own account, as `(case, ARN, the endpoint's region)`.
    let targets: Vec<(&str, String, &str)> = vec![
        (
            "never-created",
            format!(
                "arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/never-created/\
                 11111111-2222-4333-8444-555555555555-1"
            ),
            REGION,
        ),
        (
            "another-name-and-id",
            format!(
                "arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/other-name/\
                 99999999-8888-4777-8666-555555555555-1"
            ),
            REGION,
        ),
        (
            "another-revision-suffix",
            format!(
                "arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/never-created/\
                 11111111-2222-4333-8444-555555555555-7"
            ),
            REGION,
        ),
        ("deleted", deleted.clone(), REGION),
        ("another-region", live.clone(), OTHER_REGION),
    ];

    for (case, target, region) in &targets {
        for (body_case, body) in connectivity_bodies(&version) {
            let reply = update_connectivity_in(&emulator, region, target, &body).await;
            assert_refused_as_unauthorized(
                &reply,
                &format!("{case}/{body_case}"),
                "kafka:UpdateConnectivity",
                target,
            );
        }

        // The OTHER mutation of the same surface takes the same decision, so the
        // rule is the service's rather than one handler's.
        let rejected = emulator
            .reject_client_vpc_connection_in(
                region,
                target,
                &format!("arn:aws:kafka:{REGION}:{ACCOUNT}:vpc-connection/{ACCOUNT}/absent/abc-1"),
            )
            .await;
        assert_refused_as_unauthorized(
            &rejected,
            &format!("{case}/RejectClientVpcConnection"),
            "kafka:RejectClientVpcConnection",
            target,
        );
    }

    // CONTROL (the boundary below the rule): a value that is no cluster ARN at all
    // names no cluster for the decision to be taken over, so it is left to the
    // operation's own parameter refusal.
    for (case, value) in [
        ("not-an-arn", "kafka-cluster".to_string()),
        (
            "another-resource-type",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:configuration/cfg/abc-1"),
        ),
    ] {
        let reply = update_connectivity_in(
            &emulator,
            REGION,
            &value,
            &json!({
                "currentVersion": version,
                "connectivityInfo": { "publicAccess": { "type": "DISABLED" } },
            }),
        )
        .await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (400, "BadRequestException"),
            "case {case:?}: expected the parameter refusal, got {} {}",
            reply.status,
            reply.body
        );
    }

    // CONTROL (the boundary beside the rule): the REPORTING side of the same ARNs
    // still answers the absence as the absence it is.
    for (case, target, region) in &targets {
        let reply = emulator
            .call_in(region, "GET", &format!("/v1/clusters/{}", pct(target)), None)
            .await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (NOT_FOUND_STATUS, NOT_FOUND_CODE),
            "case {case:?}/DescribeCluster: expected the absence to be reported, got {} {}",
            reply.status,
            reply.body
        );
    }

    // CONTROL (the boundary above the rule): the cluster this endpoint DOES hold has
    // its connectivity update accepted at the same version the rows quoted, so every
    // refusal above is about the target.
    let accepted = update_connectivity_in(
        &emulator,
        REGION,
        &live,
        &json!({
            "currentVersion": version,
            "connectivityInfo": { "publicAccess": { "type": "SERVICE_PROVIDED_EIPS" } },
        }),
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "case \"a cluster this endpoint holds\": expected the update to be accepted, got {} {}",
        accepted.status, accepted.body
    );
    let operation = accepted.at("/clusterOperationArn").as_str().unwrap_or("");
    assert!(
        !operation.is_empty(),
        "case \"a cluster this endpoint holds\": an accepted update must answer with the \
         cluster operation it filed, got {}",
        accepted.body
    );

    // And the refusals persisted nothing: the update just accepted is the FIRST one
    // this cluster carries, read back through the cluster's own reader once the clock
    // has made it due.
    let settled = poll_until_active(&emulator, &live, "the accepted update").await;
    assert_ne!(
        settled, version,
        "case \"a cluster this endpoint holds\": an accepted update must move the cluster's \
         version, got {settled:?}"
    );
    let described = emulator.describe(&live).await;
    assert_eq!(
        described.at("/clusterInfo/brokerNodeGroupInfo/connectivityInfo/publicAccess/type"),
        &json!("SERVICE_PROVIDED_EIPS"),
        "case \"a cluster this endpoint holds\": expected the applied connectivity to read \
         back, got {}",
        described.body
    );
}
