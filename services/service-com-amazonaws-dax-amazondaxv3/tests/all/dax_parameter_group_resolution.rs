// SPDX-License-Identifier: CC0-1.0

//! The resources a cluster NAMES, resolved against the account's own records.
//!
//! `CreateCluster`/`UpdateCluster` name a parameter group of this account through
//! `ParameterGroupName` — the default group DAX provides, or one `CreateParameterGroup`
//! minted — and a stored subnet group through `SubnetGroupName`, so a request naming one
//! the store does not hold is answered with that resource's own not-found fault and
//! stores nothing, and a cluster is never left carrying a name that resolves to nothing.
//! Both are resolved BEFORE the `ClusterName` format rule is applied, so a request that
//! would fail both reports the resource that does not exist.
//!
//! Every case drives `POST /` with `X-Amz-Target: AmazonDAXV3.<Operation>` through the
//! same `MultiServiceRouter` the production router builds, so what is asserted is the
//! HTTP behaviour a DAX SDK client sees. The tables below span the CLASS each rule
//! covers; the one request the issue reported is a single row.

use serde_json::{json, Value};
use service_com_amazonaws_dax_amazondaxv3::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::dax_account_network::{
    an_account_subnet, call, cluster_in_status, emulator, error_code, error_message,
};

/// A role ARN to pass as the required `IamRoleArn`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/DAXRole";

/// A node type to pass as the required `NodeType`.
const NODE_TYPE: &str = "dax.r4.large";

/// The status a client fault answers with on this service's JSON protocol.
const CLIENT_FAULT_STATUS: u16 = 400;

/// A `CreateCluster` body with the required members filled in.
fn create_cluster_body(name: &str) -> Value {
    json!({
        "ClusterName": name,
        "NodeType": NODE_TYPE,
        "ReplicationFactor": 1,
        "IamRoleArn": ROLE_ARN,
    })
}

