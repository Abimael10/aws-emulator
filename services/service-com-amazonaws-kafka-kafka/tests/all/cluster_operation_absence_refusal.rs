// SPDX-License-Identifier: CC0-1.0

//! What the two cluster-operation READERS answer for a well-formed
//! cluster-operation ARN this endpoint holds no operation for.
//!
//! RULE: a read whose target is one cluster OPERATION is answered out of the
//! operation registry. The record of work done on a cluster is not a resource a
//! policy is attached to, so no account holds or withholds a document over it and
//! the ARN's account segment decides nothing: whichever account, region or cluster
//! a well-formed cluster-operation ARN names, an operation this endpoint does not
//! hold is reported ABSENT — HTTP 404 `NotFoundException`, the service's single
//! not-found sentence as the whole message, and the cluster member in
//! `invalidParameter` — never as an authorization failure, which would tell a
//! client polling for an operation to go and fix its permissions. The caller's own
//! identity-based decision is untouched, and an operation this endpoint DOES hold
//! is still served.
//!
//! Asserted over a TABLE whose rows differ along what the rule generalises over:
//! the account segment (the caller's own, another account, an empty one), the
//! region the ARN names against the endpoint it is read at, the cluster name and
//! id the operation is scoped to, and the operation id itself — across both
//! readers (`DescribeClusterOperation` and `DescribeClusterOperationV2`).
//!
//! Driven end to end over the same `MultiServiceRouter` the emulator serves from
//! (the shared harness in `tests/all/kafka_harness.rs`).

use serde_json::json;

use crate::kafka_harness::{
    pct, poll_until_active, ClusterRequest, Emulator, Reply, ACCOUNT, REGION,
};

/// The service's one not-found sentence (the apostrophe is U+2019, as the service
/// writes it), written out here rather than imported so this suite pins the
/// wording a client sees.
const RESOURCE_NOT_FOUND_MESSAGE: &str = "The requested resource doesn\u{2019}t exist.";

/// The member a cluster-operation reader blames when it holds no such operation:
/// the cluster the operation ARN is scoped to, in the service's own lowerCamel
/// wire vocabulary.
const BLAMED_MEMBER: &str = "clusterArn";

/// An account that is not the one this emulator serves, so nothing it holds could
/// grant a call over a resource of it.
const ANOTHER_ACCOUNT: &str = "123456789012";

/// A cluster id segment of the shape this service mints, and an operation id of
/// the shape it mints — neither ever issued here.
const UNISSUED_CLUSTER_ID: &str = "11111111-2222-4333-8444-555555555555-1";
const UNISSUED_OPERATION_ID: &str = "99999999-8888-4777-8666-555555555555";

/// The two readers of one cluster operation, by the route the model binds each to:
/// the V1 reader and the V2 one, which resolve the same registry record.
const READERS: [(&str, &str); 2] = [
    ("DescribeClusterOperation", "/v1/operations/"),
    ("DescribeClusterOperationV2", "/api/v2/operations/"),
];

/// Read one cluster operation through `prefix` at the `region` endpoint.
async fn describe_at(emulator: &Emulator, region: &str, prefix: &str, arn: &str) -> Reply {
    emulator
        .call_in(region, "GET", &format!("{prefix}{}", pct(arn)), None)
        .await
}

/// A well-formed MSK cluster-operation ARN, built from its segments.
fn operation_arn(region: &str, account: &str, cluster: &str, cluster_id: &str, id: &str) -> String {
    format!("arn:aws:kafka:{region}:{account}:cluster-operation/{cluster}/{cluster_id}/{id}")
}

/// Assert `reply` is the absence a cluster-operation reader reports: the modelled
/// `NotFoundException`, the service's fixed sentence as the WHOLE message, the
/// cluster member at fault, and nothing of the submitted value — nor any
/// authorization verdict — in the text.
fn assert_operation_absent(reply: &Reply, case: &str, submitted: &str) {
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (404, "NotFoundException", RESOURCE_NOT_FOUND_MESSAGE),
        "case {case}: expected the not-found refusal, got {} {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.at("/invalidParameter").as_str(),
        Some(BLAMED_MEMBER),
        "case {case}: expected the refusal to name {BLAMED_MEMBER}, got {}",
        reply.body
    );
    assert!(
        !reply.error_message().contains(submitted),
        "case {case}: the refusal must not echo the submitted value, got {:?}",
        reply.error_message()
    );
    assert!(
        !reply.error_message().contains("not authorized"),
        "case {case}: an operation this endpoint does not hold is absent, not unauthorized, \
         got {:?}",
        reply.error_message()
    );
}

