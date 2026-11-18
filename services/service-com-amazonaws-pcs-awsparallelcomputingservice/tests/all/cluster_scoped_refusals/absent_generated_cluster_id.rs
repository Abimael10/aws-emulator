// SPDX-License-Identifier: CC0-1.0

//! The refusal a cluster-scoped CHILD operation earns when the `clusterIdentifier` it names
//! is in the GENERATED-ID form and resolves to no cluster.
//!
//! `ClusterIdentifier` admits two forms
//! (`smithy_query(query="shape",shape="ClusterIdentifier")`:
//! `^(pcs_[a-zA-Z0-9]+|[A-Za-z][A-Za-z0-9-]{2,40})$`), and every cluster-scoped child
//! operation models `ResourceNotFoundException` and `AccessDeniedException` side by side. The
//! rule these tests pin: an identifier carrying the generated-id prefix is a value only this
//! service mints, so one nothing holds names a resource that is not there and the call is
//! refused the modelled not-found — the service's fixed cluster sentence, the identifier as
//! supplied in `resourceId`, the identifier kind in `resourceType` — while a cluster NAME,
//! which says nothing about whose cluster it is, keeps the permissions refusal. The sibling
//! suite states the rule over both forms; these tests add what a client depends on besides
//! the class:
//!
//! * the not-found holds for the WIDTHS and the character class the generated-id alternative
//!   admits, not just for the width this service happens to mint;
//! * a refused child create persists nothing — no cluster appears under the identifier it
//!   named, and no queue or compute node group is stored anywhere — while the same credential
//!   goes on being served, so the refusal is about the identifier and not about who asked;
//! * each request is refused by the form of ITS OWN identifier, so one form's refusal never
//!   decides the next call's.
//!
//! Driven over the awsJson1_0 wire through the router the emulator serves from, on the
//! harness [`crate::cluster_lifecycle`] builds.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;

use super::{
    cluster_scoped_calls, create_cluster, create_compute_node_group, error_code, not_authorized,
};
use crate::cluster_lifecycle::{call, emulator};

/// The sentence PCS states a cluster it cannot find with: fixed text throughout, echoing no
/// value the caller sent.
const CLUSTER_NOT_FOUND: &str = "AWS PCS couldn't find the cluster you specified.";

/// What a cluster not-found reports in `resourceType`: the kind of IDENTIFIER that did not
/// resolve, as the service names it.
const CLUSTER_IDENTIFIER_KIND: &str = "Cluster ID";