/// Create the parameter group `name` and assert it was stored.
async fn create_group(router: &MultiServiceRouter, name: &str) {
    let (status, body) = call(
        router,
        "CreateParameterGroup",
        json!({ "ParameterGroupName": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name:?}: CreateParameterGroup got {status} {body:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule: an operation naming a parameter group the account does not hold is answered with
// `ParameterGroupNotFoundFault` — HTTP 400 on this protocol — carrying the fixed text
// "ParameterGroup not found: " followed by the name the REQUEST carried, and stores
// nothing.
// Source: `CreateCluster$ParameterGroupName` ("The parameter group to be associated with
// the DAX cluster"), the modelled `ParameterGroupNotFoundFault` ("The specified parameter
// group does not exist") and the recorded DAX answer for a create naming an unknown
// group.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cluster_naming_a_parameter_group_the_account_does_not_hold_is_refused() {
    let router = emulator();
    create_group(&router, "held-group").await;

    // A group the account HAD and deleted: the same name, no longer held.
    create_group(&router, "deleted-group").await;
    let (status, body) = call(
        &router,
        "DeleteParameterGroup",
        json!({ "ParameterGroupName": "deleted-group" }),
    )
    .await;
    assert_eq!(status, 200, "DeleteParameterGroup got {status} {body:?}");

    // Rows differ along what the rule generalises over: the spelling of the name, its
    // case, its length, a name that differs from a held one only in case, the default
    // group's family without the `default.` prefix it carries, and a name that was held
    // and is not any more.
    let missing = [
        "no-such-group",
        "NoSuchGroup",
        "HELD-GROUP",
        "a",
        "sim2real-dax-parameter-group-0123456789",
        state::DEFAULT_PARAMETER_GROUP_FAMILY,
        "deleted-group",
    ];

    for (index, name) in missing.iter().enumerate() {
        let cluster_name = format!("pg-probe-{index}");
        let mut body = create_cluster_body(&cluster_name);
        body["ParameterGroupName"] = json!(name);
        let (status, answered) = call(&router, "CreateCluster", body).await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {name:?}: a group the account does not hold is refused, \
             got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "ParameterGroupNotFoundFault",
            "case {name:?}: expected ParameterGroupNotFoundFault, got {answered:?}"
        );
        assert_eq!(
            error_message(&answered),
            format!("ParameterGroup not found: {name}"),
            "case {name:?}: the fault must name the group the request carried, \
             got {answered:?}"
        );
        // Nothing was stored: the cluster does not exist.
        let (status, read) = call(
            &router,
            "DescribeClusters",
            json!({ "ClusterNames": [cluster_name.clone()] }),
        )
        .await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {name:?}: a refused create leaves nothing behind, got {status} {read:?}"
        );
        assert_eq!(
            error_code(&read),
            "ClusterNotFoundFault",
            "case {name:?}: expected ClusterNotFoundFault, got {read:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: a resource the request NAMES is resolved before the `ClusterName` format rule is
// applied, so a request that would fail both reports the resource that does not exist; a
// request whose named resources do resolve is still refused for its `ClusterName`.
// Source: the recorded DAX answers for a create carrying both a name outside
// `CreateCluster$ClusterName`'s constraints and a `ParameterGroupName` / `SubnetGroupName`
// the account does not hold.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_named_resources_are_resolved_before_the_cluster_name_format_rule() {
    let router = emulator();
    create_group(&router, "resolvable").await;
    let subnet = an_account_subnet(&router).await;
    let (status, body) = call(
        &router,
        "CreateSubnetGroup",
        json!({ "SubnetGroupName": "resolvable-sg", "SubnetIds": [subnet] }),
    )
    .await;
    assert_eq!(status, 200, "CreateSubnetGroup got {status} {body:?}");

    // Rows: each way `ClusterName` can break the documented SHAPE rule — "The first
    // character must be a letter. A name cannot end with a hyphen or contain two
    // consecutive hyphens", and the character set it names. Its LENGTH is not one of
    // those ways: `ClusterName` targets the unconstrained `String` shape, so a long name
    // is valid and would be CREATED here rather than refused.
    let bad_names = [
        "1leading-digit",
        "trailing-",
        "double--hyphen",
        "has_underscore",
        "",
    ];

    for name in bad_names {
        // With an unknown parameter group, the parameter-group fault wins.
        let mut body = create_cluster_body(name);
        body["ParameterGroupName"] = json!("absent-group");
        let (status, answered) = call(&router, "CreateCluster", body).await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {name:?}: expected a refusal, got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "ParameterGroupNotFoundFault",
            "case {name:?}: the parameter group is resolved first, got {answered:?}"
        );
        assert_eq!(
            error_message(&answered),
            "ParameterGroup not found: absent-group",
            "case {name:?}: the fault must name the group the request carried, \
             got {answered:?}"
        );

        // With an unknown subnet group, that group's own fault wins.
        let mut body = create_cluster_body(name);
        body["SubnetGroupName"] = json!("absent-sg");
        let (status, answered) = call(&router, "CreateCluster", body).await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {name:?} with an absent subnet group: expected a refusal, \
             got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "SubnetGroupNotFoundFault",
            "case {name:?}: the subnet group is resolved first, got {answered:?}"
        );
        assert_eq!(
            error_message(&answered),
            "Subnet Group not found.",
            "case {name:?}: a referenced group's template names no identifier, \
             got {answered:?}"
        );

        // With resources that resolve, the `ClusterName` rule is what refuses it.
        let mut body = create_cluster_body(name);
        body["ParameterGroupName"] = json!("resolvable");
        body["SubnetGroupName"] = json!("resolvable-sg");
        let (status, answered) = call(&router, "CreateCluster", body).await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {name:?} with resolvable references: expected a refusal, \
             got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "InvalidParameterValueException",
            "case {name:?} with resolvable references: the ClusterName rule still applies, \
             got {answered:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: a cluster may name any parameter group the account holds — the default group DAX
// provides, or one `CreateParameterGroup` minted — and the stored cluster reports it
// through `Cluster$ParameterGroup`.
// Source: `DEFAULT_PARAMETER_GROUP` (the group every DAX account already has) and
// `ParameterGroupStatus$ParameterGroupName`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cluster_may_name_any_parameter_group_the_account_holds() {
    let router = emulator();
    create_group(&router, "minted-group").await;
    create_group(&router, "MintedCase").await;

    // Rows: the group DAX provides itself, and two a caller created (one of them
    // mixed-case, because a parameter group name is stored as it was given).
    let held = [
        state::DEFAULT_PARAMETER_GROUP,
        "minted-group",
        "MintedCase",
    ];

    for (index, group) in held.iter().enumerate() {
        let cluster_name = format!("pg-cluster-{index}");
        let mut body = create_cluster_body(&cluster_name);
        body["ParameterGroupName"] = json!(group);
        let (status, created) = call(&router, "CreateCluster", body).await;
        assert_eq!(
            status, 200,
            "case {group:?}: a held parameter group is accepted, got {status} {created:?}"
        );
        assert_eq!(
            created["Cluster"]["ParameterGroup"]["ParameterGroupName"],
            json!(group),
            "case {group:?}: the create must report the group it was given, got {created:?}"
        );
        // Round trip: the stored cluster reports the same group.
        let read = cluster_in_status(&router, &cluster_name, state::CLUSTER_AVAILABLE).await;
        assert_eq!(
            read["ParameterGroup"]["ParameterGroupName"],
            json!(group),
            "case {group:?}: the stored cluster must report its group, got {read:?}"
        );
        assert_eq!(
            read["ParameterGroup"]["ParameterApplyStatus"],
            json!(state::PARAMETER_GROUP_IN_SYNC),
            "case {group:?}: an applied group is in sync, got {read:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: `UpdateCluster` resolves `ParameterGroupName` the same way — a group the account
// does not hold is refused with `ParameterGroupNotFoundFault` and the cluster keeps the
// group it had; one the account holds is applied and read back.
// Source: `UpdateCluster$ParameterGroupName` ("The name of a parameter group for this
// cluster") and the same modelled fault.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn updating_a_cluster_to_a_parameter_group_that_does_not_hold_is_refused() {
    let router = emulator();
    create_group(&router, "second-group").await;

    let mut body = create_cluster_body("pg-update");
    body["ParameterGroupName"] = json!(state::DEFAULT_PARAMETER_GROUP);
    let (status, created) = call(&router, "CreateCluster", body).await;
    assert_eq!(status, 200, "CreateCluster got {status} {created:?}");
    cluster_in_status(&router, "pg-update", state::CLUSTER_AVAILABLE).await;

    // Rows: a name nothing created, and one differing from a held group only in case.
    for name in ["no-such-group", "SECOND-GROUP"] {
        let (status, answered) = call(
            &router,
            "UpdateCluster",
            json!({ "ClusterName": "pg-update", "ParameterGroupName": name }),
        )
        .await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {name:?}: expected a refusal, got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "ParameterGroupNotFoundFault",
            "case {name:?}: expected ParameterGroupNotFoundFault, got {answered:?}"
        );
        assert_eq!(
            error_message(&answered),
            format!("ParameterGroup not found: {name}"),
            "case {name:?}: the fault must name the group the request carried, \
             got {answered:?}"
        );
        // The refused update changed nothing: the cluster keeps the group it had and is
        // still available rather than modifying.
        let read = cluster_in_status(&router, "pg-update", state::CLUSTER_AVAILABLE).await;
        assert_eq!(
            read["ParameterGroup"]["ParameterGroupName"],
            json!(state::DEFAULT_PARAMETER_GROUP),
            "case {name:?}: a refused update must keep the stored group, got {read:?}"
        );
    }

    // A group the account holds IS applied, and reads back.
    let (status, updated) = call(
        &router,
        "UpdateCluster",
        json!({ "ClusterName": "pg-update", "ParameterGroupName": "second-group" }),
    )
    .await;
    assert_eq!(status, 200, "UpdateCluster got {status} {updated:?}");
    let read = cluster_in_status(&router, "pg-update", state::CLUSTER_AVAILABLE).await;
    assert_eq!(
        read["ParameterGroup"]["ParameterGroupName"],
        json!("second-group"),
        "the applied group must read back out of the store, got {read:?}"
    );
}
