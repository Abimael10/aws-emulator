// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a mutation of a cluster's CLIENT-CONNECTIVITY surface takes for a
//! cluster ARN this endpoint holds no cluster for, driven end to end over the same
//! `MultiServiceRouter` the emulator serves from (the shared `kafka_harness`).
//!
//! RULE under test: a cluster ARN of the caller's own account that this endpoint's
//! Region holds no cluster under is answered as the ABSENCE it is, for a call that
//! CHANGES how clients reach the cluster exactly as for every other cluster-addressed
//! call of this service — `UpdateConnectivity` with the `NotFoundException` its model
//! declares (HTTP 404) carrying this service's one fixed sentence for a resource it
//! does not hold, which is the recorded answer, and `RejectClientVpcConnection`,
//! whose model declares no `NotFoundException`, with its own fixed parameter refusal
//! naming `clusterArn`. Whose ACCOUNT the ARN names is a different question, decided
//! before this one.
//!
//! The rows differ along what the rule generalises over: the OPERATION taking the
//! decision (both mutations of that surface), the way this endpoint comes to hold no
//! cluster under the submitted ARN (a name never created, an ARN of the shape this
//! service mints with another name and id, a cluster id with a different revision
//! suffix, a cluster that was created and then deleted, and a live cluster's own ARN
//! seen from another region's endpoint), and — for `UpdateConnectivity` — the BODY the
//! request carries (each connectivity member the operation can change, a version
//! token the cluster would refuse, and a body naming nothing to change at all), since
//! the target is resolved before any of the body is applied.
//!
//! The boundaries are pinned too: a value that is no cluster ARN at all is left to the
//! operation's own parameter refusal (HTTP 400), the REPORTING side of the same ARN
//! answers the same absence, and the cluster this endpoint DOES hold has its
//! connectivity update accepted — so the rows above are refusals about the target
//! rather than about the caller or the request.

use serde_json::{json, Value};

use crate::kafka_harness::{
    pct, poll_until_active, ClusterRequest, Emulator, Reply, ACCOUNT, REGION,
};

/// The status and wire code the service reports a resource it does not hold with,
/// spelled out here rather than read from the service so a drift in its own constants
/// fails this suite.
const NOT_FOUND_STATUS: u16 = 404;
const NOT_FOUND_CODE: &str = "NotFoundException";

/// The whole sentence that refusal carries: fixed text, naming nothing of the
/// request.
const NOT_FOUND_MESSAGE: &str = "The requested resource doesn\u{2019}t exist.";

/// The status, wire code and sentence of the parameter refusal
/// `RejectClientVpcConnection` answers an unresolvable reference with, which is what
/// it reports for an absent cluster as well, since its model declares no
/// `NotFoundException`.
const BAD_REQUEST_STATUS: u16 = 400;
const BAD_REQUEST_CODE: &str = "BadRequestException";
const BAD_REQUEST_MESSAGE: &str = "One or more of the parameters are not valid.";

/// The member each refusal names as the one at fault: the one that carried the ARN.
const CLUSTER_ARN_MEMBER: &str = "clusterArn";

/// A second region's endpoint, for the row that addresses a live cluster from an
/// endpoint that does not hold it.
const OTHER_REGION: &str = "eu-west-1";

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

/// Assert `reply` reports the addressed cluster as absent, naming the member that
/// carried the ARN and carrying nothing of the request in its sentence.
fn assert_reported_absent(reply: &Reply, case: &str) {
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (NOT_FOUND_STATUS, NOT_FOUND_CODE, NOT_FOUND_MESSAGE),
        "case {case:?}: expected {NOT_FOUND_STATUS} {NOT_FOUND_CODE} {NOT_FOUND_MESSAGE:?}, \
         got {} {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.at("/invalidParameter").as_str(),
        Some(CLUSTER_ARN_MEMBER),
        "case {case:?}: expected {CLUSTER_ARN_MEMBER} as the member at fault, got {}",
        reply.body
    );
}

/// Assert `reply` is the fixed parameter refusal `RejectClientVpcConnection` answers
/// a reference it cannot resolve with, naming the member that carried the ARN.
fn assert_refused_as_unresolvable(reply: &Reply, case: &str) {
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (BAD_REQUEST_STATUS, BAD_REQUEST_CODE, BAD_REQUEST_MESSAGE),
        "case {case:?}: expected {BAD_REQUEST_STATUS} {BAD_REQUEST_CODE} \
         {BAD_REQUEST_MESSAGE:?}, got {} {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.at("/invalidParameter").as_str(),
        Some(CLUSTER_ARN_MEMBER),
        "case {case:?}: expected {CLUSTER_ARN_MEMBER} as the member at fault, got {}",
        reply.body
    );
}

/// Every body `UpdateConnectivity` can carry while the TARGET is resolved before any
/// of it is applied: each member the operation can change, and a version token the
/// cluster would refuse. A body that names nothing to change at all is left out: that
/// is a defect of the REQUEST, which this operation answers before it resolves
/// anything (the control at the end of the suite).
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
    ]
}

/// RULE: a connectivity mutation handed a well-formed cluster ARN this endpoint holds
/// no cluster for answers the absence, whatever the body carries and however the
/// endpoint came to hold no cluster under that ARN — in each operation's own modelled
/// vocabulary.
#[tokio::test]
async fn an_absent_cluster_is_reported_absent_by_a_connectivity_mutation() {
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
            assert_reported_absent(&reply, &format!("{case}/{body_case}"));
        }

        // The OTHER mutation of the same surface answers the same absence in the
        // vocabulary ITS model gives it, so the rule is the service's rather than
        // one handler's.
        let rejected = emulator
            .reject_client_vpc_connection_in(
                region,
                target,
                &format!("arn:aws:kafka:{REGION}:{ACCOUNT}:vpc-connection/{ACCOUNT}/absent/abc-1"),
            )
            .await;
        assert_refused_as_unresolvable(&rejected, &format!("{case}/RejectClientVpcConnection"));
    }

    // CONTROL (the boundary below the rule): a value that is no cluster ARN at all
    // names no cluster to resolve, so it is left to the operation's own parameter
    // refusal — and so is a body that names nothing to change, which is a defect of
    // the request and is answered before the target is resolved at all.
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
            (BAD_REQUEST_STATUS, BAD_REQUEST_CODE),
            "case {case:?}: expected the parameter refusal, got {} {}",
            reply.status,
            reply.body
        );
    }
    for (case, target, region) in &targets {
        let reply = update_connectivity_in(
            &emulator,
            region,
            target,
            &json!({ "currentVersion": version }),
        )
        .await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (BAD_REQUEST_STATUS, BAD_REQUEST_CODE),
            "case {case:?}/nothing-to-change: expected the request's own refusal, got {} {}",
            reply.status,
            reply.body
        );
    }

    // CONTROL (the boundary beside the rule): the REPORTING side of the same ARNs
    // answers the same absence.
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