/// Assert `refused` is the modelled cluster not-found naming `identifier`, with none of the
/// members a refusal of the caller carries.
fn assert_cluster_not_found(case: &str, status: u16, refused: &Value, identifier: &str) {
    assert_eq!(
        (status, error_code(refused).as_str()),
        (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
        "case {case}: expected the cluster not-found, got {status} with {refused}"
    );
    assert_eq!(
        (
            refused["message"].as_str(),
            refused["resourceId"].as_str(),
            refused["resourceType"].as_str()
        ),
        (
            Some(CLUSTER_NOT_FOUND),
            Some(identifier),
            Some(CLUSTER_IDENTIFIER_KIND)
        ),
        "case {case}: expected the fixed cluster sentence with the identifier as supplied \
         and its kind, got {refused}"
    );
    assert!(
        refused.get("Message").is_none(),
        "case {case}: a not-found carries no IAM sentence, got {refused}"
    );
}

/// RULE: a cluster-scoped child operation whose `clusterIdentifier` carries the generated-id
/// prefix and resolves to no cluster is refused the modelled `ResourceNotFoundException` for
/// the CLUSTER, whatever width or characters the id has.
#[tokio::test]
async fn every_child_operation_reports_the_cluster_not_found_for_an_absent_generated_id() {
    let router = emulator();

    // Rows span the generated-id alternative of the pattern
    // (`smithy_query(query="shape",shape="ClusterIdentifier")`: `pcs_[a-zA-Z0-9]+`, which
    // bounds neither the width nor the case): one character past the prefix, the sixteen
    // hex digits this service mints, a mixed-case alphanumeric id, a digits-only id wider
    // than minted, and one far wider still. None of them names a cluster this emulator
    // holds.
    let minted_width = format!("pcs_{}", "0123456789abcdef");
    let digits_wider_than_minted = format!("pcs_{}", "7".repeat(20));
    let far_wider = format!("pcs_{}", "a".repeat(120));
    let rows: [(&str, &str); 5] = [
        ("one character past the prefix", "pcs_1"),
        ("the width this service mints", minted_width.as_str()),
        ("letters and digits mixed", "pcs_aB3zZ9"),
        (
            "digits only, wider than minted",
            digits_wider_than_minted.as_str(),
        ),
        ("far wider than minted", far_wider.as_str()),
    ];

    for (shape, identifier) in rows {
        for (operation, request) in cluster_scoped_calls(identifier) {
            let case = format!("{operation} naming {shape} ({identifier})");
            let (status, refused) = call(&router, operation, &request).await;
            assert_cluster_not_found(&case, status, &refused, identifier);
        }
    }
}

/// RULE: a child create refused because its cluster identifier resolves to nothing persists
/// nothing — neither the cluster it named nor the child it asked for — and the credential it
/// presented goes on being served, so the refusal is scoped to the identifier alone.
#[tokio::test]
async fn a_child_create_refused_on_an_absent_cluster_id_persists_nothing() {
    let router = emulator();
    let absent = format!("pcs_{}", "3".repeat(18));

    // Both kinds of child a cluster holds, created against an identifier nothing resolves.
    for (operation, request) in [
        (
            "CreateQueue",
            json!({"clusterIdentifier": &absent, "queueName": "work"}),
        ),
        (
            "CreateComputeNodeGroup",
            create_compute_node_group(&absent, "nodes"),
        ),
    ] {
        let (status, refused) = call(&router, operation, &request).await;
        assert_cluster_not_found(operation, status, &refused, &absent);
    }

    // The same credential is still served: the refusals above were about the identifier.
    let (status, listed) = call(&router, "ListClusters", &json!({})).await;
    assert_eq!(
        (status, listed["clusters"].as_array().map(Vec::len)),
        (200, Some(0)),
        "case ListClusters after the refused creates: expected the account's own listing \
         served and empty, got {status} with {listed}"
    );

    // Nothing was minted under the identifier either: it still resolves to no cluster.
    let (status, refused) = call(
        &router,
        "GetCluster",
        &json!({"clusterIdentifier": &absent}),
    )
    .await;
    assert_cluster_not_found("GetCluster after the refused creates", status, &refused, &absent);

    // And no child was stored anywhere: a cluster created afterwards holds neither of the
    // children the refused creates named.
    let (status, created) = call(&router, "CreateCluster", &create_cluster("held-cluster")).await;
    assert_eq!(status, 200, "setup: CreateCluster: got {created}");
    let cluster_id = created["cluster"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    for (operation, member) in [
        ("ListQueues", "queues"),
        ("ListComputeNodeGroups", "computeNodeGroups"),
    ] {
        let (status, listed) = call(
            &router,
            operation,
            &json!({"clusterIdentifier": &cluster_id}),
        )
        .await;
        assert_eq!(
            (status, listed[member].as_array().map(Vec::len)),
            (200, Some(0)),
            "case {operation} on the cluster created after the refusals: expected no child \
             persisted, got {status} with {listed}"
        );
    }
}

/// RULE, stated per request: what an unresolvable `clusterIdentifier` is refused with follows
/// the form of THAT identifier — so the two forms can be interleaved on one operation and
/// each call answers for its own.
#[tokio::test]
async fn each_request_is_refused_by_the_form_of_its_own_cluster_identifier() {
    let router = emulator();
    let absent_name = "no-such-cluster";
    let absent_id = "pcs_5aF";

    for ((operation, by_name), (_, by_id)) in cluster_scoped_calls(absent_name)
        .into_iter()
        .zip(cluster_scoped_calls(absent_id))
    {
        // The name form, then the id form, then the name form again: the third call shows
        // the id form's not-found did not change what a name earns.
        for (step, request, by_form) in [
            ("the name form", &by_name, "AccessDeniedException"),
            ("the id form", &by_id, "ResourceNotFoundException"),
            ("the name form again", &by_name, "AccessDeniedException"),
        ] {
            let case = format!("{operation} with {step}");
            let (status, refused) = call(&router, operation, request).await;
            if by_form == "ResourceNotFoundException" {
                assert_cluster_not_found(&case, status, &refused, absent_id);
                continue;
            }
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (CLIENT_ERROR_STATUS, by_form),
                "case {case}: expected the permissions failure, got {status} with {refused}"
            );
            assert_eq!(
                refused["Message"].as_str(),
                Some(not_authorized(operation, absent_name).as_str()),
                "case {case}: expected the IAM sentence in the Message member, got {refused}"
            );
        }
    }
}