/// RULE: every well-formed cluster-operation ARN this endpoint holds no operation
/// for is reported absent by both readers, whatever account, cluster or operation
/// id it names.
#[tokio::test]
async fn an_unheld_cluster_operation_arn_is_absent_whichever_account_it_names() {
    let emulator = Emulator::new();
    // A cluster this endpoint really holds, so the rows below are answered by the
    // operation lookup rather than by a service refusing everything.
    let live = emulator
        .create_ok(&ClusterRequest::named("operation-absence"))
        .await;
    poll_until_active(&emulator, &live, "the created cluster").await;

    let rows: Vec<(&str, String)> = vec![
        (
            "this account, never minted",
            operation_arn(
                REGION,
                ACCOUNT,
                "a-cluster",
                UNISSUED_CLUSTER_ID,
                UNISSUED_OPERATION_ID,
            ),
        ),
        (
            "another account",
            operation_arn(
                REGION,
                ANOTHER_ACCOUNT,
                "a-cluster",
                UNISSUED_CLUSTER_ID,
                UNISSUED_OPERATION_ID,
            ),
        ),
        (
            "another account, a cluster name this endpoint holds",
            operation_arn(
                REGION,
                ANOTHER_ACCOUNT,
                "operation-absence",
                UNISSUED_CLUSTER_ID,
                UNISSUED_OPERATION_ID,
            ),
        ),
        (
            "another account, another region",
            operation_arn(
                "eu-west-1",
                ANOTHER_ACCOUNT,
                "far-away",
                UNISSUED_CLUSTER_ID,
                UNISSUED_OPERATION_ID,
            ),
        ),
        (
            "another region of this account",
            operation_arn(
                "eu-west-1",
                ACCOUNT,
                "elsewhere",
                UNISSUED_CLUSTER_ID,
                UNISSUED_OPERATION_ID,
            ),
        ),
        (
            "another account, an operation id of another shape",
            operation_arn(
                REGION,
                ANOTHER_ACCOUNT,
                "a-cluster",
                UNISSUED_CLUSTER_ID,
                "ffffffff-1111-2222-3333-444444444444",
            ),
        ),
    ];

    for (row, arn) in &rows {
        for (reader, prefix) in READERS {
            let case = format!("{row} / {reader}");
            let reply = describe_at(&emulator, REGION, prefix, arn).await;
            assert_operation_absent(&reply, &case, arn);
        }
    }

    // The control: the refusals changed nothing about the cluster this endpoint
    // holds.
    let described = emulator.describe(&live).await;
    assert_eq!(
        (
            described.status,
            described.at("/clusterInfo/clusterArn").as_str()
        ),
        (200, Some(live.as_str())),
        "case control: the held cluster must still be served, got {} {}",
        described.status,
        described.body
    );
}

/// RULE: the same reading applies to an operation this endpoint really filed — it
/// is served where it was filed, and reported absent at any other endpoint or
/// under any other account, since what the answer turns on is whether the registry
/// holds the record.
#[tokio::test]
async fn an_operation_this_endpoint_filed_is_served_only_where_it_was_filed() {
    let emulator = Emulator::new();
    let arn = emulator
        .create_ok(&ClusterRequest::named("operation-held"))
        .await;
    let version = poll_until_active(&emulator, &arn, "the created cluster").await;
    let update = emulator
        .call(
            "PUT",
            &format!("/v1/clusters/{}/storage", pct(&arn)),
            Some(json!({ "currentVersion": version, "volumeSizeGB": 400 })),
        )
        .await;
    assert_eq!(
        update.status, 200,
        "the update that files the operation must be accepted, got {} {}",
        update.status, update.body
    );
    let filed = update
        .at("/clusterOperationArn")
        .as_str()
        .unwrap_or_else(|| panic!("the accepted update filed no operation, got {}", update.body))
        .to_string();

    for (reader, prefix) in READERS {
        // Where it was filed: served.
        let reply = describe_at(&emulator, REGION, prefix, &filed).await;
        assert_eq!(
            reply.status, 200,
            "case {reader}: the operation this endpoint filed must be served, got {} {}",
            reply.status, reply.body
        );
        assert!(
            reply.at("/clusterOperationInfo").is_object(),
            "case {reader}: expected the operation record, got {}",
            reply.body
        );
        // The same ARN under another account, and the real one read at another
        // region's endpoint: neither names a record this endpoint holds.
        let elsewhere = filed.replace(ACCOUNT, ANOTHER_ACCOUNT);
        assert_ne!(
            elsewhere, filed,
            "case {reader}: this row needs an ARN of another account to probe with"
        );
        let reply = describe_at(&emulator, REGION, prefix, &elsewhere).await;
        assert_operation_absent(&reply, &format!("another account / {reader}"), &elsewhere);
        let reply = describe_at(&emulator, "eu-west-1", prefix, &filed).await;
        assert_operation_absent(&reply, &format!("another endpoint / {reader}"), &filed);
    }
}
